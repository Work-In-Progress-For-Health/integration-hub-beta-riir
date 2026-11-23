//! Workspace automation.
//!
//! One job today: turning the per-flow HL7 v2.xml schemas into the JSON
//! dictionaries `ihb-validation` validates against, and proving the
//! committed dictionaries still match the schemas they came from.
//!
//! Implements `spec/06-validation.md` §3.2.
//!
//! ```text
//! cargo xtask build-dictionaries    # regenerate, in place
//! cargo xtask verify-dictionaries   # fail if the committed files drifted
//! cargo xtask fmt                   # format this workspace only
//! cargo xtask fmt-check             # check formatting, changing nothing
//! ```
//!
//! The dictionaries are committed rather than built, so a container image
//! needs no XSD toolchain and a build is one `cargo build`. `verify` is what
//! stops that convenience becoming a lie: it runs in CI, so a schema edit
//! that nobody regenerated fails the build rather than shipping.
//!
//! The formatting tasks exist because `cargo fmt --all` reaches through this
//! workspace's path dependencies and rewrites files in the `hl7-rust` and
//! `er7-rust` checkouts alongside it. Those are separate repositories with
//! their own formatting; editing them from here would be both surprising and
//! wrong. `cargo fmt -p <member>`, one member at a time, does not.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod gate;
mod hooks;
mod schedule;
mod stack;

use anyhow::{Context, Result, anyhow, bail};

/// Every member of this workspace, for the formatting tasks.
///
/// Kept in step with `[workspace] members` in the root `Cargo.toml` as each
/// phase adds a crate. A member missing from this list is silently
/// unformatted, so `fmt-check` also asserts the two lists agree.
const MEMBERS: &[&str] = &[
    "xtask",
    "ihb-config",
    "ihb-tls",
    "ihb-health",
    "ihb-telemetry",
    "ihb-runtime",
    "ihb-fields",
    "ihb-validation",
    "ihb-bus",
    "hl7-server",
    "hl7-soap-server",
    "ihb-transformer",
    "ihb-sender",
    "ihb-identity",
    "ihb-sql",
    "buswatch",
    "message-store-service",
    "message-replay-job",
    "hl7-phw-transformer",
    "hl7-chemo-transformer",
    "hl7-pims-transformer",
    "hl7-sender",
    "soap-sender",
    "hl7-mock-receiver",
    "http-mock-receiver",
    "dashboard",
    "xml-fhir-proms-transformer",
    "hl7-wds-transformer",
    "hl7-lims-wrrs-transformer",
    "db-sender",
    "fhir-sender",
    "wrds-service",
    "hl7-core-reference-transformer",
    "hl7-rest-server",
    "rest-server",
    "lookup-service",
    "ultra7",
    "ihb-e2e",
    "integration-hub-tester",
];

/// The schema groups, and the trigger events that arrive carried by another
/// structure.
///
/// A schema directory holds `ADT_A05.xsd` but nothing in it says that an
/// `ADT^A28` is one, so that mapping has to be stated. These entries mirror
/// `_DEFAULT_TRIGGER_MAPPING` in the Python's
/// `shared_libs/hl7_validation/hl7_validation/validate.py`, plus the two
/// PIMS-only triggers its ingress server accepts
/// (`spec/07-hl7-server.md` §7).
///
/// See `spec/06-validation.md` §2.4 and §3.1.
const GROUPS: &[Group] = &[
    Group {
        name: "phw",
        aliases: ADT_A05_ALIASES,
    },
    Group {
        name: "paris",
        aliases: ADT_A05_ALIASES,
    },
    Group {
        name: "chemo",
        aliases: ADT_A05_ALIASES,
    },
    Group {
        name: "mosaiq",
        aliases: ADT_A05_ALIASES,
    },
    Group {
        name: "wds",
        aliases: ADT_A05_ALIASES,
    },
    Group {
        name: "pims",
        aliases: PIMS_ALIASES,
    },
];

/// Trigger aliases for the five groups whose registration and update
/// messages arrive as `ADT_A05` and whose merges arrive as `ADT_A39`.
const ADT_A05_ALIASES: &[(&str, &str)] = &[
    ("ADT_A28", "ADT_A05"),
    ("ADT_A31", "ADT_A05"),
    ("ADT_A40", "ADT_A39"),
];

