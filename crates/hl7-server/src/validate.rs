//! The checks a message passes before it is forwarded.
//!
//! Implements `spec/07-hl7-server.md` §3 (the three layers) and §7 (the
//! accepted message types).
//!
//! # The strings are the contract
//!
//! Every message in this module is reproduced from the Python exactly,
//! punctuation included. They are not for developers: they reach operators
//! through `VALIDATION_FAILED` events and dashboards, and someone
//! triaging a stuck flow at three in the morning matches on them. A message
//! improved in passing is a runbook quietly broken.
//!
//! Where a string reads oddly — `PID.7 (Date of birth)` mixing a field
//! reference with prose, the inconsistent trailing full stops — that is
//! the Python's phrasing and it is preserved on purpose.

use hl7_2::Message;
use ihb_fields::{authority_codes, read};

/// `MSH-9` values this server accepts (`spec/07-hl7-server.md` §7).
///
/// A single allow-list applied by one handler. The Python registered these
/// as handler keys, which made the list look like a dispatch table; it is
/// not, because every entry maps to the same behaviour. §7.3 says so
/// explicitly.
///
/// Note the shape: both the three-component form (`ADT^A31^ADT_A05`) and the
/// two-component form (`ADT^A31`) are present, because source systems differ
/// in whether they name the structure.
pub const ACCEPTED_MESSAGE_TYPES: &[&str] = &[
    "ADT^A31^ADT_A05",
    "ADT^A28^ADT_A05",
    "ADT^A40^ADT_A39",
    "ADT^A31",
    "ADT^A28",
    "ADT^A40",
    "ADT^A04^ADT_A01",
    "ADT^A08^ADT_A01",
    "ADT^A40^ADT_A40",
];

/// Trigger events the MPI outbound flow carries
/// (`spec/07-hl7-server.md` §3.3.2).
pub const MPI_TRIGGERS: &[&str] = &["A28", "A31", "A40"];

/// The triggers Paris may send (`spec/07-hl7-server.md` §3.3.3). The same
/// three as [`MPI_TRIGGERS`] today, kept apart because they are two flows'
/// rules and upstream edits them separately.
pub const PARIS_TRIGGERS: &[&str] = &["A28", "A31", "A40"];

/// The triggers the WDS flow carries (`spec/07-hl7-server.md` §3.3.3a;
/// `A40` added upstream in `2060a7f`).
pub const WDS_TRIGGERS: &[&str] = &["A28", "A31", "A40"];

/// The earliest year a date of birth may name
/// (`spec/07-hl7-server.md` §3.3.1).
pub const EARLIEST_BIRTH_YEAR: i32 = 1800;

/// A message that will not be forwarded.
///
/// Carries the operator-facing `reason` and the `validation_result` that
/// goes in the `MESSAGE_FAILED` event, because those are two different
/// strings and the Python pairs them per failure kind rather than deriving
/// one from the other.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct Rejection {
    /// What went wrong, in the Python's exact words.
    pub reason: String,
    /// The `validation_result` for the `MESSAGE_FAILED` event.
    pub validation_result: &'static str,
    /// `MSA-1` of the NACK this rejection is answered with
    /// (`spec/07-hl7-server.md` §4.3): `AR` for a message that is invalid,
    /// `AE` for one the server could not process.
    pub ack_code: &'static str,
}

impl Rejection {
    /// A failure of the field-level checks of §3.1 to §3.3.
    #[must_use]
    pub fn validation(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            validation_result: "Validation failed",
            ack_code: crate::ack::REJECT,
        }
    }

    /// `spec/07-hl7-server.md` §7.1.
    #[must_use]
    pub fn unsupported_message_type(detail: impl Into<String>) -> Self {
        Self {
            reason: detail.into(),
            validation_result: "Unsupported message type",
            ack_code: crate::ack::ERROR,
        }
    }

    /// `spec/07-hl7-server.md` §7.2.
    #[must_use]
    pub fn unparseable(detail: impl Into<String>) -> Self {
        Self {
            reason: detail.into(),
            validation_result: "Invalid HL7 message format",
            ack_code: crate::ack::ERROR,
        }
    }
}

