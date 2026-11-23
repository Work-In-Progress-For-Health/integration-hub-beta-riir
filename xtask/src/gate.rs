//! `cargo xtask gate`: the one definition of the gate.
//!
//! Implements `spec/21-local-ci.md` §1 and §2. Git hooks, a developer at a
//! terminal and the hosted workflow all call this; none of them lists the
//! steps (§1.1). The step list is data ([`steps`]) so a test can hold the
//! workflow to it (§9.1).
//!
//! ```text
//! cargo xtask gate                       # tier 1, stop at the first failure
//! cargo xtask gate --tier fast           # tier 0: formatting only
//! cargo xtask gate --keep-going          # report every failing step
//! cargo xtask gate --tier stack          # tier 2: needs the bus stack running
//! cargo xtask gate --tier flows          # tier 3: Compose profiles, one at a time
//! cargo xtask gate --tier msrv           # only the MSRV build, always
//! ```
//!
//! `RUSTFLAGS=-D warnings` is deliberately **not** set here: changing it
//! rebuilds the whole workspace, which a developer would pay on every run.
//! The hosted workflow sets it in its environment (`spec/17` §1).

mod record;

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

/// The files whose change can move the real MSRV (`spec/21` §2.1, tier 1).
const TOOLCHAIN_INPUTS: &[&str] = &["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"];

/// A tier (`spec/21` §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// Tier 0: `fmt-check`.
    Fast,
    /// Tier 1: the gate proper. Needs only a toolchain.
    Gate,
    /// Tier 2: the `emulator` and `sqlserver` feature tests.
    Stack,
    /// Tier 3: each end-to-end flow, one Compose profile at a time.
    Flows,
    /// Only the MSRV build, whether or not the inputs changed. For the
    /// hosted `msrv` job, which has installed the toolchain itself.
    Msrv,
}

impl Tier {
    /// Parse `--tier`'s value.
    fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "fast" => Self::Fast,
            "gate" => Self::Gate,
            "stack" => Self::Stack,
            "flows" => Self::Flows,
            "msrv" => Self::Msrv,
            other => bail!("unknown tier {other:?}; one of fast, gate, stack, flows, msrv"),
        })
    }
}

/// One step: a name for the log and the command that is it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    /// What the log calls it.
    pub(crate) name: &'static str,
    /// The program and its arguments.
    pub(crate) command: Vec<String>,
    /// Extra environment for this step only.
    pub(crate) env: Vec<(&'static str, String)>,
}

/// A step that is `cargo` with these arguments.
fn cargo(name: &'static str, args: &[&str]) -> Step {
    let mut command = vec!["cargo".to_string()];
    command.extend(args.iter().map(ToString::to_string));
    Step {
        name,
        command,
        env: vec![],
    }
}

