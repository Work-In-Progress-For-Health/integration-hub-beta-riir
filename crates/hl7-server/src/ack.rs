//! Building the acknowledgement.
//!
//! Implements `spec/07-hl7-server.md` §4.
//!
//! Every field here is normative. These bytes go back to PHW, Paris,
//! ChemoCare, PIMS, WDS and Mosaiq, each of which parses them with software
//! this project does not control and cannot change. A field in the wrong
//! place is not a cosmetic defect; it is a source system deciding its
//! message was lost and sending it again.
//!
//! # `AA` only for a forwarded message
//!
//! [`build`] is the `AA`. A message the server cannot forward gets a
//! negative acknowledgement instead (`spec/07-hl7-server.md` §4.3):
//! [`build_nack`] when the message parsed and its header can be echoed,
//! [`build_generic_nack`] when it did not. `AR` means the message is invalid
//! and retrying will not help; `AE` means try again. The senders classify on
//! exactly that (`spec/10-senders.md` §3.7.1), so choosing the wrong code is
//! choosing between a dead letter and a retry storm.

use hl7_2::Message;
use ihb_fields::read;

/// The HL7 field separator, `MSH-1`.
const FIELD_SEPARATOR: char = '|';

/// The HL7 encoding characters, `MSH-2`.
const ENCODING_CHARACTERS: &str = "^~\\&";

/// `MSH-9.1` and `MSH-9.3` of an acknowledgement.
const ACK: &str = "ACK";

/// `MSA-1` for an accepted message.
const ACCEPT: &str = "AA";

/// `MSA-1`: the message is invalid; do not send it again.
pub const REJECT: &str = "AR";

/// `MSA-1`: the server failed; send it again.
pub const ERROR: &str = "AE";

/// `MSH-12` of a NACK when there is no inbound message to copy it from.
const DEFAULT_VERSION: &str = "2.5";

/// The `MSH-10` limit, which bounds a generated control id.
const GENERATED_CONTROL_ID_LENGTH: usize = 20;

/// The HL7 `ST` limit, which bounds `MSA-3`.
const MAX_NACK_REASON_LENGTH: usize = 199;

/// `MSH-11` when the inbound message did not say.
const PRODUCTION: &str = "P";

/// Build the `AA` acknowledgement for a received message.
///
/// Takes the message **already parsed**. The server parses once, at step 3
/// of `spec/07-hl7-server.md` §5, and every later step reads from that one
/// parse; re-parsing here to read seven header fields would be measurable
/// per message and would also raise the question of what to do when the
/// re-parse disagrees with the first one.
///
/// `now` is the `MSH-7` timestamp, already formatted. It is a parameter
/// rather than read from the clock inside so that a test can assert on the
/// whole ACK rather than on everything except one field
/// (`spec/17-testing-and-quality.md` §5.1); [`timestamp`] is what production
/// passes.
///
/// # Examples
///
/// ```
/// use hl7_server::ack::build;
///
/// let inbound = ihb_validation::parse(
///     "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
///      PID|||8888888^^^252^PI",
/// )?;
///
/// let ack = build(&inbound, "20260820091500");
///
/// // The addressing is reversed: their receiver becomes our sender.
/// assert!(ack.starts_with("MSH|^~\\&|100|100|252|252|20260820091500||ACK^A31^ACK|M1|P|2.5\r"));
/// assert!(ack.ends_with("MSA|AA|M1"));
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
///
/// A message that omits `MSH-11` still gets a processing id, because an ACK
/// is required to carry one:
///
/// ```
/// use hl7_server::ack::build;
///
/// let inbound =
///     ihb_validation::parse("MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1||2.5")?;
/// let ack = build(&inbound, "20260820091500");
///
/// assert!(ack.contains("|M1|P|2.5\r"), "{ack}");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
#[must_use]
pub fn build(inbound: &Message, now: &str) -> String {
    // Read every field before building anything, so the shape of the ACK is
    // visible in one place and matches the table in §4 line for line.
    let sending_application = read(inbound, "MSH-5");
    let sending_facility = read(inbound, "MSH-6");
    let receiving_application = read(inbound, "MSH-3");
    let receiving_facility = read(inbound, "MSH-4");
    let trigger_event = read(inbound, "MSH-9.2");
    let control_id = read(inbound, "MSH-10");
    let version = read(inbound, "MSH-12");

    // §4.1 — copied so a test message is never acknowledged as production
    // (HL7 v2.5 §2.9.2.2), falling back only because MSH-11 is required.
    let processing_id = {
        let value = read(inbound, "MSH-11");
        if value.is_empty() {
            PRODUCTION.to_string()
        } else {
            value
        }
    };

    let message_type = format!("{ACK}^{trigger_event}^{ACK}");

    // MSH-1 is the separator itself and MSH-2 is the encoding characters, so
    // the header is written positionally rather than joined: `MSH-1` is not
    // a field *between* separators, it *is* the separator, and treating it
    // as one would produce `MSH||^~\&|...`.
    let msh = [
        "MSH",
        ENCODING_CHARACTERS,
        &sending_application,
        &sending_facility,
        &receiving_application,
        &receiving_facility,
        now,
        "", // MSH-8, security, unused.
        &message_type,
        &control_id,
        &processing_id,
        &version,
    ]
    .join(&FIELD_SEPARATOR.to_string());

    let msa = ["MSA", ACCEPT, &control_id].join(&FIELD_SEPARATOR.to_string());

    format!("{msh}\r{msa}")
}

