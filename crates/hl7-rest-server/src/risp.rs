//! Multi-destination routing for RISP-sourced messages.
//!
//! Implements `spec/19-rest-servers.md` §6.
//!
//! Unlike every other flow — one destination, one payload format — a RISP
//! message can fan out to two destinations in two formats from a single
//! request: `A28`/`A31` to MPI as ER7, `A40` to MPI as ER7 **and** to WRRS as
//! v2.xml, `ORU_R01`/`OMG_O19` to WRRS only, after schema validation.
//!
//! # How the schema check differs from upstream
//!
//! Upstream converts, then validates the XML against the structure's XSD.
//! Here the XSD is read as a grammar and the message's segment sequence is
//! matched to it (`spec/19-rest-servers.md` §6.4). That checks presence,
//! order, cardinality and nesting, which is the part the Python's own design
//! notes call out as what the older converter got wrong, and it can only
//! ever put a segment tag and a position in an error — never a value.

use hl7_2::Message;
use ihb_validation::{grammar, resolver};

use crate::rules::{
    ADT_TRIGGERS_TO_MPI, Invalid, ORU_OMG_STRUCTURES, RISP_HL7_VERSION, WRRS_DIRECT_ADT_TRIGGERS,
    check_risp, resolve_structure, resolve_trigger,
};

/// Where a payload goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// `EGRESS_*`: `risp-hl7-transformer`, then MPI.
    MpiTransformer,
    /// `WRRS_*`.
    Wrrs,
}

/// A resolved send target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingTarget {
    /// Where.
    pub destination: Destination,
    /// What.
    pub payload: String,
    /// Whether the payload is v2.xml rather than ER7.
    pub is_xml: bool,
}

/// Validate a RISP message and resolve its targets, in send order.
///
/// # Errors
///
/// [`Invalid`] for a message that fails RISP's rules (`spec/19-rest-servers.md`
/// §5.3), a conversion failure (§6.5), or a schema failure (§6.4). In every
/// case nothing should be sent.
///
/// # Examples
///
/// ```
/// use hl7_rest_server::risp::{resolve_targets, Destination};
///
/// let er7 = "MSH|^~\\&|349|349|MPI|MPI|20240101120000||ADT^A28^ADT_A05|M1|P|2.5.1\r\
///            EVN|A28|20240101120000\r\
///            PID|1||654321^^^NHS||DOE^JANE||19900101|F";
/// let message = ihb_validation::parse(er7)?;
///
/// let targets = resolve_targets(&message, er7)?;
///
/// assert_eq!(targets.len(), 1);
/// assert_eq!(targets[0].destination, Destination::MpiTransformer);
/// assert_eq!(targets[0].payload, er7, "forwarded exactly as received");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn resolve_targets(message: &Message, raw: &str) -> Result<Vec<RoutingTarget>, Invalid> {
    check_risp(message)?;

    let trigger = resolve_trigger(message);
    let structure = resolve_structure(message);

    let mut targets = Vec::new();

    if ADT_TRIGGERS_TO_MPI.contains(&trigger.as_str()) {
        targets.push(RoutingTarget {
            destination: Destination::MpiTransformer,
            payload: raw.to_string(),
            is_xml: false,
        });
    }

    if WRRS_DIRECT_ADT_TRIGGERS.contains(&trigger.as_str()) {
        targets.push(RoutingTarget {
            destination: Destination::Wrrs,
            payload: convert_for_wrrs(raw, &trigger)?,
            is_xml: true,
        });
    } else if ORU_OMG_STRUCTURES.contains(&structure.as_str()) {
        targets.push(RoutingTarget {
            destination: Destination::Wrrs,
            payload: validate_and_convert_for_wrrs(message, raw, &structure)?,
            is_xml: true,
        });
    }

    Ok(targets)
}

/// `§6.5` — `trigger` as v2.xml for WRRS.
///
/// The conversion error is logged here in full but only reaches the caller
/// inside the `Invalid` text, which goes into a NACK; it names a structural
/// path and reason, never a field value (`spec/19-rest-servers.md` §6.5).
fn convert_for_wrrs(raw: &str, trigger: &str) -> Result<String, Invalid> {
    ihb_validation::er7_to_xml(raw).map_err(|error| {
        tracing::error!(%error, trigger, "could not convert a RISP message to v2.xml");
        Invalid(format!(
            "Failed to convert '{trigger}' message to HL7 v2.xml for WRRS: {error}"
        ))
    })
}

