//! Export naming for message files.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§6`.

use std::collections::BTreeSet;

use crate::model::MessageFormat;

/// The file extension for a format, with its leading dot (`§6.1`).
#[must_use]
pub const fn extension_for(format: MessageFormat) -> &'static str {
    match format {
        MessageFormat::Hl7 => ".hl7",
        MessageFormat::Xml => ".xml",
        MessageFormat::Json => ".json",
    }
}

/// Replace every character outside `A-Za-z0-9 _-` with `_` and trim (`§3.3`,
/// `§6.2`). Shared with the project file name rule; the result has no path
/// separator, which is why it is safe to join onto a directory.
#[must_use]
pub fn sanitise(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    replaced.trim().to_owned()
}

/// A safe file stem for a message name; `message` when nothing is left (`§6.2`).
///
/// # Examples
/// ```
/// use ultra7::export::sanitize_filename;
/// assert_eq!(sanitize_filename("../etc/passwd"), "___etc_passwd");
/// assert_eq!(sanitize_filename("  "), "message");
/// ```
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let safe = sanitise(name);
    if safe.is_empty() {
        "message".to_owned()
    } else {
        safe
    }
}

/// `stem+ext`, or `stem (2)+ext`, `stem (3)+ext` ... when already in `used`;
/// the chosen name is added to `used` (`§6.2`).
///
/// The caller owns `used` and must share one set across a whole export, or
/// duplicates are not detected. Comparison is exact, so on a
/// case-insensitive filesystem `A.hl7` and `a.hl7` can still collide.
///
/// # Examples
/// ```
/// use std::collections::BTreeSet;
/// use ultra7::export::unique_filename;
/// let mut used = BTreeSet::new();
/// assert_eq!(unique_filename("adt", ".hl7", &mut used), "adt.hl7");
/// assert_eq!(unique_filename("adt", ".hl7", &mut used), "adt (2).hl7");
/// ```
pub fn unique_filename(stem: &str, ext: &str, used: &mut BTreeSet<String>) -> String {
    let mut candidate = format!("{stem}{ext}");
    let mut counter = 2;
    while used.contains(&candidate) {
        candidate = format!("{stem} ({counter}){ext}");
        counter += 1;
    }
    used.insert(candidate.clone());
    candidate
}