/// Build an `AR` or `AE` echoing a message that did parse.
///
/// `spec/07-hl7-server.md` §4.3.1. The header is built exactly as [`build`]
/// builds it, so the two cannot drift; only `MSA-1` and `MSA-3` differ.
///
/// # Examples
///
/// ```
/// use hl7_server::ack::{build_nack, REJECT};
///
/// let inbound = ihb_validation::parse(
///     "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5",
/// )?;
/// let nack = build_nack(&inbound, REJECT, "Message has wrong version", "20260820091500");
///
/// assert!(nack.ends_with("MSA|AR|M1|Message has wrong version"), "{nack}");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
#[must_use]
pub fn build_nack(inbound: &Message, code: &str, reason: &str, now: &str) -> String {
    let accepted = build(inbound, now);
    let (msh, msa) = accepted.split_once('\r').unwrap_or((&accepted, ""));
    let control_id = msa.split('|').nth(2).unwrap_or_default();

    // MSH-12 falls back where `build` leaves it empty, because a NACK is
    // the one reply that must be parseable when the inbound was sloppy.
    let msh = if read(inbound, "MSH-12").is_empty() {
        let mut fields: Vec<&str> = msh.split('|').collect();
        if let Some(last) = fields.last_mut() {
            *last = DEFAULT_VERSION;
        }
        fields.join("|")
    } else {
        msh.to_string()
    };

    format!("{msh}\rMSA|{code}|{control_id}|{}", sanitize_reason(reason))
}

/// Build an `AE` when no message could be recovered.
///
/// `spec/07-hl7-server.md` §4.3.2: the frame was oversized, not UTF-8, or
/// did not parse, so there is no header to echo. `control_id` is generated
/// when `None`.
///
/// # Examples
///
/// ```
/// use hl7_server::ack::build_generic_nack;
///
/// let nack = build_generic_nack("could not parse", Some("GEN1"), "20260820091500");
///
/// assert_eq!(
///     nack,
///     "MSH|^~\\&|||||20260820091500||ACK^^ACK|GEN1|P|2.5\rMSA|AE|GEN1|could not parse"
/// );
/// ```
#[must_use]
pub fn build_generic_nack(reason: &str, control_id: Option<&str>, now: &str) -> String {
    let generated = control_id.map_or_else(generate_control_id, ToString::to_string);
    let control_id = generated.as_str();

    format!(
        "MSH|{ENCODING_CHARACTERS}|||||{now}||{ACK}^^{ACK}|{control_id}|{PRODUCTION}|{DEFAULT_VERSION}\
         \rMSA|{ERROR}|{control_id}|{}",
        sanitize_reason(reason)
    )
}

