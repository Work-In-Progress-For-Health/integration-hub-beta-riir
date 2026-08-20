//! Checking a message against its flow's schema, and against a release.
//!
//! Implements `spec/06-validation.md` §3.3, §6 and §8.3.

use hl7_2::validate::Kind;
use hl7_2::{Message, Options, Severity};

use crate::convert;
use crate::error::ValidationError;
use crate::flow::Flow;
use crate::repair;
use crate::structure::{self, TriggerMapping};

/// The HL7 releases this workspace validates against
/// (`spec/06-validation.md` §6.1).
pub const SUPPORTED_VERSIONS: [&str; 4] = ["2.4", "2.5", "2.5.1", "2.6"];

/// The outcome of validating a message, with the v2.xml it produced.
///
/// Both together, because the ingress path needs both and converting twice
/// is wasted work on every message
/// (`spec/06-validation.md` §8.3). The XML is present **even when the
/// message is invalid**: a rejected message is exactly the one an operator
/// wants to look at, and it is stored
/// (`spec/07-hl7-server.md` §3.4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    /// The v2.xml rendering.
    pub xml: String,
    /// The structure identifier this was validated as.
    pub structure_id: String,
    /// `MSH-9.1`, or empty.
    pub message_type: String,
    /// `MSH-9.2`, or empty.
    pub trigger_event: String,
    /// `MSH-10`, or empty.
    pub message_control_id: String,
    /// Whether the message conforms.
    pub is_valid: bool,
    /// Why not, when it does not.
    pub error: Option<ValidationError>,
}

impl ValidationResult {
    /// Whether the message conforms.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use ihb_validation::{Flow, validate_and_convert};
    ///
    /// let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
    ///            EVN||20250502092900\r\
    ///            PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
    ///            PV1||U";
    /// let result = validate_and_convert(er7, Flow::Phw, None)?;
    /// assert!(result.is_ok());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.is_valid
    }
}

/// Validate a message against its flow's schema, and return the v2.xml.
///
/// A schema failure is reported **in the result**, not as an `Err`: the
/// caller needs the XML either way. An `Err` here means the message could
/// not be read at all, or the flow has no schema for it.
///
/// # Errors
///
/// - [`ValidationError::Parse`] — the ER7 could not be read.
/// - [`ValidationError::Structure`] — no structure and no trigger.
/// - [`ValidationError::NoSchema`] — this flow has no schema for that
///   structure.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::{Flow, validate_and_convert};
///
/// // A PHW message missing its required PID segment.
/// let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
///            EVN||20250502092900\r\
///            PV1||U";
/// let result = validate_and_convert(er7, Flow::Phw, None)?;
///
/// assert!(!result.is_ok());
/// assert!(result.error.unwrap().to_string().contains("PID"));
/// // The XML is still there, so the message can be stored and inspected.
/// assert!(result.xml.contains("ADT_A05"));
/// # Ok(())
/// # }
/// ```
pub fn validate_and_convert(
    er7: &str,
    flow: Flow,
    trigger_mapping: Option<&TriggerMapping>,
) -> Result<ValidationResult, ValidationError> {
    let message = parse(er7)?;
    validate_and_convert_parsed(&message, flow, trigger_mapping)
}

/// [`validate_and_convert`] for a message that is already parsed.
///
/// The ingress path parses once and then validates, converts and reads
/// header fields from the same message; parsing again per step was measurable
/// in the Python and is why it grew these variants.
///
/// # Errors
///
/// As [`validate_and_convert`], minus the parse failure.
pub fn validate_and_convert_parsed(
    message: &Message,
    flow: Flow,
    trigger_mapping: Option<&TriggerMapping>,
) -> Result<ValidationResult, ValidationError> {
    let resolved = structure::resolve(message, trigger_mapping)?;

    if !flow.has_structure(&resolved.structure_id) {
        return Err(ValidationError::NoSchema {
            flow: flow.as_str().to_string(),
            structure: resolved.structure_id,
            available: available(flow),
        });
    }

    // The Python's conversion supplies a required-but-missing EVN or PV1
    // before validating, so a message without one is accepted with the
    // segment invented. Doing it here, on a copy, keeps that behaviour while
    // leaving what gets forwarded untouched. See spec/06-validation.md §4.6.
    let (repaired, _supplied) =
        repair::supply_required_segments(message, flow, &resolved.structure_id);
    let repaired_er7 = repaired.to_er7();

    let xml =
        convert::er7_to_xml_for_flow(&repaired_er7, flow, resolved.override_structure.as_deref())?;
    let outcome = check(&repaired_er7, flow);

    Ok(ValidationResult {
        xml,
        structure_id: resolved.structure_id,
        message_type: resolved.message_type,
        trigger_event: resolved.trigger_event,
        message_control_id: message.get("MSH-10").ok().flatten().unwrap_or_default(),
        is_valid: outcome.is_none(),
        error: outcome,
    })
}

