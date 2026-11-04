//! The ACK and NACK strings returned to the REST caller.
//!
//! Implements `spec/19-rest-servers.md` §4.
//!
//! Not `hl7-server`'s `ack` module, though it looks like it: that one answers
//! an MLLP peer and joins segments with `\r`; this one answers an HTTP
//! caller whose contract (fixed by `hl7_ack_builder.py`) joins them with
//! `\r\n`, always identifies the Hub as `DHCW` / `cymru.nhs.uk`, and falls
//! back to `2.5.1` rather than `2.5`. Sharing code would mean a parameter
//! for each of those, and a day when one service's change broke the other's
//! callers.

use hl7_2::Message;
use ihb_fields::read;

/// `MSA-1` for an accepted message.
pub const ACCEPT: &str = "AA";

/// `MSA-1`: the message is invalid; do not send it again.
pub const REJECT: &str = "AR";

/// `MSA-1`: the Hub failed; send it again.
pub const ERROR: &str = "AE";

/// `MSH-3` of every ACK: the Hub identifies itself.
const SENDING_APPLICATION: &str = "DHCW";

/// `MSH-4` of every ACK.
const SENDING_FACILITY: &str = "cymru.nhs.uk";

/// `MSH-12` when the inbound message did not say.
const DEFAULT_VERSION: &str = "2.5.1";

/// `MSA-3` of an `AA`.
const SUCCESS_TEXT: &str = "Message received successfully.";

/// Between the two segments (`spec/19-rest-servers.md` §4.1).
const SEGMENT_SEPARATOR: &str = "\r\n";

/// The `MSH-10` limit, which bounds a generated control id.
const GENERATED_CONTROL_ID_LENGTH: usize = 20;

/// The HL7 `ST` limit, which bounds `MSA-3` (§4.4).
const MAX_TEXT_LENGTH: usize = 199;

/// The `AA` for a parsed message.
///
/// # Examples
///
/// ```
/// use hl7_rest_server::ack::success;
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|252|252|MPI|MPI|20240101120000||ADT^A28^ADT_A05|MSGID12345|P|2.5",
/// )?;
/// let ack = success(&message, "20260820091500");
///
/// assert_eq!(
///     ack,
///     "MSH|^~\\&|DHCW|cymru.nhs.uk|252|252|20260820091500||ACK|MSGID12345|P|2.5\r\n\
///      MSA|AA|MSGID12345|Message received successfully."
/// );
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
#[must_use]
pub fn success(message: &Message, now: &str) -> String {
    build(
        &read(message, "MSH-3.1"),
        &read(message, "MSH-4.1"),
        &read(message, "MSH-10"),
        &version_of(message),
        ACCEPT,
        SUCCESS_TEXT,
        now,
    )
}

/// The `AR` for a parsed message that failed validation.
///
/// `generated_id` is used only when the message carries no `MSH-10`.
#[must_use]
pub fn validation_nack(message: &Message, reason: &str, generated_id: &str, now: &str) -> String {
    let control_id = read(message, "MSH-10");
    let control_id = if control_id.is_empty() {
        generated_id.to_string()
    } else {
        control_id
    };
    build(
        &read(message, "MSH-3.1"),
        &read(message, "MSH-4.1"),
        &control_id,
        &version_of(message),
        REJECT,
        reason,
        now,
    )
}

/// The `AE` when there is no parsed header (`spec/19-rest-servers.md` §4.2).
///
/// # Examples
///
/// ```
/// use hl7_rest_server::ack::generic_nack;
///
/// let nack = generic_nack("Unparsable", "abc123", "20260820091500");
///
/// assert_eq!(
///     nack,
///     "MSH|^~\\&|DHCW|cymru.nhs.uk|||20260820091500||ACK|abc123|P|2.5.1\r\n\
///      MSA|AE|abc123|Unparsable"
/// );
/// ```
#[must_use]
pub fn generic_nack(reason: &str, control_id: &str, now: &str) -> String {
    build("", "", control_id, DEFAULT_VERSION, ERROR, reason, now)
}

/// A fresh `MSH-10`: the first 20 hex characters of a UUID
/// (`spec/19-rest-servers.md` §4.3).
#[must_use]
pub fn generate_control_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..GENERATED_CONTROL_ID_LENGTH].to_string()
}

/// `MSH-12`, or the default.
fn version_of(message: &Message) -> String {
    let version = read(message, "MSH-12");
    if version.is_empty() {
        DEFAULT_VERSION.to_string()
    } else {
        version
    }
}

