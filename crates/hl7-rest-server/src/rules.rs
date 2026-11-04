//! The checks a message passes before it is forwarded.
//!
//! Implements `spec/19-rest-servers.md` §5.
//!
//! # The strings are the contract
//!
//! As in `hl7-server`'s `validate` module: each reason is upstream's text,
//! punctuation and trailing spaces included (`Unsupported message
//! type/trigger`, the `;` after the RISP version message), because operators
//! and the sending systems' own logs match on them. The RISP ones are the
//! strangest — a trailing space here, a `;` there — and are exactly as
//! `risp_validation.py` writes them.

use hl7_2::Message;
use ihb_fields::{authority_codes, read};

use crate::config::{FlowRule, Hl7Settings};

/// The only version RISP sends (`spec/19-rest-servers.md` §5.3).
pub const RISP_HL7_VERSION: &str = "2.5.1";

/// `MSH-3.1` of the ADT triggers.
pub const RISP_SENDING_FACILITY: &str = "349";

/// The inclusive range of `MSH-3.1` for `ORU_R01` and `OMG_O19`.
pub const RISP_ORU_OMG_FACILITIES: std::ops::RangeInclusive<u32> = 350..=358;

/// ADT triggers sent on to MPI.
pub const ADT_TRIGGERS_TO_MPI: [&str; 3] = ["A28", "A31", "A40"];

/// ADT triggers also sent to WRRS.
pub const WRRS_DIRECT_ADT_TRIGGERS: [&str; 1] = ["A40"];

/// Structures sent to WRRS after schema validation.
pub const ORU_OMG_STRUCTURES: [&str; 2] = ["ORU_R01", "OMG_O19"];

/// Why a message was refused: upstream's `ValidationException`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

/// `MSH-9.2`, the trigger event.
#[must_use]
pub fn resolve_trigger(message: &Message) -> String {
    read(message, "MSH-9.2")
}

/// `MSH-9.3`, falling back to `{MSH-9.1}_{MSH-9.2}` when both are present,
/// else empty (`spec/19-rest-servers.md` §5.3).
///
/// # Examples
///
/// ```
/// use hl7_rest_server::rules::resolve_structure;
///
/// let with = ihb_validation::parse("MSH|^~\\&|A|B|C|D|2024||ORU^R01^ORU_R01|1|P|2.5.1")?;
/// let without = ihb_validation::parse("MSH|^~\\&|A|B|C|D|2024||ADT^A40|1|P|2.5.1")?;
///
/// assert_eq!(resolve_structure(&with), "ORU_R01");
/// assert_eq!(resolve_structure(&without), "ADT_A40", "common for ADT");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
#[must_use]
pub fn resolve_structure(message: &Message) -> String {
    let structure = read(message, "MSH-9.3");
    if !structure.is_empty() {
        return structure;
    }
    let code = read(message, "MSH-9.1");
    let trigger = resolve_trigger(message);
    if code.is_empty() || trigger.is_empty() {
        String::new()
    } else {
        format!("{code}_{trigger}")
    }
}

/// §5.1 — the common checks and then the flow's own.
///
/// # Errors
///
/// [`Invalid`] with the first failure's text.
pub fn validate(message: &Message, settings: &Hl7Settings) -> Result<(), Invalid> {
    check_version(message, settings.hl7_version.as_deref())?;

    // §5.1.2 — RISP accepts several message types, each with its own
    // facility range, so a single `SENDING_APP` list cannot describe it and
    // the flow's own rule replaces the check.
    if settings.flow != Some(FlowRule::Risp) {
        check_sending_app(message, &settings.sending_apps)?;
    }

    match settings.flow {
        Some(FlowRule::Mpi) => check_mpi(message),
        Some(FlowRule::Risp) => check_risp(message),
        // Schema groups have no field rules here (§5.1.3).
        Some(FlowRule::Schema(_)) | None => Ok(()),
    }
}

/// `HL7_VERSION`.
fn check_version(message: &Message, expected: Option<&str>) -> Result<(), Invalid> {
    match expected {
        Some(expected) if read(message, "MSH-12") != expected => {
            Err(Invalid("Message has wrong version".to_string()))
        }
        _ => Ok(()),
    }
}

/// `SENDING_APP`.
fn check_sending_app(message: &Message, allowed: &[String]) -> Result<(), Invalid> {
    if allowed.is_empty() {
        return Ok(());
    }
    let value = read(message, "MSH-3");
    if allowed.iter().any(|candidate| candidate.trim() == value) {
        Ok(())
    } else {
        Err(Invalid(format!(
            "Message sending application '{value}' is not in allowed authority codes."
        )))
    }
}