/// Is this a message type the server handles?
///
/// Implements `spec/07-hl7-server.md` §7.
///
/// Matched against `MSH-9` in both its three-component and two-component
/// forms, because a message that names its structure and one that does not
/// are the same message and both appear in production.
///
/// # Examples
///
/// ```
/// use hl7_server::validate::is_accepted_message_type;
///
/// assert!(is_accepted_message_type("ADT", "A31", "ADT_A05"));
/// assert!(is_accepted_message_type("ADT", "A31", ""), "the structure is optional");
/// assert!(!is_accepted_message_type("ORU", "R01", ""), "not an ADT");
/// assert!(!is_accepted_message_type("ADT", "A01", ""), "not a registered trigger");
/// ```
#[must_use]
pub fn is_accepted_message_type(code: &str, trigger: &str, structure: &str) -> bool {
    let two = format!("{code}^{trigger}");
    if ACCEPTED_MESSAGE_TYPES.contains(&two.as_str()) {
        return true;
    }
    if structure.is_empty() {
        return false;
    }
    ACCEPTED_MESSAGE_TYPES.contains(&format!("{two}^{structure}").as_str())
}

/// `MSH-9` as the allow-list check reads it.
#[must_use]
pub fn message_type_of(message: &Message) -> (String, String, String) {
    (
        read(message, "MSH-9.1"),
        read(message, "MSH-9.2"),
        read(message, "MSH-9.3"),
    )
}

/// §3.1 — the HL7 release.
///
/// # Errors
///
/// [`Rejection`] with the message `Message has wrong version`.
pub fn check_version(message: &Message, expected: Option<&str>) -> Result<(), Rejection> {
    let Some(expected) = expected else {
        // §3.1.2 — not configured means not checked. A flow with mixed
        // releases is a real configuration, not an oversight.
        return Ok(());
    };

    if read(message, "MSH-12") == expected {
        Ok(())
    } else {
        // The Python's message names neither the expected nor the actual
        // version. That is unhelpful, and it is what operators' runbooks
        // match on, so it is reproduced verbatim; the *event* carries the
        // message content, which is where the version can be seen.
        Err(Rejection::validation("Message has wrong version"))
    }
}

/// §3.2 — the sending application allow-list.
///
/// # Errors
///
/// [`Rejection`] naming the value that was not allowed.
pub fn check_sending_application(message: &Message, allowed: &[String]) -> Result<(), Rejection> {
    if allowed.is_empty() {
        // §3.2.1 — an empty or absent list disables the check.
        return Ok(());
    }

    let value = read(message, "MSH-3");
    if allowed.iter().any(|candidate| candidate == &value) {
        Ok(())
    } else {
        Err(Rejection::validation(format!(
            "Message sending application '{value}' is not in allowed authority codes."
        )))
    }
}

/// §3.3.1 — the PHW date-of-birth rules.
///
/// # Errors
///
/// [`Rejection`] with one of the three messages in §3.3.1's table.
pub fn check_phw(message: &Message) -> Result<(), Rejection> {
    let dob = read(message, "PID-7");

    // §3.3.1 — "no PID segment, or PID-7 blank" is one case with one
    // message. Reading an absent segment gives an empty string, so the two
    // converge here rather than needing to be distinguished.
    if dob.is_empty() {
        return Err(Rejection::validation(
            "PID.7 (Date of birth) is required for PHW.",
        ));
    }

    let malformed = || {
        Rejection::validation(
            "PID.7 (Date of birth) must be a valid date in YYYYMMDD format for PHW.",
        )
    };

    if dob.len() != 8 || !dob.chars().all(|c| c.is_ascii_digit()) {
        return Err(malformed());
    }

    // Eight ASCII digits, so every cut is on a boundary; `split_at_checked`
    // keeps the read free of slicing all the same.
    let (year_text, rest) = dob.split_at_checked(4).ok_or_else(malformed)?;
    let (month_text, day_text) = rest.split_at_checked(2).ok_or_else(malformed)?;
    let year: i32 = year_text.parse().map_err(|_| malformed())?;
    let month: u8 = month_text.parse().map_err(|_| malformed())?;
    let day: u8 = day_text.parse().map_err(|_| malformed())?;

    let month = time::Month::try_from(month).map_err(|_| malformed())?;
    // A real date, so 20250230 is rejected rather than rounded. The Python
    // used `datetime.strptime`, which is equally strict.
    time::Date::from_calendar_date(year, month, day).map_err(|_| malformed())?;

    // Checked last, and separately, because it has its own message: a date
    // that is well formed but implausible is a different operator problem
    // from one that is malformed.
    if year < EARLIEST_BIRTH_YEAR {
        return Err(Rejection::validation(
            "PID.7 (Date of birth) - year of birth must be 1800 or later for PHW.",
        ));
    }

    Ok(())
}