/// PIMS speaks HL7 2.3.1 and carries its registrations and updates as
/// `ADT_A01`. Its merges arrive under their own `ADT_A40` structure, and
/// under `ADT_A39` from sites that follow the later releases, so both
/// schemas are present and neither needs an alias.
const PIMS_ALIASES: &[(&str, &str)] = &[
    ("ADT_A04", "ADT_A01"),
    ("ADT_A08", "ADT_A01"),
    ("ADT_A28", "ADT_A01"),
    ("ADT_A31", "ADT_A01"),
];

/// One flow's schema directory and what cannot be read out of it.
struct Group {
    /// Directory name under `crates/ihb-validation/schemas/`, and the value
    /// `HL7_VALIDATION_FLOW` takes for this flow.
    name: &'static str,
    /// `CODE_TRIGGER` to the structure that carries it.
    aliases: &'static [(&'static str, &'static str)],
}

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("build-dictionaries") => build(false),
        Some("verify-dictionaries") => build(true),
        Some("fmt") => fmt(false),
        Some("fmt-check") => fmt(true),
        Some("msrv") => msrv(),
        Some("install-hooks") => hooks::run(),
        Some("schedule") => schedule::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("stack") => stack::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("gate") => gate::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("verify-pipelines") => verify_pipelines(),
        Some(other) => {
            eprintln!("xtask: unknown task {other:?}");
            usage();
            return ExitCode::FAILURE;
        }
        None => {
            usage();
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Print the task list.
fn usage() {
    eprintln!(
        "usage: cargo xtask \
         <build-dictionaries|verify-dictionaries|fmt|fmt-check|msrv|verify-pipelines|gate|install-hooks|stack>"
    );
}

/// Check that the ADO build definitions and the workspace agree.
///
/// Implements `spec/18-deployment.md` §6.6. Two failures are possible and
/// both are silent without this:
///
/// 1. A definition names `BIN=something-that-does-not-exist`. The image
///    builds — the Dockerfile's `cargo build --bin` fails, so in fact it
///    does not, but only after a full dependency compile on a build agent.
///    Catching it here costs milliseconds.
/// 2. A new deployable binary gains no build definition. Nothing fails.
///    The service simply never ships, and the omission is invisible until
///    somebody goes looking for an image that was never pushed.
///
/// The second is the reason this task exists. The first is a bonus.
// The body is one linear walk (read, compare, report); splitting it would
// scatter a single invariant across helpers.
#[allow(clippy::too_many_lines)]
fn verify_pipelines() -> Result<()> {
    // Binaries that are tooling, not services: `xtask` is this program,
    // `health-probe` ships *inside* every image rather than as one of its
    // own (`spec/18-deployment.md` §2.1).
    const NOT_SERVICES: &[&str] = &[
        "xtask",
        "health-probe",
        // Phase 1 and not production ready (`spec/20-lookup-service.md`
        // §1.1): the Python has no build pipeline for it either, because its
        // deployment is that project's Phase 2. It has an image recipe (the
        // common Dockerfile) and a Compose service, but nothing is pushed.
        "lookup-service",
        // Developer tooling that is never deployed
        // (`spec/ultra7-test-message-sender.md` §1.1,
        // `spec/integration-hub-tester-component-preview.md`): no image, no
        // pipeline, nothing pushed.
        "ultra7",
        "integration-hub-tester",
    ];

    let root = workspace_root()?;
    let directory = root.join("pipeline-ado");

    let mut binaries = Vec::new();
    for member in MEMBERS {
        let manifest = root.join("crates").join(member).join("Cargo.toml");
        let manifest = if manifest.exists() {
            manifest
        } else {
            root.join(member).join("Cargo.toml")
        };
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        // An explicit `[[bin]]` table, or the implicit `src/main.rs`.
        let mut in_bin = false;
        let mut found_explicit = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_bin = line == "[[bin]]";
                continue;
            }
            if in_bin
                && let Some(rest) = line.strip_prefix("name")
                && let Some(name) = rest.trim_start().strip_prefix('=')
            {
                binaries.push(name.trim().trim_matches('"').to_owned());
                found_explicit = true;
            }
        }
        if !found_explicit && manifest.with_file_name("src").join("main.rs").exists() {
            binaries.push((*member).to_owned());
        }
    }
    binaries.retain(|binary| !NOT_SERVICES.contains(&binary.as_str()));
    binaries.sort();
    binaries.dedup();

    let mut declared = Vec::new();
    let mut problems = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(&directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("rust-build.yml"))
        })
        .collect();
    entries.sort();

    for path in &entries {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();

        let Some(binary) = text
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("buildArgs: 'BIN="))
            .and_then(|rest| rest.strip_suffix('\''))
        else {
            problems.push(format!("{name}: no `buildArgs: 'BIN=…'`"));
            continue;
        };

        if binaries.iter().all(|known| known != binary) {
            problems.push(format!(
                "{name}: BIN={binary} is not a binary in this workspace"
            ));
        }
        declared.push(binary.to_owned());
    }

    for binary in &binaries {
        if !declared.contains(binary) {
            problems.push(format!(
                "no build definition in pipeline-ado/ produces `{binary}`. \
                 Add one, or add the binary to NOT_SERVICES if it does not ship."
            ));
        }
    }

    if problems.is_empty() {
        println!(
            "pipeline-ado: {} definitions, {} service binaries, in agreement",
            entries.len(),
            binaries.len()
        );
        return Ok(());
    }
    for problem in &problems {
        eprintln!("  {problem}");
    }
    Err(anyhow!(
        "{} {} the workspace (spec/18 §6.6)",
        problems.len(),
        if problems.len() == 1 {
            "problem disagrees with"
        } else {
            "problems disagree with"
        }
    ))
}

