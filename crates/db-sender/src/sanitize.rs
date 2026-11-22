//! Redacting values SQL Server echoes into its own messages.
//!
//! Implements `spec/10-senders.md` §7.4. Upstream:
//! `db_client.py :: sanitize_error_message` (Caldicott principle 3).
//!
//! Some server errors quote the offending parameter: a conversion error
//! (`245`) quotes the value, a duplicate key (`2627`/`2601`) quotes the key,
//! a truncation (`2628`) quotes the data. Here the parameter is a whole
//! clinical message, so an unsanitised message would carry a patient
//! identifier into a dead-letter description and a log line.
//!
//! Written by hand rather than with a regex crate: the workspace is
//! deliberately thin (`agents/workspace.md`), the three patterns are small,
//! and a hand scanner has no backtracking to go quadratic on a hostile
//! message.

/// What replaces a redacted value.
const MASK: &str = "***";

/// Redact echoed values from a server message (`§7.4`).
///
/// If a split cannot be proved to land inside the text, the **whole message**
/// is masked: a redactor that cannot tell what it has cut must not show the
/// rest (`§7.4`, RCA-P-9). That branch is unreachable for the inputs the
/// scanners cut at (each offset comes from a search that found it), so it
/// costs nothing in practice and cannot leak.
///
/// # Examples
///
/// ```
/// use db_sender::sanitize::sanitize_error_message;
///
/// assert_eq!(
///     sanitize_error_message("Conversion failed when converting the varchar value '9999999999' to data type int."),
///     "Conversion failed when converting the varchar value '***' to data type int.",
/// );
/// assert_eq!(
///     sanitize_error_message("The duplicate key value is (9999999999, DOE)."),
///     "The duplicate key value is (***).",
/// );
/// assert_eq!(sanitize_error_message(""), "");
/// ```
#[must_use]
pub fn sanitize_error_message(message: &str) -> String {
    redact(message).unwrap_or_else(|Unsplittable| MASK.to_string())
}

/// The three redactions in turn, or `Unsplittable` if any cut failed.
fn redact(message: &str) -> Result<String, Unsplittable> {
    let step = redact_parenthesised(message, "the duplicate key value is")?;
    let step = redact_quoted(&step, "truncated value:")?;
    redact_conversion_value(&step)
}

/// A split that did not land on a character boundary inside the text.
struct Unsplittable;

/// `text` cut at byte `at`.
fn cut(text: &str, at: usize) -> Result<(&str, &str), Unsplittable> {
    text.split_at_checked(at).ok_or(Unsplittable)
}

/// The part of `whole` before its suffix `suffix` (`RCA-INT-1`: a checked
/// subtraction, not `len() - len()`).
fn before<'a>(whole: &'a str, suffix: &str) -> Result<&'a str, Unsplittable> {
    let length = whole.len().checked_sub(suffix.len()).ok_or(Unsplittable)?;
    Ok(cut(whole, length)?.0)
}

/// The byte offset of the ASCII `needle` in `haystack`, ignoring ASCII case.
/// A match of ASCII bytes cannot start inside a multi-byte character.
fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return None;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// `needle` (case-insensitive), optional whitespace, `(`..`)` becomes `(***)`.
fn redact_parenthesised(text: &str, needle: &str) -> Result<String, Unsplittable> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = find_ignoring_case(rest, needle) {
        let (head, from_needle) = cut(rest, found)?;
        let (matched, tail) = cut(from_needle, needle.len())?;
        let after_ws = tail.trim_start();
        out.push_str(head);
        out.push_str(matched);
        // `[^)]*` then `)`: no closing bracket means no match, as the regex.
        if let Some(inside) = after_ws.strip_prefix('(')
            && let Some(close) = inside.find(')')
        {
            out.push_str(before(tail, after_ws)?);
            out.push('(');
            out.push_str(MASK);
            rest = cut(inside, close)?.1;
            continue;
        }
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
}

