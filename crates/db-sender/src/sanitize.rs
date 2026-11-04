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
    let step = redact_parenthesised(message, "the duplicate key value is");
    let step = redact_quoted(&step, "truncated value:");
    redact_conversion_value(&step)
}

/// `needle` (case-insensitive), optional whitespace, `(`..`)` becomes `(***)`.
fn redact_parenthesised(text: &str, needle: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(needle) {
        let after = at + found + needle.len();
        let rest = &text[after..];
        let ws = rest.len() - rest.trim_start().len();
        // `[^)]*` then `)`: no closing bracket means no match, as the regex.
        if rest[ws..].starts_with('(')
            && let Some(close) = rest[ws + 1..].find(')')
        {
            out.push_str(&text[at..=after + ws]);
            out.push_str(MASK);
            at = after + ws + 1 + close;
            continue;
        }
        out.push_str(&text[at..after]);
        at = after;
    }
    out.push_str(&text[at..]);
    out
}

/// `needle` (case-insensitive), optional whitespace, then a single-quoted
/// string (`''` is an escaped quote) becomes `'***'`.
fn redact_quoted(text: &str, needle: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(needle) {
        let after = at + found + needle.len();
        let ws = text[after..].len() - text[after..].trim_start().len();
        if let Some(end) = quoted_end(&text[after + ws..]) {
            out.push_str(&text[at..after + ws]);
            out.push('\'');
            out.push_str(MASK);
            out.push('\'');
            at = after + ws + end;
        } else {
            out.push_str(&text[at..after]);
            at = after;
        }
    }
    out.push_str(&text[at..]);
    out
}

/// `converting [the] [type] value '..'`: the quoted text becomes `'***'`.
///
/// The Python regex was
/// `converting(?:\s+the)?(?:\s+\w+)?\s+value\s+'(?:''|[^'])*'`. Rather than
/// emulate its backtracking, this accepts what it accepted: after
/// `converting`, up to two words (`the`, then a type name) and then the word
/// `value`.
fn redact_conversion_value(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find("converting") {
        let start = at + found + "converting".len();
        if let Some(quote_at) = conversion_quote(&text[start..], &lower[start..]) {
            let quote = start + quote_at;
            if let Some(end) = quoted_end(&text[quote..]) {
                out.push_str(&text[at..quote]);
                out.push('\'');
                out.push_str(MASK);
                out.push('\'');
                at = quote + end;
                continue;
            }
        }
        out.push_str(&text[at..start]);
        at = start;
    }
    out.push_str(&text[at..]);
    out
}

/// The offset of the opening quote of a `converting ... value '` clause,
/// given the text after `converting`.
///
/// Accepted word sequences before `value` (the regex's two optional groups):
/// none, one word, or `the` followed by one word.
fn conversion_quote(text: &str, lower: &str) -> Option<usize> {
    let mut cursor = 0;
    let mut words: Vec<&str> = Vec::new();
    loop {
        let rest = &lower[cursor..];
        let ws = rest.len() - rest.trim_start().len();
        // Every word is preceded by whitespace (`\s+`).
        if ws == 0 {
            return None;
        }
        let word_start = cursor + ws;
        let word_len = lower[word_start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(lower.len() - word_start);
        if word_len == 0 {
            return None;
        }
        let word = &lower[word_start..word_start + word_len];
        cursor = word_start + word_len;

        if word == "value" {
            let valid = match words.as_slice() {
                [] | [_] => true,
                [first, _] => *first == "the",
                _ => false,
            };
            let after = &text[cursor..];
            let gap = after.len() - after.trim_start().len();
            return (valid && gap > 0 && after[gap..].starts_with('\'')).then_some(cursor + gap);
        }
        words.push(word);
        if words.len() > 2 {
            return None;
        }
    }
}

/// The byte length of the single-quoted string at the start of `text`,
/// including both quotes, or `None` when it is unterminated.
fn quoted_end(text: &str) -> Option<usize> {
    let rest = text.strip_prefix('\'')?;
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return Some(i + 2);
        }
        i += 1;
    }
    None
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
}
