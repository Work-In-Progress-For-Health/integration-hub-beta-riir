//! Removing patient detail from a payload before it is logged.
//!
//! Implements `spec/04-observability.md` §2.
//!
//! This module is a patient-safety control, not a convenience. Every event
//! the Integration Hub emits carries the message it is about, and those
//! events go to Azure Monitor, where they are queried by people and retained
//! for months. The full payload is still persisted to the message store for
//! replay; only the logging path is redacted.
//!
//! The rule is **deny by default**. A fixed allow-list of ten `MSH` fields
//! survives — the routing and metadata a support engineer needs to find one
//! message among millions — and everything else is masked. A payload that
//! cannot be recognised as an ER7 message at all is masked whole, because
//! its content is unknown and could be anything.
//!
//! Field *positions* are preserved: an empty field stays empty and a
//! populated one becomes `***`. That keeps the shape of a message visible —
//! which segments arrived, how many fields each carried, where the gaps are
//! — which is most of what a structural problem looks like, without any of
//! the content.
//!
//! # Examples
//!
//! ```
//! use ihb_telemetry::redact::redact_hl7;
//!
//! let message = "MSH|^~\\&|252|252|100|100|20260729095037|SEC|ADT^A28^ADT_A05|MSG1|P|2.5\r\
//!                PID|1||B0000010612^^^328^PI||SURNAME^FORENAME||20010909|M";
//!
//! let redacted = redact_hl7(message);
//!
//! // Routing metadata survives, so the message is still findable.
//! assert!(redacted.contains("|252|252|100|100|20260729095037|"));
//! assert!(redacted.contains("ADT^A28^ADT_A05"));
//! // The patient does not.
//! assert!(!redacted.contains("SURNAME"));
//! assert!(!redacted.contains("B0000010612"));
//! // MSH-8, the security field, is not on the allow-list.
//! assert!(!redacted.contains("SEC"));
//! ```

/// What a masked value is replaced with.
///
/// Three asterisks, matching `event_logger_lib/redaction.py`, so a log line
/// from a Rust container and one from a Python container read alike during
/// a cutover. It is [`hl7_2_redact`]'s default, and the test below is what
/// notices if that ever changes.
pub const MASK: &str = hl7_2_redact::DEFAULT_MASK;

/// The policy `spec/04-observability.md` §2.6 fixes: the ten `MSH` fields
/// below, and nothing else from any segment.
///
/// | | |
/// |---|---|
/// | `MSH-2` | encoding characters — needed to parse the message at all |
/// | `MSH-3`, `MSH-4` | sending application and facility |
/// | `MSH-5`, `MSH-6` | receiving application and facility |
/// | `MSH-7` | date and time of the message |
/// | `MSH-9` | message type |
/// | `MSH-10` | message control id |
/// | `MSH-11` | processing id |
/// | `MSH-12` | version id |
///
/// `MSH-8` (security) is deliberately absent.
///
/// Built once rather than per call: this is on the logging path of every
/// message the estate carries, and `benches/redaction.rs` measures it.
static POLICY: std::sync::LazyLock<hl7_2_redact::Policy> =
    std::sync::LazyLock::new(hl7_2_redact::Policy::routing_metadata);

