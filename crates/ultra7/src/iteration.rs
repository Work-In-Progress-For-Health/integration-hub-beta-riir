//! The iteration field: recompute a highlighted range per send.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§4`.

use std::fmt::Write as _;

use time::OffsetDateTime;

use crate::model::{IterationMode, IterationSpec};

/// `content` with the spec's range recomputed for send `index` (0-based),
/// using the current local time for timestamp mode (`§4.2`).
///
/// Local time falls back to UTC when the offset cannot be determined
/// (`§4.3`); `OffsetDateTime::now_local` refuses on some multi-threaded Unix
/// builds, so this is not a rare case.
#[must_use]
pub fn apply_iteration(content: &str, spec: &IterationSpec, index: usize) -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    apply_iteration_at(content, spec, index, now)
}

/// As [`apply_iteration`] with the clock supplied, so tests are exact.
///
/// Offsets are characters, not bytes (`§4.1`); an out-of-range or inverted
/// range returns `content` untouched rather than failing, because a stale
/// range in a hand-edited project file should not stop a test run.
#[must_use]
pub fn apply_iteration_at(
    content: &str,
    spec: &IterationSpec,
    index: usize,
    now: OffsetDateTime,
) -> String {
    let chars: Vec<char> = content.chars().collect();
    if !(spec.start < spec.end && spec.end <= chars.len()) {
        return content.to_owned();
    }
    let original: String = chars[spec.start..spec.end].iter().collect();
    let replacement = compute_value(&original, spec, index, now);
    let mut out: String = chars[..spec.start].iter().collect();
    out.push_str(&replacement);
    out.extend(&chars[spec.end..]);
    out
}

/// The replacement text for `original` (`§4.2`).
///
/// Increment arithmetic is done in `i128` with saturation so a hostile
/// `step` in a hand-edited project file cannot panic on overflow (debug
/// builds would abort). A negative result is not padded, because a leading
/// zero before the sign would be meaningless. A non-integer original is
/// returned unchanged, as the Python does.
fn compute_value(
    original: &str,
    spec: &IterationSpec,
    index: usize,
    now: OffsetDateTime,
) -> String {
    match spec.mode {
        IterationMode::Increment => {
            let Ok(base) = original.trim().parse::<i128>() else {
                return original.to_owned();
            };
            let offset = i128::from(spec.step).saturating_mul(i128::try_from(index).unwrap_or(0));
            let value = base.saturating_add(offset);
            let width = if spec.pad_width == 0 {
                original.chars().count()
            } else {
                spec.pad_width
            };
            if value >= 0 {
                format!("{value:0>width$}")
            } else {
                value.to_string()
            }
        }
        IterationMode::List => {
            if spec.values.is_empty() {
                original.to_owned()
            } else {
                spec.values[index % spec.values.len()].clone()
            }
        }
        IterationMode::Timestamp => strftime(&spec.timestamp_format, now),
    }
}

/// The `strftime` subset of `§4.3`: `%Y %y %m %d %H %M %S %f %j %%`.
/// Anything else after a `%` is copied through, so a typo shows in the
/// output instead of vanishing. A trailing lone `%` is kept.
///
/// A hand-rolled subset rather than the `time` format descriptions: the
/// project file stores the Python's `strftime` strings, which `time` cannot
/// parse.
///
/// # Examples
/// ```
/// use time::macros::datetime;
/// use ultra7::iteration::strftime;
/// let t = datetime!(2001-02-03 04:05:06 UTC);
/// assert_eq!(strftime("%Y%m%d%H%M%S", t), "20010203040506");
/// assert_eq!(strftime("%j 100%% %q", t), "034 100% %q");
/// ```
#[must_use]
pub fn strftime(format: &str, t: OffsetDateTime) -> String {
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(spec) = chars.next() else {
            out.push('%');
            break;
        };
        let _ = match spec {
            'Y' => write!(out, "{:04}", t.year()),
            'y' => write!(out, "{:02}", t.year().rem_euclid(100)),
            'm' => write!(out, "{:02}", u8::from(t.month())),
            'd' => write!(out, "{:02}", t.day()),
            'H' => write!(out, "{:02}", t.hour()),
            'M' => write!(out, "{:02}", t.minute()),
            'S' => write!(out, "{:02}", t.second()),
            'f' => write!(out, "{:06}", t.microsecond()),
            'j' => write!(out, "{:03}", t.ordinal()),
            // `%%` and an unknown `%x` both keep the `%` (`§4.3`); the
            // unknown one then shows its letter, so a typo is visible.
            '%' => write!(out, "%"),
            other => write!(out, "%{other}"),
        };
    }
    out
}