/// §5.2 — the MPI outbound field rules.
///
/// # Errors
///
/// [`Invalid`] with one of three texts.
pub fn check_mpi(message: &Message) -> Result<(), Invalid> {
    let trigger = resolve_trigger(message);
    if trigger.is_empty() {
        return Err(Invalid(
            "MSH.9.2 MessageType is missing from the MPI outbound message".to_string(),
        ));
    }
    if !ADT_TRIGGERS_TO_MPI.contains(&trigger.as_str()) {
        return Err(Invalid(format!(
            "Unsupported message type '{trigger}' for MPI outbound flow"
        )));
    }
    if authority_codes(message, 2).is_empty() {
        return Err(Invalid(
            "PID.2.4.1 UpdateSources is missing from the MPI outbound message".to_string(),
        ));
    }
    Ok(())
}

/// §5.3 — RISP's version, sending facility and message type.
///
/// # Errors
///
/// [`Invalid`] with one of four texts.
pub fn check_risp(message: &Message) -> Result<(), Invalid> {
    let version = read(message, "MSH-12");
    if version != RISP_HL7_VERSION {
        return Err(Invalid(format!(
            "Currently this HL7 version '{version}' is unsupported by the HL7 Message Receiver API;"
        )));
    }

    let facility = read(message, "MSH-3.1");
    let trigger = resolve_trigger(message);
    let structure = resolve_structure(message);

    if ADT_TRIGGERS_TO_MPI.contains(&trigger.as_str()) {
        if facility != RISP_SENDING_FACILITY {
            return Err(Invalid(format!(
                "The sending facility '{facility}' is not included in the list of permitted \
                 Assigning Authorities. (trigger '{trigger}');"
            )));
        }
        return Ok(());
    }

    if ORU_OMG_STRUCTURES.contains(&structure.as_str()) {
        if !is_valid_oru_omg_facility(&facility) {
            return Err(Invalid(format!(
                "Message sending facility '{facility}' is not included in the list of permitted \
                 Assigning Authorities. "
            )));
        }
        return Ok(());
    }

    let named = if structure.is_empty() {
        trigger
    } else {
        structure
    };
    Err(Invalid(format!(
        "Unsupported message type/trigger '{named}'"
    )))
}

