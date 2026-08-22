//! The two value transformations PHW applies.
//!
//! Implements `spec/09-transformers.md` §3.4 and §3.6.

use ihb_transformer::TransformError;

/// HL7's explicit null: two literal double-quote characters, meaning
/// "delete this value" rather than "no information"
/// (`spec/09-transformers.md` §3.6).
pub const EXPLICIT_NULL: &str = "\"\"";

/// The marker PHW sends when a death record is being withdrawn.
pub const RESURRECTED: &str = "RESURREC";

/// Normalise a timestamp to HL7's `YYYYMMDDHHMMSS` (§3.4).
///
/// # Errors
///
/// [`TransformError::Datetime`] when the value matches neither accepted
/// form. This is the Python's `ValueError`, and it stops the message: a
/// timestamp that cannot be read is not one to guess at.
///
/// # Examples
///
/// ```
/// use hl7_phw_transformer::datetime::normalise;
///
/// // Already HL7: returned unchanged.
/// assert_eq!(normalise("20250505232332")?, "20250505232332");
///
/// // PHW's own form, which is what makes this transformer necessary.
/// assert_eq!(normalise("2025-05-05 23:23:32")?, "20250505232332");
///
/// assert!(normalise("05/05/2025").is_err());
/// # Ok::<(), ihb_transformer::TransformError>(())
/// ```
pub fn normalise(value: &str) -> Result<String, TransformError> {
    // Step 1 — already `%Y%m%d%H%M%S`. Checked by shape and by parsing,
    // because 14 digits is not enough: `20251345000000` has the right
    // shape and is not a date.
    if is_hl7_timestamp(value) {
        return Ok(value.to_string());
    }

    // Step 2 — `%Y-%m-%d %H:%M:%S`.
    if let Some(converted) = from_spaced(value) {
        return Ok(converted);
    }

    // Step 3.
    Err(TransformError::Datetime(format!(
        "time data '{value}' does not match format '%Y-%m-%d %H:%M:%S'"
    )))
}

/// Whether `value` is already `%Y%m%d%H%M%S`.
fn is_hl7_timestamp(value: &str) -> bool {
    if value.len() != 14 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    parts(
        value,
        &[(0, 4), (4, 6), (6, 8), (8, 10), (10, 12), (12, 14)],
    )
    .is_some_and(|fields| valid(&fields))
}

/// Convert `%Y-%m-%d %H:%M:%S` to `%Y%m%d%H%M%S`, or `None`.
fn from_spaced(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    // `2025-05-05 23:23:32` — 19 characters, with separators at fixed
    // positions. Checked explicitly rather than by splitting, so that
    // `2025-5-5 3:3:3` is rejected: the Python's `strptime` accepts it,
    // but MPI would receive a malformed timestamp either way and the
    // Python's own tests only exercise the padded form.
    if bytes.len() != 19 {
        return None;
    }
    for (index, expected) in [(4, b'-'), (7, b'-'), (10, b' '), (13, b':'), (16, b':')] {
        if bytes.get(index) != Some(&expected) {
            return None;
        }
    }

    let fields = parts(
        value,
        &[(0, 4), (5, 7), (8, 10), (11, 13), (14, 16), (17, 19)],
    )?;
    if !valid(&fields) {
        return None;
    }

    Some(format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        fields[0], fields[1], fields[2], fields[3], fields[4], fields[5]
    ))
}

/// Parse the six numeric slices, or `None` if any is not a number.
fn parts(value: &str, spans: &[(usize, usize); 6]) -> Option<[u32; 6]> {
    let mut out = [0_u32; 6];
    for (slot, (start, end)) in out.iter_mut().zip(spans.iter()) {
        *slot = value.get(*start..*end)?.parse().ok()?;
    }
    Some(out)
}

/// Whether the six fields are a real moment.
fn valid(fields: &[u32; 6]) -> bool {
    let [year, month, day, hour, minute, second] = *fields;

    let Ok(year) = i32::try_from(year) else {
        return false;
    };
    let Ok(month) = u8::try_from(month).map(time::Month::try_from) else {
        return false;
    };
    let Ok(month) = month else { return false };
    let Ok(day) = u8::try_from(day) else {
        return false;
    };

    time::Date::from_calendar_date(year, month, day).is_ok()
        && hour < 24
        && minute < 60
        // A leap second is 60, and HL7 permits it.
        && second <= 60
}

