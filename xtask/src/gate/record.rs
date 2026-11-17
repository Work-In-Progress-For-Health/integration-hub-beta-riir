//! The evidence of a run, and posting it.
//!
//! Implements `spec/21-local-ci.md` §3. A run of tier 1 or above writes
//! `target/gate/<sha>.json`; `--post` publishes a `local/<tier>` commit
//! status from it, and refuses when it should (§3.3).
//!
//! The record carries **names, results and durations only**: no command
//! line, no environment, no output. That is how §3.2 holds even for the
//! `sqlserver` tier, whose password reaches a step through its environment
//! and goes no further.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{Outcome, Tier};

/// Where the records live, relative to the workspace root.
const DIR: &str = "target/gate";

impl Tier {
    /// The tier's name, as `--tier` takes it and as the status context
    /// `local/<name>` uses.
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Gate => "gate",
            Self::Stack => "stack",
            Self::Flows => "flows",
            Self::Msrv => "msrv",
        }
    }
}

/// What a step's result is called in the record.
const fn verdict(passed: Option<bool>) -> &'static str {
    match passed {
        Some(true) => "passed",
        Some(false) => "failed",
        None => "skipped",
    }
}

/// One tier's entry. A tier fails if any of its steps failed; a skipped
/// step is recorded as skipped, so a missing check is visible (§2.3).
pub(super) fn entry(tier: Tier, clean: bool, outcomes: &[Outcome], took: Duration) -> Value {
    let failed = outcomes.iter().any(|o| o.passed == Some(false));
    json!({
        "tier": tier.name(),
        "result": if failed { "failed" } else { "passed" },
        "clean": clean,
        "seconds": took.as_secs_f64(),
        "time": rfc3339(SystemTime::now()),
        "steps": outcomes.iter().map(|o| json!({
            "name": o.name,
            "result": verdict(o.passed),
            "seconds": o.took.as_secs_f64(),
        })).collect::<Vec<_>>(),
    })
}

/// Fold a tier's entry into the commit's record, keeping the other tiers
/// already run on the same commit (tier 1 then tier 2 accumulate).
pub(super) fn merge(
    existing: Option<Value>,
    commit: &str,
    entry: Value,
    toolchain: &str,
    msrv: &str,
) -> Value {
    let mut tiers = existing
        .filter(|doc| doc["commit"] == commit)
        .and_then(|doc| doc["tiers"].as_object().cloned())
        .unwrap_or_default();
    if let Some(name) = entry["tier"].as_str() {
        tiers.insert(name.to_string(), entry);
    }
    // The record is clean only if every tier in it ran on a clean tree.
    let clean = tiers.values().all(|tier| tier["clean"] == true);
    json!({
        "commit": commit,
        "clean": clean,
        "toolchain": toolchain,
        "msrv": msrv,
        "host": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "tiers": tiers,
    })
}

/// The path of a commit's record.
pub(super) fn path(root: &Path, commit: &str) -> PathBuf {
    root.join(DIR).join(format!("{commit}.json"))
}