/// Validate against a flow's schema, discarding the XML.
///
/// # Errors
///
/// As [`validate_and_convert`], plus [`ValidationError::Schema`] when the
/// message does not conform — because a caller that did not ask for the XML
/// wants the failure as an error rather than a field to remember to check.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::{Flow, validate_flow};
///
/// let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
///            EVN||20250502092900\r\
///            PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
///            PV1||U";
/// validate_flow(er7, Flow::Phw, None)?;
/// # Ok(())
/// # }
/// ```
pub fn validate_flow(
    er7: &str,
    flow: Flow,
    trigger_mapping: Option<&TriggerMapping>,
) -> Result<(), ValidationError> {
    let result = validate_and_convert(er7, flow, trigger_mapping)?;
    result.error.map_or(Ok(()), Err)
}

/// Validate a message against a standard HL7 release.
///
/// Selected by `HL7_VALIDATION_STANDARD`, and separate from the flow schema:
/// a flow's schema says what that sender may send, and this says what the
/// release permits at all.
///
/// # Errors
///
/// - [`ValidationError::UnsupportedVersion`] — not one of
///   [`SUPPORTED_VERSIONS`].
/// - [`ValidationError::Parse`] — the ER7 could not be read.
/// - [`ValidationError::Standard`] — the message does not conform.
///
/// # Examples
///
/// ```
/// use ihb_validation::validate_standard;
///
/// let error = validate_standard("MSH|^~\\&|A", "2.9").unwrap_err();
/// assert!(error.to_string().contains("Supported versions: 2.4, 2.5, 2.5.1, 2.6"));
/// ```
pub fn validate_standard(er7: &str, version: &str) -> Result<(), ValidationError> {
    check_version_supported(version)?;
    let message = parse(er7)?;
    validate_standard_parsed(&message, version)
}

/// [`validate_standard`] for a message that is already parsed.
///
/// Additionally checks that the message's own `MSH-12` agrees with the
/// requested release, which [`validate_standard`] cannot do without
/// re-reading the header (`spec/06-validation.md` §6.2).
///
/// # Errors
///
/// As [`validate_standard`], plus [`ValidationError::VersionMismatch`].
pub fn validate_standard_parsed(message: &Message, version: &str) -> Result<(), ValidationError> {
    check_version_supported(version)?;

    let declared = message
        .get("MSH-12")
        .ok()
        .flatten()
        .unwrap_or_default()
        .trim()
        .to_string();

    if !declared.is_empty() && declared != version {
        return Err(ValidationError::VersionMismatch {
            actual: declared,
            requested: version.to_string(),
        });
    }

    errors_of(message)
        .into_iter()
        .next()
        .map_or(Ok(()), |first| {
            Err(ValidationError::Standard {
                version: version.to_string(),
                reason: first.detail,
            })
        })
}

/// Parse an ER7 message, mapping the failure to this crate's error.
///
/// # Errors
///
/// [`ValidationError::Parse`].
pub fn parse(er7: &str) -> Result<Message, ValidationError> {
    hl7_2::parse(er7).map_err(ValidationError::parse)
}

/// Parse an ER7 message under a flow's dictionary.
///
/// # Errors
///
/// [`ValidationError::Parse`].
pub fn parse_for_flow(er7: &str, flow: Flow) -> Result<Message, ValidationError> {
    let options = Options::new().with_dictionary(flow.dictionary());
    hl7_2::parse_with_options(er7, &options).map_err(ValidationError::parse)
}

/// Validate a message against a flow's schema, returning the first problem.
fn check(er7: &str, flow: Flow) -> Option<ValidationError> {
    let message = match parse_for_flow(er7, flow) {
        Ok(message) => message,
        Err(error) => return Some(error),
    };

    errors_of(&message)
        .into_iter()
        .next()
        .map(|first| ValidationError::schema(first.path, first.detail))
}