/// Print the declared minimum supported Rust version.
///
/// Implements `spec/rust-msrv-n-minus-2.md` §1.3.
///
/// One line, no newline decoration beyond it, so the gate and the README can
/// both write `cargo +$(cargo xtask msrv) check`. The point is that the
/// toolchain the check runs against is *read from the declaration* rather
/// than written beside it: a hard-coded `+1.95` in a CI file is a second
/// copy, and a second copy is a thing that goes stale the first time the
/// floor moves.
fn msrv() -> Result<()> {
    println!("{}", declared_msrv()?);
    Ok(())
}

/// The `rust-version` declared under `[workspace.package]`.
fn declared_msrv() -> Result<String> {
    let root = workspace_root()?.join("Cargo.toml");
    let text =
        std::fs::read_to_string(&root).with_context(|| format!("reading {}", root.display()))?;

    let version = text
        .lines()
        .map(str::trim)
        // `[workspace.package]` is the only table that may carry it
        // (§1.3), and nothing else in the root manifest uses the key, so a
        // plain scan is honest here.
        .find_map(|line| line.strip_prefix("rust-version"))
        .and_then(|rest| rest.trim_start().strip_prefix('='))
        .map(|rest| rest.trim().trim_matches('"').to_string())
        .filter(|version| !version.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "no `rust-version` in {}.\n\
                 spec/rust-msrv-n-minus-2.md §1.3 requires the MSRV to be \
                 declared under [workspace.package].",
                root.display()
            )
        })?;

    Ok(version)
}

/// Format, or check the formatting of, every member of this workspace.
///
/// Runs `cargo fmt -p <member>` once per member rather than
/// `cargo fmt --all`, for the reason in the module documentation: `--all`
/// reaches into the path dependencies and edits other repositories.
fn fmt(check_only: bool) -> Result<()> {
    let declared = declared_members()?;
    let known: Vec<String> = MEMBERS.iter().map(|m| (*m).to_string()).collect();
    if declared != known {
        bail!(
            "xtask's MEMBERS list is out of step with the workspace.\n\
             Cargo.toml has: {}\n\
             xtask has:      {}",
            declared.join(", "),
            known.join(", ")
        );
    }

    let mut unformatted = Vec::new();

    for member in MEMBERS {
        let mut command = Command::new("cargo");
        command.arg("fmt").arg("-p").arg(member);
        if check_only {
            command.arg("--").arg("--check");
        }
        let status = command
            .status()
            .with_context(|| format!("running cargo fmt for {member}"))?;
        if !status.success() {
            unformatted.push(*member);
        }
    }

    if unformatted.is_empty() {
        println!("  formatting ok: {}", MEMBERS.join(", "));
        Ok(())
    } else {
        bail!(
            "these crates are not formatted: {}.\n\
             Run `cargo xtask fmt`.",
            unformatted.join(", ")
        )
    }
}