/// The steps of a tier, in order (`spec/21` §1.3: cheapest first).
///
/// The MSRV build is **not** here for [`Tier::Gate`]: whether it runs
/// depends on the working tree, which this pure function does not look at;
/// [`run`] adds it.
fn steps(tier: Tier, msrv_version: &str) -> Vec<Step> {
    match tier {
        Tier::Fast => vec![cargo("fmt-check", &["xtask", "fmt-check"])],
        Tier::Gate => {
            let mut doc = cargo("doc", &["doc", "--workspace", "--no-deps"]);
            doc.env.push(("RUSTDOCFLAGS", "-D warnings".to_string()));
            vec![
                cargo("fmt-check", &["xtask", "fmt-check"]),
                // Without `--all-features`: the emulator and sqlserver
                // features need services tier 1 does not have (they are
                // tier 2), and `buswatch`'s all-features tests carry a
                // lint the gate has never covered.
                cargo(
                    "clippy",
                    &[
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--",
                        "-D",
                        "warnings",
                    ],
                ),
                // The calls the checked-arithmetic rule bans and Clippy cannot
                // see (`spec/17` §2.4, RCA-ENF-5): cheap, so before the tests.
                cargo("checked-arithmetic", &["xtask", "check-arithmetic"]),
                cargo("test", &["test", "--workspace"]),
                doc,
                cargo("verify-dictionaries", &["xtask", "verify-dictionaries"]),
                cargo("verify-pipelines", &["xtask", "verify-pipelines"]),
                cargo("deny", &["deny", "check"]),
            ]
        }
        Tier::Stack => vec![
            // Run in parallel as `cargo test` runs everything: these tests
            // are isolated by construction, and `--test-threads=1` would
            // hide the contention `spec/17` §9.2.1 exists to prevent.
            // `ihb-e2e` is excluded because its `compose` feature tears a
            // whole profile down and up (tier 3, `spec/21` §8.1.1).
            cargo(
                "emulator-backed tests",
                &[
                    "test",
                    "--workspace",
                    "--all-features",
                    "--exclude",
                    "ihb-e2e",
                ],
            ),
            cargo(
                "in-process chain",
                &["test", "-p", "ihb-e2e", "--features", "emulator"],
            ),
        ],
        Tier::Flows => vec![cargo(
            "compose flows",
            // One at a time (`spec/21` §2.2): the emulator's connection
            // quota is exhausted by several profiles at once.
            &[
                "test",
                "-p",
                "ihb-e2e",
                "--features",
                "compose",
                "--",
                "--test-threads=1",
            ],
        )],
        Tier::Msrv => vec![msrv_step(msrv_version)],
    }
}

/// `cargo +<msrv> check --workspace --all-targets` (`spec/17` §1.4).
fn msrv_step(version: &str) -> Step {
    let mut step = cargo("msrv", &["check", "--workspace", "--all-targets"]);
    step.command.insert(1, format!("+{version}"));
    step
}

/// Do these changed files move the MSRV (`spec/21` §2.1)?
#[must_use]
fn msrv_needed(changed: &[String]) -> bool {
    changed
        .iter()
        .any(|file| TOOLCHAIN_INPUTS.contains(&file.as_str()))
}

/// Parsed command line.
#[derive(Debug, PartialEq, Eq)]
struct Options {
    /// Which tier.
    tier: Tier,
    /// Report every failing step rather than stopping at the first.
    keep_going: bool,
    /// After the run, post `local/<tier>` (`spec/21` §3.3).
    post: bool,
    /// Post from the existing record without running anything.
    post_only: bool,
}

/// Parse `gate`'s arguments.
fn parse(args: &[String]) -> Result<Options> {
    let mut tier = Tier::Gate;
    let mut keep_going = false;
    let (mut post, mut post_only) = (false, false);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--keep-going" => keep_going = true,
            "--post" => post = true,
            "--post-only" => post_only = true,
            "--tier" => {
                let value = iter.next().ok_or_else(|| anyhow!("--tier needs a value"))?;
                tier = Tier::parse(value)?;
            }
            other => bail!(
                "unknown argument {other:?}; usage: gate [--tier T] [--keep-going] [--post | --post-only]"
            ),
        }
    }
    Ok(Options {
        tier,
        keep_going,
        post,
        post_only,
    })
}

/// The files changed since the merge base with `origin/main`, or `None`
/// when that cannot be worked out (a shallow clone, no remote).
fn changed_since_merge_base() -> Option<Vec<String>> {
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git").args(args).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let base = git(&["merge-base", "HEAD", "origin/main"])?;
    let diff = git(&["diff", "--name-only", &base])?;
    Some(diff.lines().map(ToString::to_string).collect())
}

/// Is the toolchain installed? (`rustup toolchain list` names it.)
fn toolchain_installed(version: &str) -> bool {
    Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.starts_with(version))
        })
}

/// What happened to one step.
#[derive(Debug)]
struct Outcome {
    /// The step's name.
    name: &'static str,
    /// `Some(true)` passed, `Some(false)` failed, `None` skipped.
    passed: Option<bool>,
    /// How long it took.
    took: Duration,
}