/// §3.3.3a — the WDS message-type rule.
///
/// # Errors
///
/// [`Rejection`] with one of the two messages in §3.3.3a's table.
pub fn check_wds(message: &Message) -> Result<(), Rejection> {
    let trigger = read(message, "MSH-9.2");

    if trigger.is_empty() {
        return Err(Rejection::validation(
            "MSH.9.2 MessageType is missing from the WDS message",
        ));
    }
    if !WDS_TRIGGERS.contains(&trigger.as_str()) {
        return Err(Rejection::validation(format!(
            "Unsupported message type '{trigger}' for WDS flow"
        )));
    }
    Ok(())
}

/// §3.3.2 — the MPI outbound rules.
///
/// # Errors
///
/// [`Rejection`] with one of the three messages in §3.3.2's table.
pub fn check_mpi(message: &Message) -> Result<(), Rejection> {
    let trigger = read(message, "MSH-9.2");

    if trigger.is_empty() {
        return Err(Rejection::validation(
            "MSH.9.2 MessageType is missing from the MPI outbound message",
        ));
    }

    if !MPI_TRIGGERS.contains(&trigger.as_str()) {
        return Err(Rejection::validation(format!(
            "Unsupported message type '{trigger}' for MPI outbound flow"
        )));
    }

    // The routing property `UpdateSources` is built from these
    // (`spec/03-message-bus.md` §4). A message with none would reach MPI
    // with an empty routing key and match nothing, so it is refused here
    // rather than delivered somewhere it cannot be found.
    if authority_codes(message, 2).is_empty() {
        return Err(Rejection::validation(
            "PID.2.4.1 UpdateSources is missing from the MPI outbound message",
        ));
    }

    Ok(())
}

/// §3.3.3 — the Paris message-type rules (upstream `9e797ac`).
///
/// # Errors
///
/// [`Rejection`] with one of the two messages in §3.3.3's table.
pub fn check_paris(message: &Message) -> Result<(), Rejection> {
    let trigger = read(message, "MSH-9.2");

    if trigger.is_empty() {
        return Err(Rejection::validation(
            "MSH.9.2 MessageType is missing from the Paris message",
        ));
    }

    if !PARIS_TRIGGERS.contains(&trigger.as_str()) {
        return Err(Rejection::validation(format!(
            "Unsupported message type '{trigger}' for Paris flow"
        )));
    }

    Ok(())
}