/// `§6.4` — match against the structure's custom schema, then convert.
fn validate_and_convert_for_wrrs(
    message: &Message,
    raw: &str,
    structure: &str,
) -> Result<String, Invalid> {
    let failed = |reason: String| {
        Invalid(format!(
            "XSD schema validation failed for '{structure}': {reason}"
        ))
    };

    let xsd = resolver::custom_schema(RISP_HL7_VERSION, structure)
        .ok_or_else(|| failed("no schema is configured for this structure".to_string()))?;
    let root = grammar::load(xsd, structure).map_err(|error| failed(error.to_string()))?;

    let tags: Vec<&str> = message
        .segments()
        .map(|segment| segment.name.as_str())
        .collect();
    grammar::match_structure(&root, &tags).map_err(|error| failed(error.to_string()))?;

    ihb_validation::er7_to_xml(raw).map_err(|error| failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The helpers.py samples.
    const A28: &str = "MSH|^~\\&|349|349|MPI|MPI|20240101120000||ADT^A28^ADT_A05|RISPMSG001|P|2.5.1\r\
                       EVN|A28|20240101120000\r\
                       PID|1||654321^^^NHS||DOE^JANE||19900101|F";
    const A40: &str = "MSH|^~\\&|349|349|MPI|MPI|20240101120000||ADT^A40^ADT_A39|RISPMSG002|P|2.5.1\r\
                       EVN|A40|20240101120000\r\
                       PID|1||654321^^^NHS||DOE^JANE||19900101|F\r\
                       MRG|123456^^^NHS";
    // The upstream helper's ORU has no PV1. The custom schema requires one
    // (`ORU_R01.VISIT` is not optional), and upstream never noticed because
    // its tests mock the schema check; a real RISP ORU carries it.
    const ORU: &str = "MSH|^~\\&|350|350|MPI|MPI|20240101120000||ORU^R01^ORU_R01|RISPMSG003|P|2.5.1\r\
                       PID|1||654321^^^NHS||DOE^JANE||19900101|F\r\
                       PV1|1|I\r\
                       OBR|1||ORDER1||\r\
                       OBX|1|ST|TEST||RESULT";

    fn resolve(raw: &str) -> Result<Vec<RoutingTarget>, Invalid> {
        resolve_targets(&ihb_validation::parse(raw).unwrap(), raw)
    }

    #[test]
    fn a28_routes_to_the_mpi_transformer_only() {
        // test_risp_routing.py::test_a28_routes_to_mpi_transformer_only
        let targets = resolve(A28).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].destination, Destination::MpiTransformer);
        assert_eq!(targets[0].payload, A28);
        assert!(!targets[0].is_xml);
    }

    #[test]
    fn a40_routes_to_both_in_send_order() {
        // test_a40_routes_to_both_mpi_transformer_and_wrrs; spec/19 §6 table.
        let targets = resolve(A40).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].destination, Destination::MpiTransformer);
        assert_eq!(targets[0].payload, A40);
        assert!(!targets[0].is_xml);

        assert_eq!(targets[1].destination, Destination::Wrrs);
        assert!(targets[1].is_xml);
        assert!(
            targets[1].payload.contains("urn:hl7-org:v2xml"),
            "{}",
            targets[1].payload
        );
        assert!(
            targets[1].payload.contains("<ADT_A39"),
            "{}",
            targets[1].payload
        );
    }

    #[test]
    fn oru_r01_routes_to_wrrs_only_after_the_schema_match() {
        // test_oru_r01_routes_to_wrrs_only_after_schema_validation
        let targets = resolve(ORU).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].destination, Destination::Wrrs);
        assert!(targets[0].is_xml);
        assert!(targets[0].payload.contains("ORU_R01"));
    }

    #[test]
    fn an_oru_that_does_not_fit_its_schema_is_refused_without_values() {
        // test_oru_r01_schema_validation_failure_raises — §6.4 and §10.6.
        let stray = format!("{ORU}\rZZZ|SECRET-VALUE");
        let error = resolve(&stray).unwrap_err().0;
        assert!(
            error.starts_with("XSD schema validation failed for 'ORU_R01': "),
            "{error}"
        );
        assert!(error.contains("ZZZ"), "names the segment tag: {error}");
        assert!(!error.contains("SECRET-VALUE"), "never a value: {error}");

        let missing_obr = "MSH|^~\\&|350|350|MPI|MPI|2024||ORU^R01^ORU_R01|1|P|2.5.1\rPID|1\rPV1|1";
        // The matcher is greedy and reports where the failed *group* began
        // (`06-validation.md` §10.3.4), not the segment that was missing.
        let error = resolve(missing_obr).unwrap_err().0;
        assert!(
            error.starts_with("XSD schema validation failed for 'ORU_R01': "),
            "{error}"
        );
    }

    #[test]
    fn an_invalid_facility_is_refused_before_routing() {
        // test_invalid_sender_facility_raises_before_routing
        let bad = A28.replace("|349|349|", "|999|999|");
        assert!(resolve(&bad).is_err());
    }
}
