//! `cargo xtask check-arithmetic`: the calls the checked-arithmetic rule bans
//! outright and no Clippy lint catches.
//!
//! Implements `rust-checked-arithmetic` RCA-ENF-5 (the review grep, run by
//! the gate) and `spec/17-testing-and-quality.md` §2.4. Saturating, wrapping
//! and overflowing arithmetic, `clamp`, and `Saturating` / `Wrapping` types
//! have **no exception** (RCA-INT-4, RCA-EXC-4): where a bound on a computed
//! result is wanted it is `checked_*` plus a declared fallback.
//!
//! Only production code is read: files under `src/`, up to the first
//! `#[cfg(test)]` line (test modules sit at the end of their file here), and
//! never `tests/`, benches or fuzz targets (RCA-SCOPE-4, RCA-EXC-5). Comment
//! lines are skipped, so a rule can be quoted in prose.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Method names whose use is forbidden, matched as `.name`.
const FORBIDDEN_METHODS: [&str; 4] = [".saturating_", ".wrapping_", ".overflowing_", ".strict_"];

/// Other forbidden text, matched as written.
const FORBIDDEN_TEXT: [&str; 4] = [".clamp(", "Saturating<", "Wrapping<", "unwrap_unchecked"];

/// What a checked operation's result must not be turned into on the same line
/// (RCA-NONE-1): a panic or an implicit default.
const UNWRAPPED_AFTER_CHECKED: [&str; 3] = [".unwrap()", ".expect(", ".unwrap_or_default()"];

/// One use of a forbidden construct.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Violation {
    /// 1-based line number.
    line: usize,
    /// The line, trimmed.
    text: String,
    /// Which rule it breaks.
    rule: &'static str,
}

/// The forbidden constructs in one file's production code.
fn violations(source: &str) -> Vec<Violation> {
    let mut found = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        // The rest of the file is the test module.
        if trimmed == "#[cfg(test)]" {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        let rule = if FORBIDDEN_METHODS.iter().any(|name| line.contains(name)) {
            Some("RCA-INT-4: no saturating, wrapping, overflowing or strict arithmetic")
        } else if FORBIDDEN_TEXT.iter().any(|text| line.contains(text)) {
            Some("RCA-EXC-4 / RCA-NONE-1: no clamp, Saturating, Wrapping or unwrap_unchecked")
        } else if line.contains(".checked_")
            && UNWRAPPED_AFTER_CHECKED
                .iter()
                .any(|after| line.contains(after))
        {
            Some("RCA-NONE-1: a checked result is not unwrapped or defaulted")
        } else {
            None
        };
        if let Some(rule) = rule {
            found.push(Violation {
                line: crate::counting::next(index),
                text: trimmed.to_string(),
                rule,
            });
        }
    }
    found
}

/// Every `.rs` file under `dir`, depth first.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Scan `crates/*/src` and `xtask/src`; fail listing every violation.
// See `gate::run` on `redundant_pub_crate` versus `unreachable_pub`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn run() -> Result<()> {
    let mut files = Vec::new();
    for entry in fs::read_dir("crates").context("reading crates/")? {
        let src = entry?.path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files)?;
        }
    }
    rust_files(Path::new("xtask/src"), &mut files)?;
    files.sort();

    let mut report = Vec::new();
    for file in &files {
        // This file names the constructs it bans, so it would flag itself.
        if file.ends_with("xtask/src/arithmetic.rs") {
            continue;
        }
        let source = fs::read_to_string(file).with_context(|| file.display().to_string())?;
        for violation in violations(&source) {
            report.push(format!(
                "{}:{}: {}\n    {}",
                file.display(),
                violation.line,
                violation.rule,
                violation.text
            ));
        }
    }
    if report.is_empty() {
        println!(
            "checked-arithmetic: {} files, nothing forbidden",
            files.len()
        );
        return Ok(());
    }
    for line in &report {
        eprintln!("{line}");
    }
    bail!(
        "{} use(s) of a construct the checked-arithmetic rule bans (see spec/17 §2.4)",
        report.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> Vec<usize> {
        violations(source).iter().map(|v| v.line).collect()
    }

    #[test]
    fn the_banned_calls_are_found_by_line() {
        let source = "fn a() {\n    x.saturating_sub(1);\n    y.wrapping_add(2);\n    z.clamp(1, 2);\n    w.overflowing_mul(3);\n}\n";
        assert_eq!(rules(source), vec![2, 3, 4, 5]);
    }

    #[test]
    fn a_checked_result_must_not_be_unwrapped_or_defaulted() {
        assert_eq!(rules("let n = a.checked_add(b).unwrap();"), vec![1]);
        assert_eq!(
            rules("let n = a.checked_add(b).unwrap_or_default();"),
            vec![1]
        );
        assert_eq!(
            rules("let n = a.checked_add(b).unwrap_or(LIMIT); // declared"),
            Vec::<usize>::new()
        );
        assert_eq!(
            rules("let n = a.checked_add(b).ok_or(Overflow)?;"),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn prose_attributes_and_tests_are_not_scanned() {
        let source = "/// Never `x.saturating_sub(1)`.\n// nor y.clamp(1, 2)\n#[expect(clippy::manual_saturating_arithmetic, reason = \"not saturating_add\")]\nfn ok() {}\n#[cfg(test)]\nmod tests { fn t() { 1_u8.saturating_add(1); } }\n";
        assert_eq!(rules(source), Vec::<usize>::new());
    }
}