/// Run one step, inheriting stdio so the output is the tool's own.
fn execute(step: &Step) -> Outcome {
    println!("\n==> {}: {}", step.name, step.command.join(" "));
    let started = Instant::now();
    let mut words = step.command.iter();
    let mut command = Command::new(words.next().map_or("true", String::as_str));
    command.args(words);
    for (key, value) in &step.env {
        command.env(key, value);
    }
    let passed = command.status().is_ok_and(|status| status.success());
    Outcome {
        name: step.name,
        passed: Some(passed),
        took: started.elapsed(),
    }
}

/// `cargo xtask gate`.
///
/// # Errors
///
/// A bad argument, or any step failing (all of them, with `--keep-going`).
// `unreachable_pub` and `redundant_pub_crate` contradict each other for an
// item a binary's private module exposes to `main`; `pub(crate)` is the
// honest one.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn run(args: &[String]) -> Result<()> {
    let options = parse(args)?;
    let msrv_version = crate::declared_msrv()?;
    if options.post_only {
        return post_existing(options.tier);
    }
    let started = Instant::now();
    let mut plan = steps(options.tier, &msrv_version);
    let mut outcomes: Vec<Outcome> = Vec::new();

    // Tier 1 adds the MSRV build only when something that can move it
    // changed (`spec/21` §2.1), and says so either way: a step that did
    // not run must be visible (§2.3), not silently green.
    if options.tier == Tier::Gate {
        match changed_since_merge_base() {
            Some(changed) if !msrv_needed(&changed) => {
                println!("MSRV build skipped: no toolchain input changed since the merge base");
                outcomes.push(skipped("msrv"));
            }
            changed => {
                if toolchain_installed(&msrv_version) {
                    plan.push(msrv_step(&msrv_version));
                } else {
                    let why = if changed.is_some() {
                        "changed"
                    } else {
                        "unknown"
                    };
                    println!(
                        "MSRV build SKIPPED: toolchain inputs {why}, but toolchain \
                         {msrv_version} is not installed (rustup toolchain install \
                         {msrv_version}); the hosted msrv job still covers it"
                    );
                    outcomes.push(skipped("msrv"));
                }
            }
        }
    }

    if options.tier == Tier::Stack {
        require_stack()?;
    }

    for step in &plan {
        let outcome = execute(step);
        let failed = outcome.passed == Some(false);
        outcomes.push(outcome);
        if failed && !options.keep_going {
            break;
        }
    }

    report(&outcomes);
    let failed: Vec<&str> = outcomes
        .iter()
        .filter(|outcome| outcome.passed == Some(false))
        .map(|outcome| outcome.name)
        .collect();

    // `spec/21` §3.1 — tier 1 and above leave a record, failures included.
    if options.tier != Tier::Fast {
        write_record(options.tier, &outcomes, started.elapsed(), &msrv_version)?;
    }

    if !failed.is_empty() {
        return Err(anyhow!("the gate failed at: {}", failed.join(", ")));
    }
    if options.post {
        post_existing(options.tier)?;
    }
    Ok(())
}

