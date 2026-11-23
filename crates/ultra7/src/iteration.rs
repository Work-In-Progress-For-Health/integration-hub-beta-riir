//! The iteration field: recompute a highlighted range per send.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§4`.

use std::fmt::Write as _;

use time::OffsetDateTime;

use crate::model::{IterationMode, IterationSpec};

/// Why an iteration could not be applied. Carries no content: a message under
/// test can hold anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IterationError {
    /// The range is empty, inverted, or runs past the message (`§4.1`).
    RangeOutOfBounds,
    /// Increment mode, and the text in the range is not an integer.
    NotAnInteger,
    /// Increment mode, and the new value does not fit (`§4.2`).
    ValueOutOfRange,
    /// List mode, and there are no values to cycle through.
    NoValues,
}

impl std::fmt::Display for IterationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RangeOutOfBounds => "the iteration range is outside the message",
            Self::NotAnInteger => "the iteration range does not hold an integer",
            Self::ValueOutOfRange => "the incremented value is out of range",
            Self::NoValues => "the iteration list has no values",
        })
    }
}

impl std::error::Error for IterationError {}

/// `content` with the spec's range recomputed for send `index` (0-based),
/// using the current local time for timestamp mode (`§4.2`).
///
/// Local time falls back to UTC when the offset cannot be determined
/// (`§4.3`); `OffsetDateTime::now_local` refuses on some multi-threaded Unix
/// builds, so this is not a rare case.
///
/// # Errors
///
/// [`IterationError`], as [`apply_iteration_at`].
pub fn apply_iteration(
    content: &str,
    spec: &IterationSpec,
    index: usize,
) -> Result<String, IterationError> {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    apply_iteration_at(content, spec, index, now)
}

/// As [`apply_iteration`] with the clock supplied, so tests are exact.
///
/// Offsets are characters, not bytes (`§4.1`).
///
/// # Errors
///
/// [`IterationError`] for an out-of-range or inverted range, an increment
/// whose text is not an integer or whose result does not fit, and a list with
/// no values. None of them returns the content unchanged: a stale range in a
/// hand-edited project file is refused, so a run does not send a message that
/// is not the one the project describes (`rust-checked-arithmetic` RCA-DECL-6,
/// RCA-IN-2; `spec/ultra7-test-message-sender.md` §4.2).
pub fn apply_iteration_at(
    content: &str,
    spec: &IterationSpec,
    index: usize,
    now: OffsetDateTime,
) -> Result<String, IterationError> {
    let chars: Vec<char> = content.chars().collect();
    let (true, Some(before), Some(selected), Some(after)) = (
        spec.start < spec.end,
        chars.get(..spec.start),
        chars.get(spec.start..spec.end),
        chars.get(spec.end..),
    ) else {
        return Err(IterationError::RangeOutOfBounds);
    };
    let original: String = selected.iter().collect();
    let replacement = compute_value(&original, spec, index, now)?;
    let mut out: String = before.iter().collect();
    out.push_str(&replacement);
    out.extend(after);
    Ok(out)
}

/// The replacement text for `original` (`§4.2`).
///
/// Increment arithmetic is done in `i128`, checked, so a hostile `step` in a
/// hand-edited project file cannot panic on overflow; a result that does not
/// fit is an error. A negative result is not padded, because a leading zero
/// before the sign would be meaningless.
fn compute_value(
    original: &str,
    spec: &IterationSpec,
    index: usize,
    now: OffsetDateTime,
) -> Result<String, IterationError> {
    match spec.mode {
        IterationMode::Increment => {
            let base = original
                .trim()
                .parse::<i128>()
                .map_err(|_| IterationError::NotAnInteger)?;
            let value = i128::try_from(index)
                .ok()
                .and_then(|index| i128::from(spec.step).checked_mul(index))
                .and_then(|offset| base.checked_add(offset))
                .ok_or(IterationError::ValueOutOfRange)?;
            let width = if spec.pad_width == 0 {
                original.chars().count()
            } else {
                spec.pad_width
            };
            Ok(if value >= 0 {
                format!("{value:0>width$}")
            } else {
                value.to_string()
            })
        }
        // An empty list has no remainder and so no value.
        IterationMode::List => index
            .checked_rem(spec.values.len())
            .and_then(|slot| spec.values.get(slot))
            .cloned()
            .ok_or(IterationError::NoValues),
        IterationMode::Timestamp => Ok(strftime(&spec.timestamp_format, now)),
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
