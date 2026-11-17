//! `cargo xtask schedule print|install|remove|status|run`.
//!
//! Implements `spec/21-local-ci.md` §10. A launchd user agent runs tiers 2
//! and 3 nightly on this machine, one after the other, and writes the same
//! evidence record as a manual run (`spec/21` §3).
//!
//! # What the job will not do
//!
//! It never fights the developer for the machine or the tree: it skips a
//! dirty tree, an already-running job, and a commit whose two tiers already
//! passed. It never posts a commit status and never pushes. It brings the
//! stack down afterwards even when the tier failed.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The launchd label, and the plist's file name.
const LABEL: &str = "com.dhcw.integration-hub-riir.nightly";

/// When it runs (local time). Early, when the machine is idle.
const HOUR: u8 = 2;
/// The minute.
const MINUTE: u8 = 30;

/// The tiers the job runs, in order. Tier 3 starts only after tier 2 has
/// torn its stack down, so the Service Bus emulator's connection quota is
/// never shared (`spec/21` §2.2).
const TIERS: [&str; 2] = ["stack", "flows"];

/// The agent's plist, for a repository at `repo` and a home at `home`.
///
/// A login shell, so `PATH` carries `rustup`, the `kache` shims and
/// Homebrew's `podman` as they do in a terminal; launchd gives a job almost
/// nothing. No secret is written here: `run` reads `local/.secrets` itself.
fn plist(repo: &Path, home: &Path) -> String {
    let repo = xml_escape(&repo.display().to_string());
    let log = xml_escape(
        &home
            .join("Library/Logs/integration-hub-riir-nightly.log")
            .display()
            .to_string(),
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/zsh</string>
    <string>-l</string>
    <string>-c</string>
    <string>cd '{repo}' &amp;&amp; cargo xtask schedule run</string>
  </array>
  <key>WorkingDirectory</key><string>{repo}</string>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key><integer>{HOUR}</integer>
    <key>Minute</key><integer>{MINUTE}</integer>
  </dict>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
  <key>Nice</key><integer>10</integer>
</dict>
</plist>
"#
    )
}

/// Escape the five XML specials, for a path placed in the plist.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Where the agent's plist lives.
fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// The home directory.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

/// The launchd domain of the current user, `gui/<uid>`.
fn domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("running id")?;
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