/// The field-level layers of §3.1 to §3.3, in the specified order.
///
/// Ordering is normative: a message failing both the version check and the
/// PHW date check must report the version, because that is what the Python
/// reports and what the operator's first question will be about.
///
/// `flow_name` is the raw `HL7_VALIDATION_FLOW`, not a resolved schema
/// group, because `mpi` selects rules here without being one
/// (`spec/01-architecture.md` §2.2.1).
///
/// # Errors
///
/// The first [`Rejection`] any layer produces.
///
/// # Examples
///
/// ```
/// use hl7_server::validate::check_fields;
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
///      PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M",
/// )?;
///
/// // The PHW flow, correctly configured, accepts it.
/// check_fields(&message, Some("2.5"), &["252".to_string()], Some("phw"))?;
///
/// // The version check runs first, and names only itself.
/// let wrong = check_fields(&message, Some("2.4"), &[], Some("phw")).unwrap_err();
/// assert_eq!(wrong.reason, "Message has wrong version");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn check_fields(
    message: &Message,
    hl7_version: Option<&str>,
    sending_apps: &[String],
    flow_name: Option<&str>,
) -> Result<(), Rejection> {
    check_version(message, hl7_version)?;
    check_sending_application(message, sending_apps)?;

    // §3.3.3 — any other flow has no field-level rules here; its checking is
    // the schema validation of §3.4.
    match flow_name {
        Some("phw") => check_phw(message),
        Some("mpi") => check_mpi(message),
        Some("paris") => check_paris(message),
        Some("wds") => check_wds(message),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(er7: &str) -> Message {
        ihb_validation::parse(er7).expect("the test message parses")
    }

    fn phw(dob: &str) -> Message {
        parse(&format!(
            "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
             PID|||8888888^^^252^PI||SURNAME^FORENAME||{dob}|M"
        ))
    }

    #[test]
    fn the_version_check_is_exact_and_only_runs_when_configured() {
        // spec/07-hl7-server.md §3.1.
        let message = phw("19990101");

        assert!(check_version(&message, Some("2.5")).is_ok());
        assert!(check_version(&message, None).is_ok(), "§3.1.2");

        let error = check_version(&message, Some("2.5.1")).unwrap_err();
        assert_eq!(error.reason, "Message has wrong version");
        assert_eq!(error.validation_result, "Validation failed");
    }

    #[test]
    fn the_sending_application_message_quotes_the_value() {
        // spec/07-hl7-server.md §3.2.2 — verbatim, full stop included.
        let message = phw("19990101");
        let error = check_sending_application(&message, &["999".to_string()]).unwrap_err();

        assert_eq!(
            error.reason,
            "Message sending application '252' is not in allowed authority codes."
        );
    }

    #[test]
    fn an_empty_sending_application_list_disables_the_check() {
        // spec/07-hl7-server.md §3.2.1.
        assert!(check_sending_application(&phw("19990101"), &[]).is_ok());
    }

    #[test]
    fn every_row_of_the_phw_date_of_birth_table() {
        // spec/07-hl7-server.md §3.3.1 — all four rows, so a change to one
        // message cannot be made without seeing the others.
        const REQUIRED: &str = "PID.7 (Date of birth) is required for PHW.";
        const MALFORMED: &str =
            "PID.7 (Date of birth) must be a valid date in YYYYMMDD format for PHW.";
        const TOO_EARLY: &str =
            "PID.7 (Date of birth) - year of birth must be 1800 or later for PHW.";

        for (dob, expected) in [
            ("", REQUIRED),
            ("1999010", MALFORMED),
            ("199901011", MALFORMED),
            ("1999AB01", MALFORMED),
            ("19990230", MALFORMED),
            ("19991301", MALFORMED),
            ("17991231", TOO_EARLY),
        ] {
            let error = check_phw(&phw(dob)).unwrap_err();
            assert_eq!(error.reason, expected, "for date of birth {dob:?}");
        }

        for good in ["19990101", "18000101", "20240229"] {
            assert!(check_phw(&phw(good)).is_ok(), "{good} should be accepted");
        }
    }

    #[test]
    fn a_message_with_no_pid_segment_reports_the_required_message() {
        // spec/07-hl7-server.md §3.3.1, first row: "No PID segment, or
        // PID-7 blank" is deliberately one case with one message.
        let message = parse("MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5");
        let error = check_phw(&message).unwrap_err();

        assert_eq!(error.reason, "PID.7 (Date of birth) is required for PHW.");
    }

    #[test]
    fn the_earliest_birth_year_boundary_is_inclusive() {
        assert!(check_phw(&phw("18000101")).is_ok(), "1800 is allowed");
        assert!(check_phw(&phw("17991231")).is_err(), "1799 is not");
    }

    #[test]
    fn every_row_of_the_mpi_table() {
        // spec/07-hl7-server.md §3.3.2.
        let no_trigger =
            parse("MSH|^~\\&|100|100|252|252|20250505232332||ADT|M1|P|2.5\rPID||1^^^108^PI");
        assert_eq!(
            check_mpi(&no_trigger).unwrap_err().reason,
            "MSH.9.2 MessageType is missing from the MPI outbound message"
        );

        let wrong_trigger = parse(
            "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A04^ADT_A01|M1|P|2.5\rPID||1^^^108^PI",
        );
        assert_eq!(
            check_mpi(&wrong_trigger).unwrap_err().reason,
            "Unsupported message type 'A04' for MPI outbound flow"
        );

        let no_authority =
            parse("MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\rPID");
        assert_eq!(
            check_mpi(&no_authority).unwrap_err().reason,
            "PID.2.4.1 UpdateSources is missing from the MPI outbound message"
        );

        for trigger in MPI_TRIGGERS {
            let good = parse(&format!(
                "MSH|^~\\&|100|100|252|252|20250505232332||ADT^{trigger}^ADT_A05|M1|P|2.5\r\
                 PID||1^^^108^PI"
            ));
            assert!(check_mpi(&good).is_ok(), "{trigger}");
        }
    }

    #[test]
    fn every_row_of_the_paris_table() {
        // spec/07-hl7-server.md §3.3.3, and that a missing PID is not a
        // Paris rejection (unlike §3.3.2).
        let message = |msh9: &str| {
            parse(&format!(
                "MSH|^~\\&|169|169|100|100|20250505232328||{msh9}|M1|P|2.5.1\rPID"
            ))
        };

        assert_eq!(
            check_paris(&message("ADT")).unwrap_err().reason,
            "MSH.9.2 MessageType is missing from the Paris message"
        );
        assert_eq!(
            check_paris(&message("ADT^A05^ADT_A05")).unwrap_err().reason,
            "Unsupported message type 'A05' for Paris flow"
        );
        for trigger in PARIS_TRIGGERS {
            assert!(check_paris(&message(&format!("ADT^{trigger}"))).is_ok());
        }
    }

    #[test]
    fn the_paris_rule_runs_only_for_the_paris_flow() {
        let a05 = parse("MSH|^~\\&|169|169|100|100|20250505232328||ADT^A05^ADT_A05|M1|P|2.5.1");

        assert!(check_fields(&a05, None, &[], Some("paris")).is_err());
        assert!(check_fields(&a05, None, &[], Some("chemo")).is_ok());
    }

    #[test]
    fn the_allow_list_accepts_both_forms_and_nothing_else() {
        // spec/07-hl7-server.md §7 — every registered key, plus the shapes
        // that must not be accepted.
        for accepted in ACCEPTED_MESSAGE_TYPES {
            let parts: Vec<&str> = accepted.split('^').collect();
            let structure = parts.get(2).copied().unwrap_or("");
            assert!(
                is_accepted_message_type(parts[0], parts[1], structure),
                "{accepted}"
            );
        }

        assert!(!is_accepted_message_type("ORU", "R01", "ORU_R01"));
        assert!(!is_accepted_message_type("ADT", "A01", "ADT_A01"));
        assert!(!is_accepted_message_type("", "", ""));
        assert!(
            !is_accepted_message_type("ADT", "A04", ""),
            "A04 is registered only with its structure"
        );
    }

    #[test]
    fn the_layers_run_in_the_order_section_three_gives() {
        // spec/07-hl7-server.md §3 — a message that fails several layers
        // reports the first, and which one that is, is normative.
        let bad_everything = parse(
            "MSH|^~\\&|999|999|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.4\rPID|||1||S^F||",
        );

        let error = check_fields(
            &bad_everything,
            Some("2.5"),
            &["252".to_string()],
            Some("phw"),
        )
        .unwrap_err();
        assert_eq!(
            error.reason, "Message has wrong version",
            "version is first"
        );

        // With the version right, the sending application is next.
        let error = check_fields(
            &bad_everything,
            Some("2.4"),
            &["252".to_string()],
            Some("phw"),
        )
        .unwrap_err();
        assert!(error.reason.contains("sending application"), "{error}");

        // With both right, the flow rules are last.
        let error = check_fields(&bad_everything, Some("2.4"), &[], Some("phw")).unwrap_err();
        assert!(error.reason.contains("Date of birth"), "{error}");
    }

    #[test]
    fn a_flow_with_no_field_rules_passes_the_third_layer() {
        // spec/07-hl7-server.md §3.3.4 — chemo, pims and mosaiq have no
        // field-level rules; their checking is the schema. (Paris has its
        // own, §3.3.3, tested below.)
        let message = phw("");
        for flow in [Some("chemo"), Some("pims"), None] {
            assert!(
                check_fields(&message, None, &[], flow).is_ok(),
                "{flow:?} should have no field rules of its own"
            );
        }
    }

    #[test]
    fn the_wds_flow_takes_a28_a31_and_a40_only() {
        // spec/07-hl7-server.md §3.3.3a.
        let with_trigger = |trigger: &str| {
            parse(&format!(
                "MSH|^~\\&|129|129|100|100|20250505232332||ADT^{trigger}|M1|P|2.5\rPID|||1"
            ))
        };
        for ok in ["A28", "A31", "A40"] {
            assert!(check_wds(&with_trigger(ok)).is_ok(), "{ok}");
        }
        let error = check_wds(&with_trigger("A08")).unwrap_err();
        assert_eq!(error.reason, "Unsupported message type 'A08' for WDS flow");

        let blank = parse("MSH|^~\\&|129|129|100|100|20250505232332||ADT|M1|P|2.5\rPID|||1");
        let error = check_wds(&blank).unwrap_err();
        assert_eq!(
            error.reason,
            "MSH.9.2 MessageType is missing from the WDS message"
        );
        assert!(
            check_fields(&blank, None, &[], Some("wds")).is_err(),
            "wired in"
        );
    }
}
