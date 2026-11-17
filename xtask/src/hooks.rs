//! `cargo xtask install-hooks`.
//!
//! Implements `spec/21-local-ci.md` §7. Points this repository's
//! `core.hooksPath` at the committed `.githooks/`, whose scripts only call
//! `cargo xtask gate`. It writes the **local** configuration of this
//! repository and never the global one (§7.1).

use std::process::Command;

use anyhow::{Context, Result, bail};

/// The committed hooks directory, relative to the repository root.
const HOOKS_DIR: &str = ".githooks";

/// What changed, in words (§7.1: "MUST print what it changed").
fn describe(previous: Option<&str>) -> String {
    match previous {
        Some(old) if old == HOOKS_DIR => {
            format!("core.hooksPath was already {HOOKS_DIR}; nothing changed")
        }
        Some(old) => format!(
            "core.hooksPath: {old} -> {HOOKS_DIR} (this repository's local config; \
             restore with `git config --local core.hooksPath {old}`)"
        ),
        None => format!(
            "core.hooksPath: (unset) -> {HOOKS_DIR} (this repository's local config; \
             undo with `git config --local --unset core.hooksPath`)"
        ),
    }
}

/// Run `git` in the workspace root and return its trimmed stdout.
fn git(args: &[&str]) -> Result<(bool, String)> {
    let root = crate::workspace_root()?;
    let output = Command::new("git")
        .args(args)
        .current_dir(&root)
        .output()
        .context("running git")?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// `cargo xtask install-hooks`.
///
/// # Errors
///
/// A missing hooks directory, or git refusing the setting.
// See `gate::run` on `redundant_pub_crate` versus `unreachable_pub`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn run() -> Result<()> {
    let root = crate::workspace_root()?;
    for hook in ["pre-commit", "pre-push"] {
        let path = root.join(HOOKS_DIR).join(hook);
        if !path.is_file() {
            bail!(
                "{} is missing; the hooks are committed, check the checkout",
                path.display()
            );
        }
    }

    // `--local` is explicit on both calls: the global file is never read
    // for the "before" nor written for the "after".
    let (set, previous) = git(&["config", "--local", "--get", "core.hooksPath"])?;
    let previous = set.then_some(previous);
    let (ok, _) = git(&["config", "--local", "core.hooksPath", HOOKS_DIR])?;
    if !ok {
        bail!("git refused to set core.hooksPath");
    }
    println!("{}", describe(previous.as_deref()));
    println!("pre-commit runs tier 0 (cargo xtask gate --tier fast)");
    println!("pre-push runs tier 1 (cargo xtask gate)");
    println!("bypass with --no-verify; that leaves no local/gate status (spec/21 §7.3)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every non-blank, non-comment line of a hook.
    fn commands(name: &str) -> Vec<String> {
        let path = crate::workspace_root().unwrap().join(HOOKS_DIR).join(name);
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.starts_with("#!/bin/sh\n"), "{name} needs a shebang");
        text.lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_hooks_only_call_the_task() {
        // spec/21 §7.1, §7.2.
        assert_eq!(
            commands("pre-commit"),
            ["exec cargo xtask gate --tier fast"]
        );
        assert_eq!(commands("pre-push"), ["exec cargo xtask gate"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_hooks_are_executable() {
        // A hook git cannot execute is skipped with a warning, which would
        // look like a hook that passed.
        use std::os::unix::fs::PermissionsExt;
        for name in ["pre-commit", "pre-push"] {
            let path = crate::workspace_root().unwrap().join(HOOKS_DIR).join(name);
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "{name} is not executable ({mode:o})");
        }
    }

    #[test]
    fn what_changed_is_said_in_every_case() {
        // spec/21 §7.1.
        assert!(describe(None).contains("(unset) -> .githooks"));
        assert!(describe(Some("old")).contains("old -> .githooks"));
        assert!(describe(Some("old")).contains("git config --local core.hooksPath old"));
        assert!(describe(Some(".githooks")).contains("nothing changed"));
    }

    #[test]
    fn nothing_here_touches_the_global_config() {
        // spec/21 §7.1: no `--global` or `--system` anywhere in this file's
        // code (this test's own text excepted).
        let source = include_str!("hooks.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        assert!(!code.contains("--global") && !code.contains("--system"));
    }
}