/// Best-effort `MSH-10` from text that did not parse as a message
/// (`spec/07-hl7-server.md` §2.6.2; upstream `_extract_control_id`).
///
/// Looks only at the first segment, after any leading `<CR>`; the field
/// separator is the character after `MSH`. Capped at 199 characters.
/// `None` when there is no `MSH` or the field is blank.
#[must_use]
pub fn sniff_control_id(raw: &str) -> Option<String> {
    let first = raw.trim_start_matches('\r').split('\r').next()?;
    let mut chars = first.chars();
    if !first.starts_with("MSH") {
        return None;
    }
    let separator = first.chars().nth(3)?;
    let _ = chars.next();
    let id = first.split(separator).nth(9)?.trim();
    let id: String = id.chars().take(199).collect();
    (!id.is_empty()).then_some(id)
}

/// A fresh `MSH-10`: the first 20 hex characters of a UUID.
#[must_use]
pub fn generate_control_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..GENERATED_CONTROL_ID_LENGTH].to_string()
}

/// `spec/07-hl7-server.md` §4.3.3: no line breaks, delimiters escaped,
/// at most 199 characters.
///
/// The cap is applied to the **input** first, as upstream does, so an
/// escape sequence is never cut in half.
fn sanitize_reason(reason: &str) -> String {
    reason
        .chars()
        .take(MAX_NACK_REASON_LENGTH)
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .fold(String::new(), |mut out, c| {
            match c {
                '|' => out.push_str("\\F\\"),
                '^' => out.push_str("\\S\\"),
                '&' => out.push_str("\\T\\"),
                '~' => out.push_str("\\R\\"),
                '\\' => out.push_str("\\E\\"),
                other => out.push(other),
            }
            out
        })
}

/// Re-exported from [`ihb_fields::timestamp`], where it lives so that
/// `hl7-mock-receiver` uses the same one — `spec/11-mock-receivers.md`
/// §2.4 makes its `MSH-7` this field, and one clause implemented twice is
/// the shape that drifts.
pub use ihb_fields::timestamp::{capture_local_offset, timestamp};

#[cfg(test)]
mod tests {
    use super::*;

    /// Every inbound release this estate receives
    /// (`spec/01-architecture.md` §2.2), so the ACK is proven against each
    /// rather than against 2.5 and hope.
    const RELEASES: [&str; 4] = ["2.3.1", "2.4", "2.5", "2.5.1"];

    /// The sample shape every test in this module varies.
    fn inbound(version: &str, processing_id: &str) -> Message {
        ihb_validation::parse(&raw(version, processing_id)).expect("the sample parses")
    }

    fn raw(version: &str, processing_id: &str) -> String {
        format!(
            "MSH|^~\\&|252|252SEND|100|100RECV|20250505232332||ADT^A31^ADT_A05|MSGID1|{processing_id}|{version}\r\
             EVN||20250502092900\r\
             PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M"
        )
    }

    #[test]
    fn every_field_of_section_four() {
        // spec/07-hl7-server.md §4 — the whole table, in order.
        let ack = build(&inbound("2.5", "P"), "20260820091500");
        let fields: Vec<&str> = ack.split('\r').next().unwrap().split('|').collect();

        assert_eq!(fields[0], "MSH", "MSH-1 is the separator, not a field");
        assert_eq!(fields[1], "^~\\&", "MSH-2");
        assert_eq!(fields[2], "100", "MSH-3 is the inbound MSH-5");
        assert_eq!(fields[3], "100RECV", "MSH-4 is the inbound MSH-6");
        assert_eq!(fields[4], "252", "MSH-5 is the inbound MSH-3");
        assert_eq!(fields[5], "252SEND", "MSH-6 is the inbound MSH-4");
        assert_eq!(fields[6], "20260820091500", "MSH-7");
        assert_eq!(fields[7], "", "MSH-8");
        assert_eq!(fields[8], "ACK^A31^ACK", "MSH-9");
        assert_eq!(fields[9], "MSGID1", "MSH-10");
        assert_eq!(fields[10], "P", "MSH-11");
        assert_eq!(fields[11], "2.5", "MSH-12");

        let msa: Vec<&str> = ack.split('\r').nth(1).unwrap().split('|').collect();
        assert_eq!(msa[0], "MSA");
        assert_eq!(msa[1], "AA", "MSA-1 — this server sends nothing else");
        assert_eq!(msa[2], "MSGID1", "MSA-2 is the inbound MSH-10");
    }