/// The commit checked out, and whether the tree is clean.
fn head_and_clean() -> Result<(String, bool)> {
    let git = |args: &[&str]| -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(crate::workspace_root()?)
            .output()
            .context("running git")?;
        if !output.status.success() {
            bail!("git {} failed", args.join(" "));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    Ok((
        git(&["rev-parse", "HEAD"])?,
        git(&["status", "--porcelain"])?.is_empty(),
    ))
}

/// Write this run's tier into the commit's record (`spec/21` §3.1).
fn write_record(
    tier: Tier,
    outcomes: &[Outcome],
    took: Duration,
    msrv_version: &str,
) -> Result<()> {
    let root = crate::workspace_root()?;
    let (commit, clean) = head_and_clean()?;
    if !clean {
        println!("working tree is dirty: the record says so and no status can be posted from it");
    }
    let toolchain = Command::new("rustc").arg("--version").output().map_or_else(
        |_| "unknown".to_string(),
        |output| String::from_utf8_lossy(&output.stdout).trim().to_string(),
    );
    let entry = record::entry(tier, clean, outcomes, took);
    let doc = record::merge(
        record::read(&root, &commit),
        &commit,
        entry,
        &toolchain,
        msrv_version,
    );
    let path = record::write(&root, &commit, &doc)?;
    println!("record: {}", path.display());
    Ok(())
}

/// Post `local/<tier>` from the existing record, if §3.3 allows.
fn post_existing(tier: Tier) -> Result<()> {
    let root = crate::workspace_root()?;
    let (head, clean_now) = head_and_clean()?;
    let record = record::read(&root, &head);
    if let Some(why) = record::refusal(record.as_ref(), tier, &head, clean_now) {
        bail!("not posting local/{}: {why}", tier.name());
    }
    let entry = record
        .as_ref()
        .and_then(|doc| doc.get("tiers")?.get(tier.name()))
        .unwrap_or(&serde_json::Value::Null);
    record::post(tier, &head, entry)
}

/// A skipped step's outcome.
const fn skipped(name: &'static str) -> Outcome {
    Outcome {
        name,
        passed: None,
        took: Duration::ZERO,
    }
}

/// Print the summary table.
fn report(outcomes: &[Outcome]) {
    println!("\n---- gate summary ----");
    for outcome in outcomes {
        let verdict = match outcome.passed {
            Some(true) => "ok",
            Some(false) => "FAILED",
            None => "skipped",
        };
        println!(
            "{verdict:>8}  {:<22} {:>6.1}s",
            outcome.name,
            outcome.took.as_secs_f64()
        );
    }
}

/// Tier 2 needs the broker and the database answering, and a password for
/// the latter; say so up front instead of failing 70 seconds into a test.
///
/// "Answering" is a protocol reply, not an open port
/// (`spec/21` §6.1; see `stack`).
fn require_stack() -> Result<()> {
    let (amqp, tds) = crate::stack::both_ready();
    for (what, ready) in [
        ("Service Bus emulator (AMQP)", amqp),
        ("SQL Edge (TDS)", tds),
    ] {
        if !ready {
            bail!(
                "the {what} is not answering. `cargo xtask stack up` starts both \
                 and waits until they do (an open port is not a broker that answers, \
                 spec/17 §9.2.1e)"
            );
        }
    }
    if std::env::var_os("MSSQL_SA_PASSWORD").is_none() {
        bail!(
            "MSSQL_SA_PASSWORD is not set; the sqlserver tests need it \
             (`set -a; . local/.secrets; set +a`). It is never echoed (spec/21 §3.2)."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tier: Tier) -> Vec<&'static str> {
        steps(tier, "1.98").iter().map(|step| step.name).collect()
    }

    #[test]
    fn the_gate_tier_runs_the_cheap_steps_first() {
        // spec/21 §1.3.
        assert_eq!(
            names(Tier::Gate),
            [
                "fmt-check",
                "clippy",
                "checked-arithmetic",
                "test",
                "doc",
                "verify-dictionaries",
                "verify-pipelines",
                "deny"
            ]
        );
    }

    #[test]
    fn tier_zero_is_formatting_alone() {
        assert_eq!(names(Tier::Fast), ["fmt-check"]);
    }

    #[test]
    fn formatting_is_the_xtask_task_never_cargo_fmt_all() {
        // spec/21 §1.4, spec/17 §1.0.
        for tier in [Tier::Fast, Tier::Gate] {
            let first = &steps(tier, "1.98")[0];
            assert_eq!(first.command, ["cargo", "xtask", "fmt-check"]);
        }
    }

    #[test]
    fn the_doc_step_denies_warnings_for_itself_only() {
        let plan = steps(Tier::Gate, "1.98");
        let doc = plan.iter().find(|step| step.name == "doc").unwrap();
        assert_eq!(doc.env, [("RUSTDOCFLAGS", "-D warnings".to_string())]);
        assert!(
            plan.iter()
                .filter(|s| s.name != "doc")
                .all(|s| s.env.is_empty())
        );
    }

    #[test]
    fn tier_one_needs_neither_podman_nor_the_stack() {
        // spec/21 §2.3: no all-features build, no compose feature.
        for step in steps(Tier::Gate, "1.98") {
            let joined = step.command.join(" ");
            assert!(!joined.contains("all-features"), "{joined}");
            assert!(!joined.contains("compose"), "{joined}");
            assert!(!joined.contains("podman"), "{joined}");
        }
    }

    #[test]
    fn the_flows_run_one_at_a_time() {
        // spec/21 §2.2.
        let flows = &steps(Tier::Flows, "1.98")[0];
        assert!(flows.command.contains(&"--test-threads=1".to_string()));
    }

    #[test]
    fn the_stack_tier_leaves_the_compose_tests_to_tier_three() {
        // spec/21 §8.1.1.
        let joined: Vec<String> = steps(Tier::Stack, "1.98")
            .iter()
            .map(|step| step.command.join(" "))
            .collect();
        assert!(joined[0].contains("--exclude ihb-e2e"), "{joined:?}");
        assert!(!joined[1].contains("compose"), "{joined:?}");
    }

    #[test]
    fn the_msrv_step_names_the_declared_toolchain() {
        let step = &steps(Tier::Msrv, "1.98")[0];
        assert_eq!(
            step.command,
            ["cargo", "+1.98", "check", "--workspace", "--all-targets"]
        );
    }

    #[test]
    fn only_a_toolchain_input_triggers_the_msrv_build() {
        let files =
            |names: &[&str]| -> Vec<String> { names.iter().map(ToString::to_string).collect() };
        assert!(msrv_needed(&files(&["README.md", "Cargo.lock"])));
        assert!(msrv_needed(&files(&["rust-toolchain.toml"])));
        assert!(msrv_needed(&files(&["Cargo.toml"])));
        // A crate's own manifest is not a toolchain input: it cannot move
        // the workspace's floor without the lock changing too.
        assert!(!msrv_needed(&files(&[
            "crates/db-sender/Cargo.toml",
            "spec/21-local-ci.md"
        ])));
    }

    #[test]
    fn the_command_line_is_parsed() {
        let args =
            |list: &[&str]| -> Vec<String> { list.iter().map(ToString::to_string).collect() };
        assert_eq!(
            parse(&args(&[])).unwrap(),
            Options {
                tier: Tier::Gate,
                keep_going: false,
                post: false,
                post_only: false
            }
        );
        assert_eq!(
            parse(&args(&["--tier", "stack", "--keep-going"])).unwrap(),
            Options {
                tier: Tier::Stack,
                keep_going: true,
                post: false,
                post_only: false
            }
        );
        assert!(parse(&args(&["--tier", "nope"])).is_err());
        assert!(parse(&args(&["--tier"])).is_err());
        assert!(parse(&args(&["--wat"])).is_err());
    }

    /// `spec/21` §9.1 — the workflow does not carry its own list.
    ///
    /// Every `cargo` invocation in `gate.yml` MUST be `cargo xtask gate ...`,
    /// `cargo xtask msrv` (which only reads the declared version so the
    /// msrv job can install it), or `cargo xtask stack` (the container runtime
    /// helper, `spec/21` §6). Anything else is a second list, which is
    /// the drift `spec/21` §1.2 records.
    #[test]
    fn the_workflow_only_calls_the_task() {
        let path = crate::workspace_root()
            .unwrap()
            .join(".github/workflows/gate.yml");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut offenders = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            let command = trimmed
                .trim_start_matches("- ")
                .strip_prefix("run: ")
                .unwrap_or(trimmed);
            if command.starts_with("cargo ")
                && !command.starts_with("cargo xtask gate")
                && !command.starts_with("cargo xtask msrv")
                && !command.starts_with("cargo xtask stack")
            {
                offenders.push(command.to_string());
            }
        }
        assert_eq!(
            offenders,
            Vec::<String>::new(),
            "gate.yml lists steps of its own (spec/21 §1.1)"
        );
    }
}