/// `KEY=VALUE` lines of a `.secrets` file, with an optional `export `,
/// optional quotes and `#` comments. Values are never printed.
fn parse_secrets(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            Some((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

/// Whether every scheduled tier already passed on this commit, from the
/// text of its record.
fn already_passed(record: Option<&str>) -> bool {
    let Some(doc) = record.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
    else {
        return false;
    };
    TIERS
        .iter()
        .all(|tier| doc["tiers"][tier]["result"] == "passed" && doc["tiers"][tier]["clean"] == true)
}

/// Why the job should not run now, or `None` to run.
fn skip_reason(clean: bool, record: Option<&str>, other_running: bool) -> Option<&'static str> {
    if other_running {
        Some("another scheduled run is in progress")
    } else if !clean {
        Some("the working tree is dirty; not running over uncommitted work")
    } else if already_passed(record) {
        Some("this commit already passed both tiers")
    } else {
        None
    }
}

/// Run `git` in `root` and return its trimmed stdout.
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// A lock that is released when dropped. Holds the pid, so a lock left by a
/// crash is recognised as stale.
struct Lock(PathBuf);

impl Lock {
    /// Take the lock, or `None` when a live process holds it.
    fn take(path: PathBuf) -> Option<Self> {
        if let Some(pid) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            && Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .is_ok_and(|o| o.status.success())
        {
            return None;
        }
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, std::process::id().to_string()).ok()?;
        Some(Self(path))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// Run this same binary with `args`, with the secrets in its environment.
fn xtask(root: &Path, secrets: &[(String, String)], args: &[&str]) -> Result<bool> {
    let status = Command::new(std::env::current_exe()?)
        .args(args)
        .envs(secrets.iter().map(|(k, v)| (k, v)))
        .current_dir(root)
        .status()
        .with_context(|| format!("running xtask {}", args.join(" ")))?;
    Ok(status.success())
}

/// The job: tier 2, tear down, tier 3.
fn job() -> Result<()> {
    let root = crate::workspace_root()?;
    println!(
        "== nightly {} ==",
        git(&root, &["log", "-1", "--format=%h %s"])?
    );

    let head = git(&root, &["rev-parse", "HEAD"])?;
    let clean = git(&root, &["status", "--porcelain"])?.is_empty();
    let record =
        std::fs::read_to_string(root.join("target/gate").join(format!("{head}.json"))).ok();
    let lock = Lock::take(root.join("target/schedule/lock"));
    if let Some(reason) = skip_reason(clean, record.as_deref(), lock.is_none()) {
        println!("skipped: {reason}");
        return Ok(());
    }

    let secrets = std::fs::read_to_string(root.join("local/.secrets"))
        .map_or_else(|_| Vec::new(), |text| parse_secrets(&text));

    let mut failed = Vec::new();

    // Tier 2 needs the stack; it is torn down whatever happens, before
    // tier 3 starts its own profiles.
    if xtask(&root, &secrets, &["stack", "up"])? {
        if !xtask(&root, &secrets, &["gate", "--tier", "stack"])? {
            failed.push("stack");
        }
    } else {
        failed.push("stack (the stack did not come up)");
    }
    xtask(&root, &secrets, &["stack", "down"]).ok();

    if !xtask(&root, &secrets, &["gate", "--tier", "flows"])? {
        failed.push("flows");
    }

    if failed.is_empty() {
        println!("nightly passed");
        Ok(())
    } else {
        bail!("nightly failed: {}", failed.join(", "))
    }
}

/// `cargo xtask schedule <print|install|remove|status|run>`.
///
/// # Errors
///
/// A bad argument, `launchctl` refusing, or the job failing.
// See `gate::run` on `redundant_pub_crate` versus `unreachable_pub`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn run(args: &[String]) -> Result<()> {
    let root = crate::workspace_root()?;
    let home = home()?;
    match args.first().map(String::as_str) {
        Some("print") => {
            print!("{}", plist(&root, &home));
            Ok(())
        }
        Some("install") => {
            let path = plist_path(&home);
            std::fs::create_dir_all(path.parent().context("a LaunchAgents directory")?)?;
            std::fs::write(&path, plist(&root, &home))?;
            let domain = domain()?;
            // Replace a previous install rather than fail on it.
            Command::new("launchctl")
                .args(["bootout", &format!("{domain}/{LABEL}")])
                .output()
                .ok();
            let status = Command::new("launchctl")
                .args(["bootstrap", &domain])
                .arg(&path)
                .status()
                .context("running launchctl")?;
            if !status.success() {
                bail!(
                    "launchctl bootstrap failed; the plist is at {}",
                    path.display()
                );
            }
            println!("installed {} (daily {HOUR:02}:{MINUTE:02})", path.display());
            println!(
                "log: {}",
                home.join("Library/Logs/integration-hub-riir-nightly.log")
                    .display()
            );
            println!("remove with `cargo xtask schedule remove`");
            Ok(())
        }
        Some("remove") => {
            let path = plist_path(&home);
            Command::new("launchctl")
                .args(["bootout", &format!("{}/{LABEL}", domain()?)])
                .output()
                .ok();
            if path.exists() {
                std::fs::remove_file(&path)?;
                println!("removed {}", path.display());
            } else {
                println!("nothing installed at {}", path.display());
            }
            Ok(())
        }
        Some("status") => {
            let status = Command::new("launchctl")
                .args(["print", &format!("{}/{LABEL}", domain()?)])
                .output()?;
            if status.status.success() {
                println!("installed and loaded: {LABEL}");
            } else {
                println!("not loaded: {LABEL}");
            }
            Ok(())
        }
        Some("run") => job(),
        _ => bail!("usage: cargo xtask schedule print|install|remove|status|run"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_names_the_repository_and_carries_no_secret() {
        // spec/21 §10.2.
        let text = plist(Path::new("/Users/x/my repo"), Path::new("/Users/x"));
        assert!(text.contains("cd '/Users/x/my repo' &amp;&amp; cargo xtask schedule run"));
        assert!(text.contains("<key>Hour</key><integer>2</integer>"));
        assert!(!text.to_lowercase().contains("password"));
        assert!(!text.contains("MSSQL"));
    }

    #[test]
    fn a_path_with_markup_is_escaped() {
        let text = plist(Path::new("/Users/x/a&b<c>"), Path::new("/Users/x"));
        assert!(text.contains("a&amp;b&lt;c&gt;"));
        assert!(!text.contains("a&b"));
    }

    #[test]
    fn secrets_parse_with_export_quotes_and_comments() {
        let parsed = parse_secrets("# c\n\nexport A=\"x y\"\nB='z'\nC=plain\nnot a pair\n");
        assert_eq!(
            parsed,
            vec![
                ("A".to_string(), "x y".to_string()),
                ("B".to_string(), "z".to_string()),
                ("C".to_string(), "plain".to_string()),
            ]
        );
    }

    fn record(stack: &str, flows: &str, clean: bool) -> String {
        format!(
            r#"{{"tiers":{{"stack":{{"result":"{stack}","clean":{clean}}},"flows":{{"result":"{flows}","clean":{clean}}}}}}}"#
        )
    }

    #[test]
    fn a_commit_that_passed_both_tiers_is_not_run_again() {
        assert!(already_passed(Some(&record("passed", "passed", true))));
        assert!(!already_passed(Some(&record("passed", "failed", true))));
        assert!(!already_passed(Some(&record("passed", "passed", false))));
        assert!(!already_passed(None));
        assert!(!already_passed(Some("not json")));
    }

    #[test]
    fn the_job_yields_to_the_developer() {
        // spec/21 §10.3: never over uncommitted work, never two at once.
        assert!(skip_reason(false, None, false).is_some());
        assert!(skip_reason(true, None, true).is_some());
        assert!(skip_reason(true, Some(&record("passed", "passed", true)), false).is_some());
        assert_eq!(skip_reason(true, None, false), None);
    }

    #[test]
    fn a_lock_held_by_a_live_process_is_respected_and_a_stale_one_is_not() {
        let dir = std::env::temp_dir().join(format!("ihb-lock-{}", std::process::id()));
        let path = dir.join("lock");

        let held = Lock::take(path.clone()).expect("free");
        assert!(Lock::take(path.clone()).is_none(), "this process holds it");
        drop(held);

        // A pid that is not running is stale.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "4294967").unwrap();
        assert!(Lock::take(path).is_some(), "a dead holder is not a holder");
        std::fs::remove_dir_all(dir).ok();
    }
}