    #[test]
    fn the_addressing_is_reversed_for_every_release() {
        // spec/07-hl7-server.md §9.2.
        for version in RELEASES {
            let ack = build(&inbound(version, "P"), "20260820091500");
            assert!(
                ack.starts_with("MSH|^~\\&|100|100RECV|252|252SEND|"),
                "{version}: {ack}"
            );
            assert!(ack.contains(&format!("|{version}\r")), "{version}: {ack}");
        }
    }

    #[test]
    fn a_test_message_is_not_acknowledged_as_production() {
        // spec/07-hl7-server.md §4.1 — HL7 v2.5 §2.9.2.2. This is the whole
        // reason MSH-11 is copied rather than hard-coded, and getting it
        // wrong means a test harness's traffic is indistinguishable from a
        // hospital's in every downstream log.
        for version in RELEASES {
            let ack = build(&inbound(version, "T"), "20260820091500");
            assert!(ack.contains("|MSGID1|T|"), "{version}: {ack}");
            assert!(!ack.contains("|MSGID1|P|"), "{version}: {ack}");
        }
    }

    #[test]
    fn a_missing_processing_id_falls_back_to_production() {
        // spec/07-hl7-server.md §4.1 — the fallback exists only because
        // MSH-11 is required in an ACK, not because P is a sensible guess.
        for version in RELEASES {
            let ack = build(&inbound(version, ""), "20260820091500");
            assert!(ack.contains("|MSGID1|P|"), "{version}: {ack}");
        }
    }

    #[test]
    fn the_trigger_event_is_carried_but_the_type_and_structure_are_not() {
        // spec/07-hl7-server.md §4: MSH-9.1 and MSH-9.3 are literally ACK,
        // and only MSH-9.2 comes from the inbound message.
        for (trigger, expected) in [("A28", "ACK^A28^ACK"), ("A40", "ACK^A40^ACK")] {
            let message = ihb_validation::parse(&format!(
                "MSH|^~\\&|252|252|100|100|20250505232332||ADT^{trigger}^ADT_A05|M1|P|2.5"
            ))
            .unwrap();
            let ack = build(&message, "20260820091500");
            assert!(ack.contains(expected), "{ack}");
        }
    }

    #[test]
    fn a_message_with_no_trigger_event_still_produces_a_well_formed_ack() {
        // Not a shape §4 describes, but reachable: MSH-9 can be a bare
        // `ADT`. An ACK that is malformed here would be worse than one with
        // an empty component, because the source system would fail to parse
        // it rather than see an empty field.
        let message =
            ihb_validation::parse("MSH|^~\\&|252|252|100|100|20250505232332||ADT|M1|P|2.5")
                .unwrap();
        let ack = build(&message, "20260820091500");

        assert!(ack.contains("|ACK^^ACK|"), "{ack}");
        assert!(ack.ends_with("MSA|AA|M1"), "{ack}");
    }

    #[test]
    fn there_is_exactly_one_carriage_return_and_no_trailing_one() {
        // spec/index.md §0.6 — segments end with \r, and nothing this
        // workspace emits carries a trailing newline. A trailing separator
        // reads to some parsers as an empty third segment.
        let ack = build(&inbound("2.5", "P"), "20260820091500");

        assert_eq!(ack.matches('\r').count(), 1, "{ack:?}");
        assert!(!ack.ends_with('\r'), "{ack:?}");
        assert!(!ack.contains('\n'), "{ack:?}");
    }

