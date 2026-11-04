//! Routing an `eventCode` to a bundle shape.
//!
//! Implements `spec/16-proms-transformer.md` §2.2 and §2.4.1.
//!
//! # Ten types, twelve codes
//!
//! Upstream #370 gave the three previously unreachable Python types
//! (`SURGERY_PERFORMED`, `OUTPATIENT_VISIT`, `DISCHARGE`) codes of their own
//! and added `APPOINTMENT_RESCHEDULE`, so all ten types are routable and all
//! ten are ported (§2.4.2, §10.2). The bare `SURGERY` and `PREOP` codes
//! survive as **legacy aliases** of `SURGERY-PROC` and `PREOP-AS`
//! ([`LEGACY_ALIASES`]), so twelve codes select ten types.
//!
//! The collision `spec/16-proms-transformer.md` §9.8 recorded — a discharge
//! arriving as `SURGERY` becoming a `Procedure` — is therefore confined to
//! payloads that still use the bare code.

/// The clinical resource a bundle carries (§2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clinical {
    /// A referral.
    ServiceRequest,
    /// A performed procedure.
    Procedure,
    /// A scheduled, rescheduled or cancelled appointment.
    Appointment,
    /// An admission, pre-admission, outpatient visit or discharge.
    Encounter,
}

impl Clinical {
    /// The FHIR resource type name, as written in `resourceType` and in a
    /// bundle entry's `fullUrl` reference type.
    #[must_use]
    pub const fn resource_type(self) -> &'static str {
        match self {
            Self::ServiceRequest => "ServiceRequest",
            Self::Procedure => "Procedure",
            Self::Appointment => "Appointment",
            Self::Encounter => "Encounter",
        }
    }
}

/// One routed message type (§2.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageType {
    /// The `eventCode` that selects it.
    pub code: &'static str,
    /// The Python's name for it, which appears in log lines.
    pub name: &'static str,
    /// `MessageHeader.eventCoding.code`.
    ///
    /// Not always the same as [`MessageType::code`] — see `PREREAD`.
    pub event_code: &'static str,
    /// `MessageHeader.eventCoding.display`.
    pub event_display: &'static str,
    /// The clinical resource this bundle carries.
    pub clinical: Clinical,
    /// `MessageHeader.definition`, appended to the definition base.
    pub definition_suffix: &'static str,
    /// `Appointment.status`, where the bundle has one.
    pub appointment_status: Option<&'static str>,
    /// Whether the bundle carries a `PractitionerRole` entry (§3).
    pub has_practitioner_role: bool,
}

/// `MessageHeader.eventCoding.system`; one code system for every event code.
pub const EVENT_SYSTEM: &str = "https://fhir.promptly.health/wpas/CodeSystem/wpas-event-codes";

/// The base `MessageHeader.definition` is appended to; the per-type tail is
/// [`MessageType::definition_suffix`].
pub const DEFINITION_BASE: &str = "https://fhir.promptly.health/wpas/MessageDefinition/wpas";