/// Assemble the two segments.
fn build(
    sending_application: &str,
    sending_facility: &str,
    control_id: &str,
    version: &str,
    code: &str,
    text: &str,
    now: &str,
) -> String {
    format!(
        "MSH|^~\\&|{SENDING_APPLICATION}|{SENDING_FACILITY}|{sending_application}|{sending_facility}|{now}||ACK|{control_id}|P|{version}\
         {SEGMENT_SEPARATOR}MSA|{code}|{control_id}|{}",
        sanitize(text)
    )
}

/// `spec/19-rest-servers.md` §4.4: no line breaks, delimiters escaped, at
/// most 199 characters. The cap applies to the input so an escape is never
/// cut in half.
fn sanitize(text: &str) -> String {
    text.chars()
        .take(MAX_TEXT_LENGTH)
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .fold(String::new(), |mut out, c| {
            match c {
                '\\' => out.push_str("\\E\\"),
                '|' => out.push_str("\\F\\"),
                '^' => out.push_str("\\S\\"),
                '&' => out.push_str("\\T\\"),
                '~' => out.push_str("\\R\\"),
                other => out.push(other),
            }
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "MSH|^~\\&|252|252|MPI|MPI|20240101120000||ADT^A28^ADT_A05|MSGID12345|P|2.5\r\
                         EVN|A28|20240101120000\r\
                         PID|1||123456^^^NHS||SMITH^JOHN||19800101|M";

    fn message() -> Message {
        ihb_validation::parse(VALID).unwrap()
    }

    #[test]
    fn success_ack_format() {
        // test_hl7_ack_builder.py::test_success_ack_format
        let ack = success(&message(), "20260820091500");
        assert!(
            ack.starts_with("MSH|^~\\&|DHCW|cymru.nhs.uk|252|252|"),
            "{ack}"
        );
        assert!(ack.contains("|ACK|MSGID12345|P|2.5"), "{ack}");
        assert!(
            ack.contains("\r\nMSA|AA|MSGID12345|Message received successfully."),
            "{ack}"
        );
    }

    #[test]
    fn a_validation_nack_echoes_the_control_id() {
        // test_hl7_ack_builder.py::test_validation_nack_echoes_control_id
        let nack = validation_nack(&message(), "Bad DOB", "GEN", "20260820091500");
        assert!(nack.contains("MSA|AR|MSGID12345|Bad DOB"), "{nack}");
    }

    #[test]
    fn a_validation_nack_without_a_control_id_uses_the_generated_one() {
        let bare = ihb_validation::parse("MSH|^~\\&|252|252|MPI|MPI|20240101||ADT^A28|").unwrap();
        let nack = validation_nack(&bare, "x", "GEN1", "20260820091500");
        assert!(nack.contains("|ACK|GEN1|"), "{nack}");
        assert!(nack.contains("MSA|AR|GEN1|x"), "{nack}");
    }

    #[test]
    fn the_generic_nack_has_blank_sender_and_the_default_version() {
        // test_hl7_ack_builder.py::test_generic_nack_generates_control_id
        let nack = generic_nack("Unparsable", &generate_control_id(), "20260820091500");
        assert!(nack.contains("MSH|^~\\&|DHCW|cymru.nhs.uk|||"), "{nack}");
        let control_id = nack
            .split("MSA|AE|")
            .nth(1)
            .unwrap()
            .split('|')
            .next()
            .unwrap();
        assert!((1..=20).contains(&control_id.len()), "{control_id}");
        assert!(nack.contains("|2.5.1"), "{nack}");
    }

    #[test]
    fn a_generated_control_id_is_bounded() {
        // test_hl7_ack_builder.py::test_generated_control_id_is_bounded
        assert!(generate_control_id().len() <= 20);
        assert_ne!(generate_control_id(), generate_control_id());
    }

    #[test]
    fn a_reason_cannot_corrupt_the_segment() {
        // spec/19-rest-servers.md §4.4 — the one deliberate difference.
        let nack = generic_nack("a|b^c\r\nd", "X", "20260820091500");
        let msa = nack.split("\r\n").nth(1).unwrap();
        assert_eq!(msa, "MSA|AE|X|a\\F\\b\\S\\c  d");

        let long = generic_nack(&"x".repeat(500), "X", "20260820091500");
        assert_eq!(
            long.split("\r\n").nth(1).unwrap().len(),
            "MSA|AE|X|".len() + 199
        );
    }
}