/// The workspace member names declared in the root `Cargo.toml`.
///
/// Read by hand rather than through `cargo metadata` so `xtask` needs no
/// JSON dependency for one list of strings. The manifest's `members` array
/// holds directory paths; the member's name is its last segment, which holds
/// for every crate in this workspace by construction.
fn declared_members() -> Result<Vec<String>> {
    let manifest_path = workspace_root()?.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;

    let body = manifest
        .split_once("members = [")
        .context("the root Cargo.toml has no [workspace] members list")?
        .1
        .split_once(']')
        .context("the root Cargo.toml's members list is not closed")?
        .0;

    Ok(body
        .split(',')
        .filter_map(|entry| {
            let trimmed = entry.trim().trim_matches('"');
            if trimmed.is_empty() {
                None
            } else {
                trimmed.rsplit('/').next().map(str::to_string)
            }
        })
        .collect())
}

/// Convert every group's schemas, then either write the result or compare it
/// with what is committed.
///
/// One pass over the groups serves both tasks deliberately: `verify` running
/// the same code as `build` is what makes a green `verify` mean anything.
fn build(verify_only: bool) -> Result<()> {
    let root = workspace_root()?;
    let schemas = root.join("crates/ihb-validation/schemas");
    let dictionaries = root.join("crates/ihb-validation/dictionaries");

    if !verify_only {
        std::fs::create_dir_all(&dictionaries)
            .with_context(|| format!("creating {}", dictionaries.display()))?;
    }

    let mut drifted = Vec::new();

    for group in GROUPS {
        let directory = schemas.join(group.name);
        let json = convert(&directory, group)
            .with_context(|| format!("converting the {} schemas", group.name))?;

        // Loading it back is the only honest check that what we wrote is
        // what `ihb-validation` will be able to read.
        hl7_2::Dictionary::from_json(&json, group.name)
            .map_err(|error| anyhow::anyhow!("{error}"))
            .with_context(|| format!("the {} dictionary does not load", group.name))?;

        let target = dictionaries.join(format!("{}.json", group.name));

        if verify_only {
            let committed = std::fs::read_to_string(&target)
                .with_context(|| format!("reading {}", target.display()))?;
            if committed == json {
                println!("  ok      {}", group.name);
            } else {
                println!("  DRIFTED {}", group.name);
                drifted.push(group.name);
            }
        } else {
            std::fs::write(&target, &json)
                .with_context(|| format!("writing {}", target.display()))?;
            println!("  wrote   {} ({} bytes)", target.display(), json.len());
        }
    }

    if !drifted.is_empty() {
        bail!(
            "these dictionaries no longer match their schemas: {}.\n\
             Run `cargo xtask build-dictionaries` and commit the result.",
            drifted.join(", ")
        );
    }

    Ok(())
}

/// Convert one group's schema directory into dictionary JSON.
fn convert(directory: &Path, group: &Group) -> Result<String> {
    let mut aliases = BTreeMap::new();
    for (trigger, structure) in group.aliases {
        aliases.insert((*trigger).to_string(), (*structure).to_string());
    }

    let options = hl7_2_from_xsd_into_json_dictionary::Options {
        name: Some(group.name.to_string()),
        aliases,
        ..Default::default()
    };

    let document = hl7_2_from_xsd_into_json_dictionary::convert_directory(directory, &options)
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    Ok(document.to_json())
}

/// The workspace root, found by walking up from this crate.
///
/// `CARGO_MANIFEST_DIR` is `<root>/xtask`, and `cargo xtask` may be invoked
/// from anywhere in the tree, so the current directory is not a safe
/// starting point.
fn workspace_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("xtask has no parent directory; the workspace layout has changed")
}