/// Read a commit's record, if there is one.
pub(super) fn read(root: &Path, commit: &str) -> Option<Value> {
    std::fs::read_to_string(path(root, commit))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// Write a commit's record.
pub(super) fn write(root: &Path, commit: &str, doc: &Value) -> Result<PathBuf> {
    let path = path(root, commit);
    std::fs::create_dir_all(path.parent().context("record path has a parent")?)?;
    std::fs::write(&path, serde_json::to_string_pretty(doc)? + "\n")
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Why a status must **not** be posted, or `None` when it may be
/// (`spec/21` §3.3).
///
/// `head` is the commit checked out now and `clean_now` whether the tree is
/// clean now; the record's own `clean` is the tree *when the tier ran*.
pub(super) fn refusal(
    record: Option<&Value>,
    tier: Tier,
    head: &str,
    clean_now: bool,
) -> Option<String> {
    let Some(record) = record else {
        return Some(format!("there is no record for {head}; run the tier first"));
    };
    if record["commit"] != head {
        return Some(format!(
            "the record is for {}, but {head} is checked out",
            record["commit"].as_str().unwrap_or("?")
        ));
    }
    if !clean_now {
        return Some(
            "the working tree is dirty; a status would vouch for code that is not the commit"
                .into(),
        );
    }
    let Some(entry) = record["tiers"].get(tier.name()) else {
        return Some(format!("the record has no `{}` tier", tier.name()));
    };
    if entry["clean"] != true {
        return Some(format!(
            "tier `{}` ran on a dirty tree; run it again on a clean one",
            tier.name()
        ));
    }
    if entry["result"] != "passed" {
        return Some(format!("tier `{}` did not pass", tier.name()));
    }
    None
}

/// Post `local/<tier>` for `commit` with `gh` (`spec/21` §3.3).
///
/// The description is the tier's duration and the host, nothing else.
pub(super) fn post(tier: Tier, commit: &str, entry: &Value) -> Result<()> {
    let seconds = entry["seconds"].as_f64().unwrap_or(0.0);
    let description = format!(
        "{} passed locally in {seconds:.0}s on {}-{}",
        tier.name(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let status = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{{owner}}/{{repo}}/statuses/{commit}"),
            "-f",
            "state=success",
            "-f",
            &format!("context=local/{}", tier.name()),
            "-f",
            &format!("description={description}"),
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .context("running gh")?;
    if !status.success() {
        bail!("gh refused to post the status");
    }
    println!("posted local/{} for {commit}", tier.name());
    Ok(())
}

/// A UTC timestamp, `2026-11-17T10:30:00Z`.
fn rfc3339(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Howard Hinnant's civil-from-days.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(name: &'static str, passed: Option<bool>) -> Outcome {
        Outcome {
            name,
            passed,
            took: Duration::from_secs(2),
        }
    }

    fn record_for(commit: &str, clean: bool, passed: bool) -> Value {
        let outcomes = [outcome("test", Some(passed))];
        merge(
            None,
            commit,
            entry(Tier::Gate, clean, &outcomes, Duration::from_secs(5)),
            "rustc 1.100.0",
            "1.95",
        )
    }

    #[test]
    fn a_failing_tier_is_recorded_as_failing() {
        // spec/21 §9.2.
        let record = record_for("abc", true, false);
        assert_eq!(record["tiers"]["gate"]["result"], "failed");
        assert_eq!(record["tiers"]["gate"]["steps"][0]["result"], "failed");
    }

    #[test]
    fn a_skipped_step_is_visible_and_does_not_fail_the_tier() {
        // spec/21 §2.3.
        let outcomes = [outcome("msrv", None), outcome("test", Some(true))];
        let entry = entry(Tier::Gate, true, &outcomes, Duration::from_secs(1));
        assert_eq!(entry["result"], "passed");
        assert_eq!(entry["steps"][0]["result"], "skipped");
    }

    #[test]
    fn a_dirty_tree_is_recorded_and_refused_for_posting() {
        // spec/21 §3.1, §3.3, §9.2.
        let dirty = record_for("abc", false, true);
        assert_eq!(dirty["clean"], false);
        let why = refusal(Some(&dirty), Tier::Gate, "abc", true).unwrap();
        assert!(why.contains("dirty"), "{why}");
        // And a tree that is dirty *now* is refused whatever the record says.
        let clean = record_for("abc", true, true);
        assert!(
            refusal(Some(&clean), Tier::Gate, "abc", false)
                .unwrap()
                .contains("dirty")
        );
    }

    #[test]
    fn a_status_is_refused_for_another_commit_a_failed_tier_or_no_record() {
        // spec/21 §3.3.
        let passed = record_for("abc", true, true);
        assert_eq!(refusal(Some(&passed), Tier::Gate, "abc", true), None);
        assert!(
            refusal(Some(&passed), Tier::Gate, "def", true)
                .unwrap()
                .contains("checked out")
        );
        assert!(
            refusal(Some(&passed), Tier::Stack, "abc", true)
                .unwrap()
                .contains("no `stack`")
        );
        let failed = record_for("abc", true, false);
        assert!(
            refusal(Some(&failed), Tier::Gate, "abc", true)
                .unwrap()
                .contains("did not pass")
        );
        assert!(
            refusal(None, Tier::Gate, "abc", true)
                .unwrap()
                .contains("no record")
        );
    }

    #[test]
    fn tiers_accumulate_on_one_commit_and_a_new_commit_starts_afresh() {
        let first = record_for("abc", true, true);
        let stack = entry(
            Tier::Stack,
            true,
            &[outcome("emulator", Some(true))],
            Duration::from_secs(9),
        );
        let both = merge(Some(first.clone()), "abc", stack.clone(), "rustc", "1.95");
        assert!(both["tiers"]["gate"].is_object() && both["tiers"]["stack"].is_object());
        let next = merge(Some(first), "def", stack, "rustc", "1.95");
        assert!(
            next["tiers"]["gate"].is_null(),
            "another commit's tiers do not carry over"
        );
    }

    #[test]
    fn no_secret_reaches_the_record() {
        // spec/21 §3.2, §9.2. A step's environment holds the password; the
        // record is built from outcomes, which carry names and numbers.
        const SECRET: &str = "Sup3r-Secret-Passw0rd!";
        let step = super::super::Step {
            name: "sqlserver",
            command: vec!["cargo".into(), "test".into()],
            env: vec![("MSSQL_SA_PASSWORD", SECRET.to_string())],
        };
        let outcomes = [outcome(step.name, Some(true))];
        let doc = merge(
            None,
            "abc",
            entry(Tier::Stack, true, &outcomes, Duration::from_secs(1)),
            "rustc 1.100.0",
            "1.95",
        );
        let text = serde_json::to_string(&doc).unwrap();
        assert!(
            !text.contains(SECRET) && !text.contains("MSSQL_SA_PASSWORD"),
            "{text}"
        );
    }

    #[test]
    fn the_timestamp_is_rfc3339_utc() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // 2026-11-17T00:00:00Z
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_hours(498_576)),
            "2026-11-17T00:00:00Z"
        );
        // A leap day.
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_hours(474_768)),
            "2024-02-29T00:00:00Z"
        );
    }
}