    #[test]
    fn the_timestamp_is_fourteen_digits() {
        // spec/07-hl7-server.md §4 — `%Y%m%d%H%M%S`, no separators, no
        // timezone. The value cannot be asserted, but its shape can.
        let now = timestamp();

        assert_eq!(now.len(), 14, "{now}");
        assert!(now.chars().all(|c| c.is_ascii_digit()), "{now}");
    }

    #[test]
    fn a_nack_echoes_the_header_with_the_code_and_reason() {
        // spec/07-hl7-server.md §4.3.1.
        for code in [REJECT, ERROR] {
            let nack = build_nack(&inbound("2.5", "T"), code, "why", "20260820091500");
            let ack = build(&inbound("2.5", "T"), "20260820091500");

            assert_eq!(
                nack.split('\r').next(),
                ack.split('\r').next(),
                "the header is the AA's"
            );
            assert!(nack.ends_with(&format!("MSA|{code}|MSGID1|why")), "{nack}");
        }
    }

    #[test]
    fn a_nack_for_a_message_without_a_version_still_has_one() {
        // spec/07-hl7-server.md §4.3.1 — MSH-12 falls back to 2.5.
        let message =
            ihb_validation::parse("MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31|M1|P|")
                .unwrap();
        let nack = build_nack(&message, ERROR, "x", "20260820091500");

        assert!(nack.contains("|M1|P|2.5\r"), "{nack}");
    }

    #[test]
    fn a_generic_nack_has_the_shape_of_section_4_3_2() {
        let nack = build_generic_nack("bad", Some("CTRL"), "20260820091500");

        assert_eq!(
            nack,
            "MSH|^~\\&|||||20260820091500||ACK^^ACK|CTRL|P|2.5\rMSA|AE|CTRL|bad"
        );
    }

    #[test]
    fn a_generated_control_id_fits_msh_10() {
        let id = generate_control_id();

        assert_eq!(id.len(), 20, "{id}");
        assert_ne!(id, generate_control_id());
        let nack = build_generic_nack("x", None, "20260820091500");
        assert!(
            nack.contains(&format!("|{id}|")) || nack.contains("|ACK^^ACK|"),
            "{nack}"
        );
    }

    #[test]
    fn the_reason_cannot_break_the_segment() {
        // spec/07-hl7-server.md §4.3.3.
        let nack = build_generic_nack("a|b^c&d~e\\f\r\ng", Some("C"), "20260820091500");
        let msa = nack.split('\r').nth(1).unwrap();

        assert_eq!(nack.matches('\r').count(), 1, "{nack:?}");
        assert_eq!(msa, "MSA|AE|C|a\\F\\b\\S\\c\\T\\d\\R\\e\\E\\f  g");
    }

    #[test]
    fn the_reason_is_capped_at_199_characters() {
        // spec/07-hl7-server.md §4.3.3 — the HL7 ST limit.
        let nack = build_generic_nack(&"x".repeat(1000), Some("C"), "20260820091500");

        assert_eq!(nack.rsplit('|').next().unwrap().len(), 199);
    }

    #[test]
    fn a_control_id_is_read_from_text_that_does_not_parse() {
        // spec/07-hl7-server.md §2.6.2.
        let framed = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31|CTRL42|P|2.5\r\rPID|1";
        assert_eq!(sniff_control_id(framed).as_deref(), Some("CTRL42"));
        assert_eq!(
            sniff_control_id("\rMSH|^~\\&|a|b|c|d|e||f|ID7").as_deref(),
            Some("ID7")
        );
        assert_eq!(sniff_control_id("PID|1"), None, "no MSH");
        assert_eq!(sniff_control_id("MSH|^~\\&|a|b"), None, "too short");
        assert_eq!(sniff_control_id("MSH|^~\\&|a|b|c|d|e||f||P"), None, "blank");
        let long = format!("MSH|^~\\&|a|b|c|d|e||f|{}|P", "9".repeat(300));
        assert_eq!(sniff_control_id(&long).unwrap().len(), 199);
    }
}