/// The `Error`-severity diagnostics that count against a message.
///
/// **`ValueFormat` is excluded**, and this is the single most consequential
/// line in the crate (`spec/06-validation.md` §3.3.2a).
///
/// `hl7-2` checks that a value matches its data type — that a `DTM` parses
/// as a timestamp, that an `NM` is numeric. The flow XSDs do not: every
/// primitive in them is `<xsd:restriction base="xsd:string"/>`, so
/// `xmlschema` never looked at a value's shape and the Python's verdict never
/// depended on one. Including these would reject messages that are flowing
/// through production today — starting with the PHW sample, whose `MSH-7` is
/// `2025-05-05 23:23:32`, the very value the PHW transformer exists to
/// normalise.
///
/// It is also what keeps [`ValidationError`] free of patient data:
/// `ValueFormat` is the only diagnostic kind whose text embeds the value it
/// is complaining about.
fn errors_of(message: &Message) -> Vec<hl7_2::Diagnostic> {
    message
        .validate()
        .into_iter()
        .filter(counts_against_a_flow_schema)
        .collect()
}

/// Whether a diagnostic makes a message invalid *for a flow schema*.
///
/// Two adjustments to `hl7-2`'s own severities, and both come from the same
/// place: a flow dictionary was compiled from an XSD that **enumerates
/// exactly what that sender may send**, which is a stronger claim than a
/// bundled release's tables make.
///
/// **`ValueFormat` is excluded**, though `hl7-2` calls it an error. See
/// `spec/06-validation.md` §3.3.2a: the XSDs declare every primitive as an
/// unconstrained `xsd:string`, so `xmlschema` never checked a value's shape.
/// Including these would reject the PHW sample, whose `MSH-7` is
/// `2025-05-05 23:23:32` — the very value the PHW transformer exists to
/// normalise. It is also the only diagnostic kind whose text embeds the
/// value it is complaining about, so excluding it is what keeps
/// [`ValidationError`] free of patient data.
///
/// **`SegmentUnknown` and `StructureMismatch` are included**, though
/// `hl7-2` calls them warnings. Its reasoning is sound for a bundled release
/// — an unrecognised segment is usually a local extension rather than a
/// malformed message — but a flow XSD lists the segments that flow permits,
/// so one it does not list is a schema violation. A local Z-segment arrives
/// as `StructureMismatch`, detailed as "the message also carries local
/// Z-segments, which no structure describes". The Python rejects both, and
/// accepting them would mean forwarding to MPI a message production
/// currently bounces. See `spec/06-validation.md` §3.3.2c.
fn counts_against_a_flow_schema(diagnostic: &hl7_2::Diagnostic) -> bool {
    match diagnostic.kind {
        Kind::ValueFormat => false,
        Kind::SegmentUnknown | Kind::StructureMismatch => true,
        _ => diagnostic.severity == Severity::Error,
    }
}

/// Reject a release this workspace does not validate against.
fn check_version_supported(version: &str) -> Result<(), ValidationError> {
    if SUPPORTED_VERSIONS.contains(&version) {
        Ok(())
    } else {
        Err(ValidationError::UnsupportedVersion {
            version: version.to_string(),
            supported: SUPPORTED_VERSIONS.join(", "),
        })
    }
}