/// §2.4.1's table: one entry per **type**, in the order of upstream's
/// `message_types.py` (`REFERRAL`, the surgery types, the pre-op types,
/// then `INPATIENT`, `CANCELLED`, `PREREAD`).
///
/// Each type's [`MessageType::code`] is its canonical code; the two legacy
/// bare codes reach it through [`LEGACY_ALIASES`].
pub const MESSAGE_TYPES: [MessageType; 10] = [
    MessageType {
        code: "REFERRAL",
        name: "REFERRAL",
        event_code: "REFERRAL",
        event_display: "Patient Referral",
        clinical: Clinical::ServiceRequest,
        definition_suffix: "/referral",
        appointment_status: None,
        has_practitioner_role: true,
    },
    MessageType {
        code: "SURGERY-PROC",
        name: "PROCEDURE_PERFORMED",
        event_code: "SURGERY-PROC",
        event_display: "Procedure Performed",
        clinical: Clinical::Procedure,
        definition_suffix: "/procedure",
        appointment_status: None,
        has_practitioner_role: false,
    },
    MessageType {
        code: "SURGERY-PERF",
        name: "SURGERY_PERFORMED",
        event_code: "SURGERY-PERF",
        event_display: "Surgery Performed",
        clinical: Clinical::Procedure,
        definition_suffix: "/surgery",
        appointment_status: None,
        has_practitioner_role: false,
    },
    MessageType {
        code: "SURGERY-DN",
        name: "DISCHARGE",
        event_code: "SURGERY-DN",
        event_display: "Discharge Notification",
        clinical: Clinical::Encounter,
        definition_suffix: "/discharge",
        appointment_status: None,
        has_practitioner_role: false,
    },
    MessageType {
        code: "PREOP-AS",
        name: "APPOINTMENT_SCHEDULED",
        event_code: "PREOP-AS",
        event_display: "Appointment Scheduled",
        clinical: Clinical::Appointment,
        definition_suffix: "/appointment",
        appointment_status: Some("booked"),
        has_practitioner_role: false,
    },
    MessageType {
        code: "PREOP-VIS",
        name: "OUTPATIENT_VISIT",
        event_code: "PREOP-VIS",
        event_display: "Outpatient Visit",
        clinical: Clinical::Encounter,
        definition_suffix: "/outpatient",
        appointment_status: None,
        has_practitioner_role: false,
    },
    MessageType {
        code: "PREOP-AR",
        name: "APPOINTMENT_RESCHEDULE",
        event_code: "PREOP-AR",
        event_display: "Appointment Reschedule",
        clinical: Clinical::Appointment,
        definition_suffix: "/appointment-reschedule",
        appointment_status: Some("booked"),
        has_practitioner_role: false,
    },
    MessageType {
        code: "INPATIENT",
        name: "INPATIENT_ADMISSION",
        event_code: "INPATIENT",
        event_display: "Inpatient Admission",
        clinical: Clinical::Encounter,
        definition_suffix: "/encounter",
        appointment_status: None,
        has_practitioner_role: false,
    },
    MessageType {
        code: "CANCELLED",
        name: "APPOINTMENT_CANCELLED",
        event_code: "CANCELLED",
        event_display: "Appointment Cancellation",
        clinical: Clinical::Appointment,
        definition_suffix: "/appointment-cancellation",
        appointment_status: Some("cancelled"),
        has_practitioner_role: false,
    },
    MessageType {
        code: "PREREAD",
        name: "PREADMISSION",
        event_code: "PREREAD",
        event_display: "Pre-admission Notification",
        clinical: Clinical::Encounter,
        definition_suffix: "/preadmission",
        appointment_status: None,
        has_practitioner_role: false,
    },
];

/// Codes that select a type whose canonical [`MessageType::code`] differs
/// (§2.4.1, §10.2).
///
/// Upstream `MESSAGE_TYPES_BY_CODE` keeps bare `SURGERY` and `PREOP` "as a
/// legacy fallback ... for payloads that have not yet switched to the new
/// disambiguated codes". They route to the original scenarios, not to the
/// new ones.
pub const LEGACY_ALIASES: [(&str, &str); 2] = [("SURGERY", "SURGERY-PROC"), ("PREOP", "PREOP-AS")];

/// The type a routing key selects, canonical code or legacy alias.
fn find(key: &str) -> Option<&'static MessageType> {
    let canonical = LEGACY_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map_or(key, |(_, canonical)| *canonical);

    MESSAGE_TYPES.iter().find(|t| t.code == canonical)
}

/// Why a payload could not be routed (§2.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "cannot route the WPAS message: eventCode={event_code:?}, root={root_tag:?}. \
     Expected one of: {expected}"
)]
pub struct RoutingError {
    /// What the payload said, if anything.
    pub event_code: Option<String>,
    /// The root element, tried as a fallback (§2.1.1).
    pub root_tag: String,
    /// Every routable code, legacy aliases included, sorted (§2.4.3).
    pub expected: String,
}

