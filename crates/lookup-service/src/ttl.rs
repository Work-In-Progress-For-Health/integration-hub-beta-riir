//! TTL unit conversion for the forms; the stored form is always whole
//! seconds.
//!
//! Implements `spec/20-lookup-service.md` §9.4.

use crate::error::LookupError;
use crate::model::MAX_TTL_SECONDS;

/// The longest TTL in whole days (`RCA-EXC-1`: a constant expression).
#[expect(clippy::integer_division, reason = "RCA-EXC-1: constant expression")]
const MAX_TTL_DAYS: u32 = MAX_TTL_SECONDS / 86_400;

/// Largest first, so [`split_ttl`] picks the most readable exact unit.
pub const TTL_UNITS: [(&str, u32); 4] = [
    ("days", 86_400),
    ("hours", 3600),
    ("minutes", 60),
    ("seconds", 1),
];

/// The unit names, smallest first, as the forms list them.
#[must_use]
pub fn unit_names() -> Vec<&'static str> {
    TTL_UNITS.iter().rev().map(|(name, _)| *name).collect()
}

/// Convert a value in a unit to seconds.
///
/// # Errors
///
/// `invalid_settings` for an unknown unit, a value below 1 or a result over
/// seven days.
pub fn ttl_to_seconds(value: i64, unit: &str, table: Option<&str>) -> Result<u32, LookupError> {
    let Some((_, size)) = TTL_UNITS.iter().find(|(name, _)| *name == unit) else {
        return Err(LookupError::invalid_settings(
            format!("TTL unit must be one of {}", unit_names().join(", ")),
            table,
        ));
    };
    // An overflowing product is out of range, so it is the same error as any
    // other value over the limit (RCA-IN-2), not a clamped one.
    let seconds = value
        .checked_mul(i64::from(*size))
        .filter(|seconds| value >= 1 && *seconds <= i64::from(MAX_TTL_SECONDS));
    let Some(seconds) = seconds else {
        return Err(LookupError::invalid_settings(
            format!("TTL must be between 1 second and {MAX_TTL_DAYS} days"),
            table,
        ));
    };
    u32::try_from(seconds).map_err(|_| {
        LookupError::invalid_settings("TTL must be between 1 second and 7 days", table)
    })
}

/// The largest unit that divides `seconds` exactly, e.g. 7200 as
/// `(2, "hours")`.
#[must_use]
pub fn split_ttl(seconds: u32) -> (u32, &'static str) {
    for (name, size) in TTL_UNITS {
        if seconds.is_multiple_of(size)
            && let Some(value) = seconds.checked_div(size)
        {
            return (value, name);
        }
    }
    (seconds, "seconds")
}

/// `5 minutes`, `1 day`.
#[must_use]
pub fn format_ttl(seconds: u32) -> String {
    let (value, unit) = split_ttl(seconds);
    let unit = if value == 1 {
        unit.trim_end_matches('s')
    } else {
        unit
    };
    format!("{value} {unit}")
}