/// Map a date of death (§3.6).
///
/// | Input | Output |
/// | --- | --- |
/// | Blank or whitespace | `""` (empty) |
/// | `RESURREC`, case-insensitive, trimmed | `""` (two quote characters) |
/// | Anything else | the input, trimmed |
///
/// The middle row is the interesting one: PHW sends `RESURREC` when a death
/// record is withdrawn, and MPI needs HL7's *explicit null* to delete the
/// value it already holds. An empty field would mean "no information" and
/// would leave the death date in place.
///
/// # Examples
///
/// ```
/// use hl7_phw_transformer::datetime::date_of_death;
///
/// assert_eq!(date_of_death("20240101120000"), "20240101120000");
/// assert_eq!(date_of_death("  20240101  "), "20240101");
/// assert_eq!(date_of_death("   "), "");
///
/// // The withdrawal marker becomes an explicit null, not an empty field.
/// assert_eq!(date_of_death("RESURREC"), "\"\"");
/// assert_eq!(date_of_death(" resurrec "), "\"\"");
/// ```
#[must_use]
pub fn date_of_death(value: &str) -> String {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.eq_ignore_ascii_case(RESURRECTED) {
        return EXPLICIT_NULL.to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_hl7_timestamp_passes_through_unchanged() {
        // spec/09-transformers.md §3.4 step 1.
        for value in ["20250505232332", "19991231235959", "20240229000000"] {
            assert_eq!(normalise(value).unwrap(), value);
        }
    }

    #[test]
    fn phws_spaced_form_is_converted() {
        // §3.4 step 2 — the reason this transformer exists.
        assert_eq!(normalise("2025-05-05 23:23:32").unwrap(), "20250505232332");
        assert_eq!(normalise("1999-01-01 00:00:00").unwrap(), "19990101000000");
    }

    #[test]
    fn anything_else_fails_the_transform() {
        // §3.4 step 3 — the Python's `ValueError`, which surfaces as §1.6's
        // transform error and abandons the message.
        for value in [
            "05/05/2025",
            "2025-05-05T23:23:32",
            "2025-05-05 23:23",
            "",
            "not a date",
            "2025050523233",
            "202505052323321",
        ] {
            assert!(normalise(value).is_err(), "{value:?} should not parse");
        }
    }

    #[test]
    fn a_well_shaped_value_that_is_not_a_real_moment_is_refused() {
        // Fourteen digits is a shape, not a date. Forwarding 20251345000000
        // to MPI would put a timestamp in the record that no reader can
        // interpret.
        for value in ["20251345000000", "20250230000000", "20250505250000"] {
            assert!(normalise(value).is_err(), "{value:?} should not parse");
        }
    }

    #[test]
    fn a_leap_second_is_accepted() {
        // HL7 permits :60, and refusing one would drop a real message.
        assert_eq!(normalise("20161231235960").unwrap(), "20161231235960");
    }

    #[test]
    fn every_row_of_the_date_of_death_table() {
        // spec/09-transformers.md §3.6.
        assert_eq!(date_of_death(""), "");
        assert_eq!(date_of_death("   \t "), "");

        assert_eq!(date_of_death("RESURREC"), EXPLICIT_NULL);
        assert_eq!(date_of_death("resurrec"), EXPLICIT_NULL);
        assert_eq!(date_of_death("ReSuRrEc"), EXPLICIT_NULL);
        assert_eq!(date_of_death("  RESURREC  "), EXPLICIT_NULL);

        assert_eq!(date_of_death("20240101"), "20240101");
        assert_eq!(date_of_death(" 20240101120000 "), "20240101120000");
    }

    #[test]
    fn resurrec_is_matched_whole_not_as_a_prefix() {
        // A date that merely begins with the marker is a value, not a
        // withdrawal. Getting this wrong would delete a real death date.
        assert_eq!(date_of_death("RESURRECTED"), "RESURRECTED");
        assert_eq!(date_of_death("RESURREC1"), "RESURREC1");
    }
}