/// All digits, and in the inclusive range.
///
/// `str::parse::<u32>` would accept a leading `+`, which Python's
/// `str.isdigit` does not, so the digits are checked first.
fn is_valid_oru_omg_facility(facility: &str) -> bool {
    !facility.is_empty()
        && facility.chars().all(|c| c.is_ascii_digit())
        && facility
            .parse::<u32>()
            .is_ok_and(|value| RISP_ORU_OMG_FACILITIES.contains(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Python test's `_msg` helper.
    fn msg(facility: &str, message_type: &str, version: &str) -> Message {
        ihb_validation::parse(&format!(
            "MSH|^~\\&|{facility}|{facility}|MPI|MPI|20240101120000||{message_type}|MSGID001|P|{version}\r\
             EVN|A28|20240101120000\r\
             PID|1||654321^^^NHS||DOE^JANE||19900101|F"
        ))
        .unwrap()
    }

    fn ok(facility: &str, message_type: &str) {
        check_risp(&msg(facility, message_type, "2.5.1")).unwrap();
    }

    fn bad(facility: &str, message_type: &str) -> String {
        check_risp(&msg(facility, message_type, "2.5.1"))
            .unwrap_err()
            .0
    }

    #[test]
    fn valid_adt_triggers_with_the_right_facility() {
        // spec/19 §5.3 — test_valid_a28/a31/a40_with_correct_facility_and_version
        ok("349", "ADT^A28^ADT_A05");
        ok("349", "ADT^A31^ADT_A05");
        ok("349", "ADT^A40^ADT_A39");
    }

    #[test]
    fn an_adt_with_the_wrong_facility_is_rejected_with_upstreams_text() {
        // test_a28_with_wrong_facility_is_rejected
        assert_eq!(
            bad("350", "ADT^A28^ADT_A05"),
            "The sending facility '350' is not included in the list of permitted \
             Assigning Authorities. (trigger 'A28');"
        );
    }

    #[test]
    fn a_wrong_version_is_rejected_with_upstreams_text() {
        // test_a28_with_wrong_version_is_rejected
        assert_eq!(
            check_risp(&msg("349", "ADT^A28^ADT_A05", "2.5"))
                .unwrap_err()
                .0,
            "Currently this HL7 version '2.5' is unsupported by the HL7 Message Receiver API;"
        );
    }

    #[test]
    fn oru_and_omg_accept_the_whole_inclusive_range() {
        // test_oru_r01_within_facility_range_is_valid, test_omg_o19_...
        ok("350", "ORU^R01^ORU_R01");
        ok("358", "ORU^R01^ORU_R01");
        ok("355", "OMG^O19^OMG_O19");
    }

    #[test]
    fn oru_outside_the_range_or_non_numeric_is_rejected() {
        // test_oru_r01_outside_facility_range_is_rejected, ..._non_numeric_...
        for facility in ["359", "349", "ABC", "+355", ""] {
            assert!(
                bad(facility, "ORU^R01^ORU_R01").ends_with("Assigning Authorities. "),
                "{facility}: the text keeps its trailing space"
            );
        }
    }

    #[test]
    fn an_unsupported_type_is_named_by_structure_then_trigger() {
        // test_unsupported_message_type_is_rejected
        assert_eq!(
            bad("349", "ADT^A01^ADT_A01"),
            "Unsupported message type/trigger 'ADT_A01'"
        );
        // No structure and no code: falls back to the trigger.
        let message = ihb_validation::parse("MSH|^~\\&|349|349|M|M|2024||^A01|1|P|2.5.1").unwrap();
        assert_eq!(
            check_risp(&message).unwrap_err().0,
            "Unsupported message type/trigger 'A01'"
        );
    }

    fn settings(flow: Option<FlowRule>) -> Hl7Settings {
        Hl7Settings {
            flow,
            ..Hl7Settings::default()
        }
    }

    #[test]
    fn the_version_check_comes_first_and_says_only_that() {
        // spec/19 §5.1 item 1.
        let message = msg("349", "ADT^A28^ADT_A05", "2.5.1");
        let mut s = settings(Some(FlowRule::Risp));
        s.hl7_version = Some("2.9".into());
        s.sending_apps = vec!["nope".into()];
        assert_eq!(
            validate(&message, &s).unwrap_err().0,
            "Message has wrong version"
        );
    }

    #[test]
    fn sending_app_is_a_trimmed_allow_list_and_skipped_for_risp() {
        // spec/19 §5.1 item 2.
        let message = msg("349", "ADT^A28^ADT_A05", "2.5.1");
        let mut s = settings(None);
        s.sending_apps = vec!["252".into(), " 349 ".into()];
        validate(&message, &s).unwrap();

        s.sending_apps = vec!["252".into()];
        assert_eq!(
            validate(&message, &s).unwrap_err().0,
            "Message sending application '349' is not in allowed authority codes."
        );

        s.flow = Some(FlowRule::Risp);
        validate(&message, &s).unwrap();
    }

    #[test]
    fn mpi_rules_are_the_three_of_the_outbound_flow() {
        // spec/19 §5.2.
        let ok = ihb_validation::parse(
            "MSH|^~\\&|100|100|252|252|2024||ADT^A31^ADT_A05|M|P|2.5\rPID|1|8888888^^^252^PI",
        )
        .unwrap();
        check_mpi(&ok).unwrap();

        let no_source =
            ihb_validation::parse("MSH|^~\\&|100|100|252|252|2024||ADT^A31^ADT_A05|M|P|2.5\rPID|1")
                .unwrap();
        assert!(
            check_mpi(&no_source)
                .unwrap_err()
                .0
                .starts_with("PID.2.4.1")
        );

        let wrong = ihb_validation::parse(
            "MSH|^~\\&|100|100|252|252|2024||ADT^A01^ADT_A01|M|P|2.5\rPID|1|8888888^^^252^PI",
        )
        .unwrap();
        assert_eq!(
            check_mpi(&wrong).unwrap_err().0,
            "Unsupported message type 'A01' for MPI outbound flow"
        );

        let none = ihb_validation::parse("MSH|^~\\&|100|100|252|252|2024||ADT|M|P|2.5").unwrap();
        assert!(
            check_mpi(&none)
                .unwrap_err()
                .0
                .starts_with("MSH.9.2 MessageType")
        );
    }

    #[test]
    fn a_schema_flow_has_no_field_rules_here() {
        // spec/19 §5.1 item 3 — phw's date-of-birth rules are hl7-server's.
        let message = msg("349", "ADT^A28^ADT_A05", "2.5.1");
        let s = settings(Some(FlowRule::Schema(ihb_validation::Flow::Phw)));
        validate(&message, &s).unwrap();
    }
}