/// `needle` (case-insensitive), optional whitespace, then a single-quoted
/// string (`''` is an escaped quote) becomes `'***'`.
fn redact_quoted(text: &str, needle: &str) -> Result<String, Unsplittable> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = find_ignoring_case(rest, needle) {
        let (head, from_needle) = cut(rest, found)?;
        let (matched, tail) = cut(from_needle, needle.len())?;
        let after_ws = tail.trim_start();
        out.push_str(head);
        out.push_str(matched);
        if let Some(after) = after_quoted(after_ws) {
            out.push_str(before(tail, after_ws)?);
            push_masked_quote(&mut out);
            rest = after;
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// `'***'`.
fn push_masked_quote(out: &mut String) {
    out.push('\'');
    out.push_str(MASK);
    out.push('\'');
}

/// `converting [the] [type] value '..'`: the quoted text becomes `'***'`.
///
/// The Python regex was
/// `converting(?:\s+the)?(?:\s+\w+)?\s+value\s+'(?:''|[^'])*'`. Rather than
/// emulate its backtracking, this accepts what it accepted: after
/// `converting`, up to two words (`the`, then a type name) and then the word
/// `value`.
fn redact_conversion_value(text: &str) -> Result<String, Unsplittable> {
    const NEEDLE: &str = "converting";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = find_ignoring_case(rest, NEEDLE) {
        let (head, from_needle) = cut(rest, found)?;
        let (matched, tail) = cut(from_needle, NEEDLE.len())?;
        out.push_str(head);
        out.push_str(matched);
        if let Some(from_quote) = conversion_quote(tail)?
            && let Some(after) = after_quoted(from_quote)
        {
            out.push_str(before(tail, from_quote)?);
            push_masked_quote(&mut out);
            rest = after;
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// The text starting at the opening quote of a `converting ... value '`
/// clause, given the text after `converting`.
///
/// Accepted word sequences before `value` (the regex's two optional groups):
/// none, one word, or `the` followed by one word.
fn conversion_quote(text: &str) -> Result<Option<&str>, Unsplittable> {
    let mut rest = text;
    let mut words: Vec<&str> = Vec::new();
    loop {
        let trimmed = rest.trim_start();
        // Every word is preceded by whitespace (`\s+`).
        if trimmed.len() == rest.len() {
            return Ok(None);
        }
        let word_length = trimmed
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(trimmed.len());
        if word_length == 0 {
            return Ok(None);
        }
        let (word, after) = cut(trimmed, word_length)?;
        if word.eq_ignore_ascii_case("value") {
            let valid = match words.as_slice() {
                [] | [_] => true,
                [first, _] => first.eq_ignore_ascii_case("the"),
                _ => false,
            };
            let at_quote = after.trim_start();
            let spaced = at_quote.len() != after.len();
            return Ok((valid && spaced && at_quote.starts_with('\'')).then_some(at_quote));
        }
        words.push(word);
        if words.len() > 2 {
            return Ok(None);
        }
        rest = after;
    }
}

/// What follows the single-quoted string at the start of `text` (both quotes
/// consumed; `''` inside is an escaped quote), or `None` when `text` does not
/// start with one or it is unterminated.
fn after_quoted(text: &str) -> Option<&str> {
    let mut rest = text.strip_prefix('\'')?;
    loop {
        let (_, from_quote) = rest.split_at_checked(rest.find('\'')?)?;
        let after = from_quote.strip_prefix('\'')?;
        match after.strip_prefix('\'') {
            Some(escaped) => rest = escaped,
            None => return Some(after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversion_failure_is_masked() {
        // §7.4 (upstream test_sanitize_error_message_conversion_failure).
        let raw = "[22018] [Microsoft][ODBC Driver 18 for SQL Server][SQL Server]Conversion failed \
                   when converting the varchar value '9999999999' to data type int.";
        let clean = sanitize_error_message(raw);
        assert!(!clean.contains("9999999999"), "{clean}");
        assert!(
            clean.contains("converting the varchar value '***' to data type int."),
            "{clean}"
        );
    }

    #[test]
    fn a_duplicate_key_is_masked() {
        let raw = "Violation of PRIMARY KEY constraint 'PK_Msg'. Cannot insert duplicate key in object \
                   'dbo.Messages'. The duplicate key value is (9999999999, DOE, 1980-01-01).";
        let clean = sanitize_error_message(raw);
        assert!(
            !clean.contains("DOE") && !clean.contains("9999999999"),
            "{clean}"
        );
        assert!(
            clean.contains("The duplicate key value is (***)."),
            "{clean}"
        );
        assert!(
            clean.contains("'PK_Msg'"),
            "text outside the value is kept: {clean}"
        );
    }

    #[test]
    fn a_truncation_is_masked() {
        let raw = "String or binary data would be truncated in table 'DRS.dbo.Messages', column \
                   'Payload'. Truncated value: '<ADT_A39><PID><PID.5>PATIENT_SECRET_DATA</PID.5></PID></ADT_A39>'.";
        let clean = sanitize_error_message(raw);
        assert!(!clean.contains("PATIENT_SECRET_DATA"), "{clean}");
        assert!(clean.contains("Truncated value: '***'."), "{clean}");
    }

    #[test]
    fn a_doubled_quote_does_not_end_the_value_early() {
        let clean =
            sanitize_error_message("converting the varchar value 'O''BRIEN-123' to data type int");
        assert_eq!(clean, "converting the varchar value '***' to data type int");
    }

    #[test]
    fn the_optional_words_are_optional() {
        assert_eq!(
            sanitize_error_message("when converting varchar value 'S3CRET' to int"),
            "when converting varchar value '***' to int"
        );
        assert_eq!(
            sanitize_error_message("when converting value 'S3CRET' to int"),
            "when converting value '***' to int"
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        let clean = sanitize_error_message("THE DUPLICATE KEY VALUE IS (SECRET).");
        assert_eq!(clean, "THE DUPLICATE KEY VALUE IS (***).");
    }

    #[test]
    fn an_unterminated_value_is_left_alone() {
        // As the regex: no closing quote, no match.
        let raw = "converting the varchar value 'unterminated";
        assert_eq!(sanitize_error_message(raw), raw);
    }

    #[test]
    fn a_message_with_nothing_to_hide_is_unchanged() {
        let raw = "Could not find stored procedure 'dbo.prQueueMessage'.";
        assert_eq!(sanitize_error_message(raw), raw);
        assert_eq!(sanitize_error_message(""), "");
    }

    #[test]
    fn non_ascii_text_does_not_panic() {
        // Byte offsets come from an ASCII-lowercased copy, which preserves them.
        let raw = "Für 日本 converting the varchar value 'ä' to int, truncated value: 'ö'";
        let clean = sanitize_error_message(raw);
        assert!(!clean.contains('ä') && !clean.contains('ö'), "{clean}");
    }

    #[test]
    fn whitespace_before_the_bracket_is_kept_and_an_open_bracket_is_left_alone() {
        // `§7.4`: the regex allowed `\s*` before `(` and needed a `)`.
        assert_eq!(
            sanitize_error_message("the duplicate key value is \t (SECRET) end"),
            "the duplicate key value is \t (***) end"
        );
        let open = "the duplicate key value is (SECRET never closed";
        assert_eq!(sanitize_error_message(open), open);
    }

    #[test]
    fn every_occurrence_is_masked_not_just_the_first() {
        let clean = sanitize_error_message(
            "converting varchar value 'A1' x converting the int value 'B2' y truncated value: 'C3'",
        );
        assert!(
            !clean.contains("A1") && !clean.contains("B2") && !clean.contains("C3"),
            "{clean}"
        );
        assert_eq!(clean.matches("'***'").count(), 3, "{clean}");
    }
}