/// Return a copy of a payload that is safe to log.
///
/// See the module documentation for the policy. Three cases are worth
/// stating outright:
///
/// - **Empty or whitespace-only input is returned unchanged.** There is
///   nothing to hide and masking it would only obscure that the payload was
///   empty, which is itself the diagnosis (`spec/04-observability.md` §2.3).
/// - **Anything that is not a recognisable ER7 message is masked whole.**
///   JSON, XML, a truncated frame, binary — the content is unknown, so none
///   of it is shown (§2.4).
/// - **The field separator is read from the message**, not assumed to be
///   `|` (§2.5).
///
/// Both `\r` and `\n` are accepted as segment terminators on input; the
/// result always uses `\r`.
///
/// # Examples
///
/// An empty payload is untouched:
///
/// ```
/// use ihb_telemetry::redact::redact_hl7;
///
/// assert_eq!(redact_hl7(""), "");
/// assert_eq!(redact_hl7("   "), "   ");
/// ```
///
/// Anything unrecognised is masked whole:
///
/// ```
/// use ihb_telemetry::redact::redact_hl7;
///
/// assert_eq!(redact_hl7(r#"{"nhsNumber": "4444444444"}"#), "***");
/// assert_eq!(redact_hl7("<Patient><name>Jones</name></Patient>"), "***");
/// ```
///
/// Empty fields stay empty, so the shape of a message survives:
///
/// ```
/// use ihb_telemetry::redact::redact_hl7;
///
/// // PID-2 is empty and PID-3 is not.
/// assert_eq!(
///     redact_hl7("MSH|^~\\&|A\rPID|1||X"),
///     "MSH|^~\\&|A\rPID|***||***"
/// );
/// ```
#[must_use]
pub fn redact_hl7(payload: &str) -> String {
    POLICY.redact(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_is_the_one_the_specification_fixes() {
        // spec/04-observability.md §2.6. `hl7-2-redact` lets a caller
        // choose; this workspace does not, and this is what notices if the
        // upstream `routing_metadata` policy ever changes shape.
        for field in [2, 3, 4, 5, 6, 7, 9, 10, 11, 12] {
            assert!(POLICY.accepts("MSH", field), "MSH-{field} must survive");
        }
        assert!(!POLICY.accepts("MSH", 8), "MSH-8 is the security field");
        assert!(!POLICY.accepts("MSH", 1), "MSH-1 is the separator itself");
        assert!(!POLICY.accepts("PID", 3), "no segment but MSH survives");
        assert_eq!(MASK, "***");
    }

    /// A message carrying values distinctive enough that finding any of them
    /// in an output is unambiguous.
    const MESSAGE: &str = concat!(
        "MSH|^~\\&|252|252|100|100|20260729095037|SECRET|ADT^A28^ADT_A05|MSG1|P|2.5|||AL|NE\r",
        "EVN|A28|20260729095037\r",
        "PID|1||NEEDLE0612^^^328^PI||NEEDLESURNAME^NEEDLEFORENAME||20010909|M\r",
        "PD1|||^^UNK|UNK\r",
        "PV1||N"
    );

    #[test]
    fn no_patient_marker_survives() {
        // spec/04-observability.md §2.12 — the test the whole module is for.
        let redacted = redact_hl7(MESSAGE);
        assert!(!redacted.contains("NEEDLE"), "{redacted}");
        assert!(!redacted.contains("20010909"), "{redacted}");
    }

    #[test]
    fn empty_input_is_returned_unchanged() {
        // spec/04-observability.md §2.3
        assert_eq!(redact_hl7(""), "");
        assert_eq!(redact_hl7("   "), "   ");
        assert_eq!(redact_hl7("\r\n"), "\r\n");
    }

    #[test]
    fn unrecognised_content_is_masked_whole() {
        // spec/04-observability.md §2.4
        assert_eq!(redact_hl7("{\"nhs\": \"4444444444\"}"), MASK);
        assert_eq!(redact_hl7("<Patient/>"), MASK);
        assert_eq!(redact_hl7("PID|1||X"), MASK, "no MSH header");
        assert_eq!(redact_hl7("MSH"), MASK, "too short to carry a separator");
    }

    #[test]
    fn a_multibyte_character_where_the_separator_goes_does_not_panic() {
        // Found by fuzzing: `MSH` plus any non-ASCII character put a byte
        // slice in the middle of a character and panicked, which an ingress
        // server would have taken as a five-byte denial of service.
        // spec/04-observability.md §2.5a.
        for suffix in ["\u{57e}", "é", "\u{1F600}", "\u{4e2d}\u{6587}"] {
            let input = format!("MSH{suffix}");
            let redacted = redact_hl7(&input);
            assert!(!redacted.is_empty(), "{input:?} produced nothing");
        }
    }

    #[test]
    fn a_multibyte_separator_is_honoured_rather_than_assumed_ascii() {
        // The same input class, but well-formed: a message whose MSH-1 is a
        // multi-byte character still splits on it, keeps the allow-list, and
        // masks MSH-8 like any other.
        let redacted = redact_hl7(
            "MSH\u{a7}^~\\&\u{a7}252\u{a7}252\u{a7}100\u{a7}100\u{a7}20260729\u{a7}SECRET",
        );

        assert!(
            redacted.starts_with("MSH\u{a7}^~\\&\u{a7}252\u{a7}252"),
            "the allow-list did not survive a multi-byte separator: {redacted}"
        );
        assert!(
            !redacted.contains("SECRET"),
            "MSH-8 was not masked: {redacted}"
        );
    }

    #[test]
    fn a_non_pipe_separator_is_read_from_the_message() {
        // spec/04-observability.md §2.5
        let redacted = redact_hl7("MSH!^~\\&!252!252!100!100!20260729!SECRET\rPID!1!!NEEDLE");
        assert!(redacted.starts_with("MSH!^~\\&!252!252!100!100!20260729!***"));
        assert!(!redacted.contains("NEEDLE"));
    }

    #[test]
    fn the_msh_allow_list_is_exactly_ten_fields() {
        // spec/04-observability.md §2.6
        let redacted = redact_hl7(MESSAGE);
        let header = redacted.split('\r').next().unwrap();
        let fields: Vec<&str> = header.split('|').collect();

        // fields[n] is MSH-(n + 1); see redact_msh.
        assert_eq!(fields[1], "^~\\&", "MSH-2 encoding characters");
        assert_eq!(fields[2], "252", "MSH-3 sending application");
        assert_eq!(fields[3], "252", "MSH-4 sending facility");
        assert_eq!(fields[4], "100", "MSH-5 receiving application");
        assert_eq!(fields[5], "100", "MSH-6 receiving facility");
        assert_eq!(fields[6], "20260729095037", "MSH-7 date/time");
        assert_eq!(fields[7], MASK, "MSH-8 security is NOT on the list");
        assert_eq!(fields[8], "ADT^A28^ADT_A05", "MSH-9 message type");
        assert_eq!(fields[9], "MSG1", "MSH-10 control id");
        assert_eq!(fields[10], "P", "MSH-11 processing id");
        assert_eq!(fields[11], "2.5", "MSH-12 version");
        // MSH-13 and MSH-14 are empty in the fixture, so they stay empty
        // (§2.8); MSH-15 is populated and is masked because it is not on
        // the allow-list.
        assert_eq!(fields[12], "", "MSH-13 is empty and stays empty");
        assert_eq!(fields[14], MASK, "MSH-15 and beyond are masked");
    }

    #[test]
    fn every_other_segment_keeps_only_its_identifier() {
        // spec/04-observability.md §2.7
        let redacted = redact_hl7(MESSAGE);
        let segments: Vec<&str> = redacted.split('\r').collect();
        assert_eq!(segments[1], "EVN|***|***");
        assert_eq!(segments[2], "PID|***||***||***||***|***");
        assert_eq!(segments[3], "PD1|||***|***");
        assert_eq!(segments[4], "PV1||***");
    }

    #[test]
    fn an_empty_field_stays_empty() {
        // spec/04-observability.md §2.8 — what keeps the shape readable.
        assert_eq!(
            redact_hl7("MSH|^~\\&|A\rPID|1||X"),
            "MSH|^~\\&|A\rPID|***||***"
        );
    }

    #[test]
    fn segments_rejoin_with_carriage_returns() {
        // spec/04-observability.md §2.9, and spec/index.md §0.6.
        let redacted = redact_hl7("MSH|^~\\&|A\nPID|1\nPV1|2");
        assert_eq!(redacted, "MSH|^~\\&|A\rPID|***\rPV1|***");
        assert!(!redacted.contains('\n'));
    }

    #[test]
    fn the_worked_example_from_the_specification_holds() {
        // spec/04-observability.md §2.10
        let input = concat!(
            "MSH|^~\\&|252|252|100|100|20260729095037|SEC|ADT^A28^ADT_A05|MSG1|P|2.5\r",
            "PID|1||B0000010612^^^328^PI||SURNAME^FORENAME||20010909|M"
        );
        let expected = concat!(
            "MSH|^~\\&|252|252|100|100|20260729095037|***|ADT^A28^ADT_A05|MSG1|P|2.5\r",
            "PID|***||***||***||***|***"
        );
        assert_eq!(redact_hl7(input), expected);
    }

    #[test]
    fn a_trailing_terminator_does_not_produce_an_empty_segment() {
        let redacted = redact_hl7("MSH|^~\\&|A\rPID|1\r");
        assert_eq!(redacted, "MSH|^~\\&|A\rPID|***");
    }

    #[test]
    fn a_segment_with_no_fields_is_left_alone() {
        assert_eq!(redact_hl7("MSH|^~\\&|A\rNTE"), "MSH|^~\\&|A\rNTE");
    }

    #[test]
    fn several_msh_segments_are_each_treated_as_a_header() {
        // A batch that reached the logger unsplit. Each MSH keeps its
        // allow-list rather than the second being masked as a body segment.
        let redacted = redact_hl7("MSH|^~\\&|A|B|C|D|E|SEC\rMSH|^~\\&|A|B|C|D|E|SEC");
        let segments: Vec<&str> = redacted.split('\r').collect();
        assert_eq!(segments[0], segments[1]);
        assert!(segments[1].ends_with("|***"));
    }
}