/// Select a message type (§2.1, §2.1.1, §2.3).
///
/// The event code first, then the root tag as a fallback. A legacy bare
/// `SURGERY` or `PREOP` resolves to the suffixed type (§10.2).
///
/// # Errors
///
/// [`RoutingError`] when neither resolves — §2.3 makes that a failed
/// transform, so the message is abandoned rather than turned into a bundle
/// nobody asked for.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::message_type::{resolve, Clinical};
///
/// let referral = resolve(Some("REFERRAL"), "PromsEventRequest").unwrap();
/// assert_eq!(referral.clinical, Clinical::ServiceRequest);
///
/// // §10.2 — the bare legacy code routes to the original scenario.
/// assert_eq!(resolve(Some("PREOP"), "").unwrap().code, "PREOP-AS");
///
/// // §2.3 — anything else fails, and the message names what would work.
/// let error = resolve(Some("UNKNOWN_TYPE"), "PromsEventRequest").unwrap_err();
/// assert!(error.to_string().contains("REFERRAL"), "{error}");
/// ```
pub fn resolve(
    event_code: Option<&str>,
    root_tag: &str,
) -> Result<&'static MessageType, RoutingError> {
    for candidate in [event_code.unwrap_or_default(), root_tag] {
        let key = candidate.trim().to_ascii_uppercase();
        if key.is_empty() {
            continue;
        }
        if let Some(found) = find(&key) {
            return Ok(found);
        }
    }

    // §2.4.3: the Python lists `sorted(MESSAGE_TYPES_BY_CODE)`, whose keys
    // are the canonical codes *and* the two legacy ones.
    let mut expected: Vec<&str> = MESSAGE_TYPES
        .iter()
        .map(|t| t.code)
        .chain(LEGACY_ALIASES.iter().map(|(alias, _)| *alias))
        .collect();
    expected.sort_unstable();

    Err(RoutingError {
        event_code: event_code.map(ToString::to_string),
        root_tag: root_tag.to_string(),
        expected: expected.join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_of_the_routing_table() {
        // spec/16-proms-transformer.md §2.4.1 and §8.2.
        let expected = [
            (
                "REFERRAL",
                Clinical::ServiceRequest,
                "Patient Referral",
                "/referral",
            ),
            (
                "SURGERY-PROC",
                Clinical::Procedure,
                "Procedure Performed",
                "/procedure",
            ),
            (
                "SURGERY-PERF",
                Clinical::Procedure,
                "Surgery Performed",
                "/surgery",
            ),
            (
                "SURGERY-DN",
                Clinical::Encounter,
                "Discharge Notification",
                "/discharge",
            ),
            (
                "PREOP-AS",
                Clinical::Appointment,
                "Appointment Scheduled",
                "/appointment",
            ),
            (
                "PREOP-VIS",
                Clinical::Encounter,
                "Outpatient Visit",
                "/outpatient",
            ),
            (
                "PREOP-AR",
                Clinical::Appointment,
                "Appointment Reschedule",
                "/appointment-reschedule",
            ),
            (
                "INPATIENT",
                Clinical::Encounter,
                "Inpatient Admission",
                "/encounter",
            ),
            (
                "CANCELLED",
                Clinical::Appointment,
                "Appointment Cancellation",
                "/appointment-cancellation",
            ),
            (
                "PREREAD",
                Clinical::Encounter,
                "Pre-admission Notification",
                "/preadmission",
            ),
        ];

        assert_eq!(expected.len(), MESSAGE_TYPES.len());
        for (code, clinical, display, suffix) in expected {
            let found =
                resolve(Some(code), "PromsEventRequest").unwrap_or_else(|e| panic!("{code}: {e}"));

            assert_eq!(found.code, code);
            assert_eq!(found.clinical, clinical, "{code}");
            assert_eq!(found.event_display, display, "{code}");
            assert_eq!(found.definition_suffix, suffix, "{code}");
        }
    }

    #[test]
    fn the_legacy_bare_codes_route_to_the_original_scenarios() {
        // §2.4.1 and §10.2 — upstream #370 kept `SURGERY` and `PREOP` "as a
        // legacy fallback", NOT as aliases of the new scenarios.
        assert_eq!(resolve(Some("SURGERY"), "").unwrap().code, "SURGERY-PROC");
        assert_eq!(resolve(Some("PREOP"), "").unwrap().code, "PREOP-AS");
        assert_eq!(
            resolve(Some("surgery"), "").unwrap().name,
            "PROCEDURE_PERFORMED"
        );
    }

    #[test]
    fn the_appointment_types_differ_only_in_status_and_coding() {
        // §2.4 — "they differ in status and coding, not structure".
        let scheduled = resolve(Some("PREOP-AS"), "").unwrap();
        let cancelled = resolve(Some("CANCELLED"), "").unwrap();
        let rescheduled = resolve(Some("PREOP-AR"), "").unwrap();

        assert_eq!(scheduled.clinical, cancelled.clinical);
        assert_eq!(scheduled.clinical, rescheduled.clinical);
        assert_eq!(scheduled.appointment_status, Some("booked"));
        assert_eq!(rescheduled.appointment_status, Some("booked"));
        assert_eq!(cancelled.appointment_status, Some("cancelled"));
        assert_ne!(scheduled.event_display, cancelled.event_display);
    }

    #[test]
    fn the_encounter_types_differ_only_in_coding() {
        let inpatient = resolve(Some("INPATIENT"), "").unwrap();
        let preread = resolve(Some("PREREAD"), "").unwrap();

        assert_eq!(inpatient.clinical, preread.clinical);
        assert_ne!(inpatient.event_display, preread.event_display);
        assert_ne!(inpatient.definition_suffix, preread.definition_suffix);
    }

    #[test]
    fn only_the_referral_carries_a_practitioner_role() {
        // §3 — every other bundle goes straight from its clinical resource
        // to the common tail.
        let with_role: Vec<&str> = MESSAGE_TYPES
            .iter()
            .filter(|t| t.has_practitioner_role)
            .map(|t| t.code)
            .collect();

        assert_eq!(with_role, vec!["REFERRAL"]);
    }

    #[test]
    fn a_code_is_matched_case_insensitively_and_trimmed() {
        // §2.5.7 upper-cases before routing, and a payload can carry
        // whitespace around a value.
        for code in ["referral", "REFERRAL", " Referral ", "ReFeRrAl"] {
            assert_eq!(resolve(Some(code), "").unwrap().code, "REFERRAL", "{code}");
        }
    }

    #[test]
    fn an_unknown_code_fails_and_says_what_would_have_worked() {
        // §2.3 — the message is abandoned, so the error is the only thing
        // an operator gets.
        let error = resolve(Some("UNKNOWN_TYPE"), "PromsEventRequest").unwrap_err();

        let rendered = error.to_string();
        assert!(rendered.contains("UNKNOWN_TYPE"), "{rendered}");
        // §2.4.3 — the sorted key list, bare legacy codes included.
        assert!(
            rendered.ends_with(
                "CANCELLED, INPATIENT, PREOP, PREOP-AR, PREOP-AS, PREOP-VIS, PREREAD, \
                 REFERRAL, SURGERY, SURGERY-DN, SURGERY-PERF, SURGERY-PROC"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn the_root_tag_is_a_fallback_that_never_resolves_in_practice() {
        // §2.1.1 — the code does consult it, and no real root is one of
        // the six codes, so it always falls through to §2.3's failure.
        assert!(resolve(None, "PromsEventRequest").is_err());
        assert!(resolve(None, "WpasMessage").is_err());

        // It *would* resolve if a root happened to be a code, which is why
        // the fallback is described rather than dismissed.
        assert_eq!(resolve(None, "REFERRAL").unwrap().code, "REFERRAL");
    }

    #[test]
    fn no_event_code_and_no_root_is_a_failure_rather_than_a_default() {
        // Defaulting to any type would emit a bundle asserting something
        // about a patient on the strength of an empty payload.
        assert!(resolve(None, "").is_err());
        assert!(resolve(Some(""), "").is_err());
        assert!(resolve(Some("   "), "  ").is_err());
    }

    #[test]
    fn no_two_types_share_an_event_code() {
        // §2.4.2 — since #370 every type has a code of its own, so the
        // collision §9.8 recorded cannot recur for canonical codes. An alias
        // must not shadow a canonical code either.
        for (alias, canonical) in LEGACY_ALIASES {
            assert!(MESSAGE_TYPES.iter().all(|t| t.code != alias), "{alias}");
            assert!(
                MESSAGE_TYPES.iter().any(|t| t.code == canonical),
                "{canonical}"
            );
        }
        let mut codes: Vec<&str> = MESSAGE_TYPES.iter().map(|t| t.code).collect();
        codes.sort_unstable();
        let count = codes.len();
        codes.dedup();

        assert_eq!(codes.len(), count, "two types share an eventCode");
    }

    #[test]
    fn the_event_coding_system_and_definition_base_are_promptlys() {
        // These reach `MessageHeader`, which Promptly validates.
        assert!(EVENT_SYSTEM.starts_with("https://fhir.promptly.health/wpas/CodeSystem/"));
        assert!(
            DEFINITION_BASE.starts_with("https://fhir.promptly.health/wpas/MessageDefinition/")
        );

        for message_type in &MESSAGE_TYPES {
            assert!(
                message_type.definition_suffix.starts_with('/'),
                "{} would concatenate badly",
                message_type.code
            );
        }
    }
}
