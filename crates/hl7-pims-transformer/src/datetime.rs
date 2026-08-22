//! Timezone stripping.
//!
//! Implements `spec/09-transformers.md` §5.6.

use ihb_transformer::TransformError;

/// HL7's explicit null: two literal double-quote characters
/// (`spec/09-transformers.md` §5.3.3).
pub const EXPLICIT_NULL: &str = "\"\"";

/// Strip a `+` timezone offset, and check what is left is a date or a
/// timestamp.
///
/// 1. Blank or whitespace returns `""`.
/// 2. Everything before the first `+`, trimmed.
/// 3. The result MUST be all digits, and exactly 8 or 14 characters.
///
/// # Errors
///
/// [`TransformError::Datetime`] with §5.6's exact text. The message is
/// abandoned rather than forwarded: a timestamp MPI cannot read is worse
/// than a message it never received, because the first is silent.
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::datetime::strip_timezone;
///
/// assert_eq!(strip_timezone("20250702085450+0000")?, "20250702085450");
/// assert_eq!(strip_timezone("20250702")?, "20250702");
/// assert_eq!(strip_timezone("  ")?, "");
///
/// // PIMS sends the date of birth with a bare `+` and a degree indicator.
/// assert_eq!(strip_timezone("20000101+^D")?, "20000101");
/// # Ok::<(), ihb_transformer::TransformError>(())
/// ```
///
/// Only `+` offsets are handled (§5.6.1), matching the Python and the PIMS
/// feed, which only ever sends `+`. A `-` offset is not stripped and fails:
///
/// ```
/// use hl7_pims_transformer::datetime::strip_timezone;
///
/// assert!(strip_timezone("20250702085450-0500").is_err());
/// ```
pub fn strip_timezone(value: &str) -> Result<String, TransformError> {
    // Step 1.
    if value.trim().is_empty() {
        return Ok(String::new());
    }

    // Step 2.
    let stripped = value.split('+').next().unwrap_or("").trim();

    // Step 3.
    if (stripped.len() == 8 || stripped.len() == 14)
        && stripped.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok(stripped.to_string());
    }

    // The wording is normative — §5.6, and the PIMS support team matches on
    // it. Note it quotes the value *after* stripping, not the original.
    Err(TransformError::Datetime(format!(
        "Invalid datetime format after timezone removal: '{stripped}'. \
         Expected format: YYYYMMDD (8 digits) or YYYYMMDDHHMMSS (14 digits)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_returns_empty_rather_than_failing() {
        // §5.6 step 1. A field PIMS did not send is not an error.
        for value in ["", " ", "\t", "   \r"] {
            assert_eq!(strip_timezone(value).unwrap(), "");
        }
    }

    #[test]
    fn an_offset_is_removed_and_both_widths_are_accepted() {
        // §5.6 steps 2 and 3.
        assert_eq!(
            strip_timezone("20250702085450+0000").unwrap(),
            "20250702085450"
        );
        assert_eq!(strip_timezone("20250702+0100").unwrap(), "20250702");
        assert_eq!(strip_timezone("20250702085450").unwrap(), "20250702085450");
        assert_eq!(strip_timezone("20250702").unwrap(), "20250702");
        assert_eq!(strip_timezone("  20250702  ").unwrap(), "20250702");
    }

    #[test]
    fn the_failure_text_is_the_specified_one() {
        // §5.6 step 3 — normative, and it quotes the value *after*
        // stripping rather than the original.
        let error = strip_timezone("2025070").unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid datetime format after timezone removal: '2025070'. \
             Expected format: YYYYMMDD (8 digits) or YYYYMMDDHHMMSS (14 digits)"
        );
    }

    #[test]
    fn a_minus_offset_is_not_handled() {
        // §5.6.1 — deliberately. The PIMS feed only ever sends `+`, and
        // inventing `-` handling would be untested behaviour on a path that
        // touches every message.
        let error = strip_timezone("20250702085450-0500").unwrap_err();
        assert!(error.to_string().contains("-0500"), "{error}");
    }

    #[test]
    fn a_wrong_width_or_a_non_digit_fails() {
        for value in [
            "2025",
            "202507020854501",
            "2025-07-02",
            "abcdefgh",
            "2025070208545O",
        ] {
            assert!(strip_timezone(value).is_err(), "{value:?} should fail");
        }
    }
}