/// A flow's structures, for [`ValidationError::NoSchema`].
fn available(flow: Flow) -> String {
    let structures = flow.structures();
    if structures.is_empty() {
        "<none>".to_string()
    } else {
        structures.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PHW message that conforms.
    const PHW: &str = concat!(
        "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r",
        "EVN||20250502092900\r",
        "PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r",
        "PV1||U"
    );

    #[test]
    fn a_conforming_message_validates() {
        let result = validate_and_convert(PHW, Flow::Phw, None).unwrap();
        assert!(result.is_ok(), "{:?}", result.error);
        assert_eq!(result.structure_id, "ADT_A05");
        assert_eq!(result.message_type, "ADT");
        assert_eq!(result.trigger_event, "A31");
        assert_eq!(result.message_control_id, "M1");
    }

    #[test]
    fn a_missing_required_segment_fails() {
        // spec/06-validation.md §3.3.2 — a structural error counts.
        let without_pid: String = PHW
            .split('\r')
            .filter(|s| !s.starts_with("PID"))
            .collect::<Vec<_>>()
            .join("\r");

        let result = validate_and_convert(&without_pid, Flow::Phw, None).unwrap();
        assert!(!result.is_ok());
        assert!(result.error.unwrap().to_string().contains("PID"));
    }

    #[test]
    fn the_xml_is_produced_even_when_the_message_is_invalid() {
        // spec/06-validation.md §8.3, spec/07-hl7-server.md §3.4.4 — a
        // rejected message is the one an operator wants to look at.
        let without_pid: String = PHW
            .split('\r')
            .filter(|s| !s.starts_with("PID"))
            .collect::<Vec<_>>()
            .join("\r");

        let result = validate_and_convert(&without_pid, Flow::Phw, None).unwrap();
        assert!(!result.is_ok());
        assert!(result.xml.contains("ADT_A05"));
    }

    #[test]
    fn a_value_that_is_not_its_declared_type_does_not_fail_validation() {
        // spec/06-validation.md §3.3.2a — the finding the Phase 0 probe
        // turned up. MSH-7 here is not a valid DTM, and the PHW transformer
        // exists to normalise exactly this. Rejecting it would break the
        // flow.
        let odd_datetime = PHW.replace("20250505232332", "2025-05-05 23:23:32");
        let result = validate_and_convert(&odd_datetime, Flow::Phw, None).unwrap();
        assert!(
            result.is_ok(),
            "a ValueFormat diagnostic must not fail a message: {:?}",
            result.error
        );
    }

    #[test]
    fn a_structural_error_still_fails_after_the_value_format_exclusion() {
        // The exclusion must not have thrown out the baby: structural
        // diagnostics are still errors.
        let odd_and_broken = PHW
            .replace("20250505232332", "2025-05-05 23:23:32")
            .split('\r')
            .filter(|s| !s.starts_with("PID"))
            .collect::<Vec<_>>()
            .join("\r");

        let result = validate_and_convert(&odd_and_broken, Flow::Phw, None).unwrap();
        assert!(!result.is_ok());
    }

    #[test]
    fn a_structure_the_flow_has_no_schema_for_is_an_error() {
        // spec/06-validation.md §3.1.2
        let a03 = PHW.replace("ADT^A31^ADT_A05", "ADT^A03^ADT_A03");
        let error = validate_and_convert(&a03, Flow::Phw, None).unwrap_err();

        let text = error.to_string();
        assert!(text.contains("ADT_A03"), "{text}");
        assert!(text.contains("Available: ADT_A05, ADT_A39"), "{text}");
    }

    #[test]
    fn an_unparseable_message_is_an_error_not_a_verdict() {
        let error = validate_and_convert("not a message", Flow::Phw, None).unwrap_err();
        assert!(error.to_string().starts_with("Unable to parse ER7 message"));
    }

    #[test]
    fn validate_flow_reports_a_failure_as_an_error() {
        let without_pid: String = PHW
            .split('\r')
            .filter(|s| !s.starts_with("PID"))
            .collect::<Vec<_>>()
            .join("\r");

        assert!(validate_flow(&without_pid, Flow::Phw, None).is_err());
        assert!(validate_flow(PHW, Flow::Phw, None).is_ok());
    }

    #[test]
    fn an_unsupported_release_is_rejected_before_anything_else() {
        // spec/06-validation.md §6.1 — including for input that would not
        // have parsed, so the version check is unambiguously first.
        let error = validate_standard("nonsense", "2.9").unwrap_err();
        assert!(matches!(error, ValidationError::UnsupportedVersion { .. }));
    }

    #[test]
    fn every_supported_release_is_accepted_as_a_release() {
        // spec/06-validation.md §6.1
        for version in SUPPORTED_VERSIONS {
            assert!(check_version_supported(version).is_ok(), "{version}");
        }
        assert!(check_version_supported("2.3.1").is_err());
    }

    #[test]
    fn a_declared_release_that_disagrees_is_a_mismatch() {
        // spec/06-validation.md §6.2
        let message = hl7_2::parse(PHW).unwrap();
        let error = validate_standard_parsed(&message, "2.4").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Message version 2.5 does not match requested version 2.4"
        );
    }

    #[test]
    fn a_message_with_no_declared_release_is_not_a_mismatch() {
        // An absent MSH-12 cannot disagree with anything.
        let no_version = PHW.replace("|P|2.5", "|P|");
        let message = hl7_2::parse(&no_version).unwrap();
        assert!(!matches!(
            validate_standard_parsed(&message, "2.5"),
            Err(ValidationError::VersionMismatch { .. })
        ));
    }
}
