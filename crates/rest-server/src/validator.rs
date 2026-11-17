//! Payload validators.
//!
//! Implements `spec/19-rest-servers.md` §9.5.
//!
//! # Two kinds of failure, kept apart
//!
//! A payload that does not conform is the sender's problem and a 400
//! ([`Fault::Validation`]). A deployment that allows a structure its schema
//! group has no schema for is the **operator's** problem and a 500
//! ([`Fault::Request`] with `Server.Configuration`): the sender did nothing
//! wrong, and retrying will work once someone fixes the deployment. Collapsing
//! the two turns a configuration error into a message silently discarded as
//! invalid, which is why upstream reports them differently.

use std::path::{Path, PathBuf};

use ihb_validation::{
    Flow, structure, validate_and_convert, validate_standard_parsed, xml_to_er7, xsd,
};

use crate::adapter::RequestError;

/// Why validation stopped a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// The payload is invalid: always a 400 `Client.Validation`.
    Validation(String),
    /// A request-level or configuration failure with its own status.
    Request(RequestError),
}

/// Which validator a deployment uses (`VALIDATOR_TYPE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validator {
    /// `none`: no schema validation.
    None,
    /// `hl7-xsd`: a flow's schemas, through its dictionary
    /// (`spec/19-rest-servers.md` §9.5.1).
    Hl7Xsd {
        /// `VALIDATION_SCHEMA`.
        flow: Flow,
        /// `ALLOWED_HL7_STRUCTURES`.
        allowed_structures: Vec<String>,
    },
    /// `hl7apy`: the message's own release (`spec/19-rest-servers.md` §9.5.2).
    Hl7Standard {
        /// `ALLOWED_HL7_STRUCTURES`.
        allowed_structures: Vec<String>,
    },
    /// `xsd`: an arbitrary schema file.
    Xsd {
        /// `VALIDATION_SCHEMA`.
        path: PathBuf,
    },
}

impl Validator {
    /// The name `VALIDATOR_TYPE` spells it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Hl7Xsd { .. } => "hl7-xsd",
            Self::Hl7Standard { .. } => "hl7apy",
            Self::Xsd { .. } => "xsd",
        }
    }

    /// Validate a payload.
    ///
    /// # Errors
    ///
    /// [`Fault`], with no field value in its text.
    ///
    /// # Examples
    ///
    /// ```
    /// use rest_server::validator::Validator;
    ///
    /// // `none` accepts anything, by design (spec/19 §9.5).
    /// assert!(Validator::None.validate("<anything/>", Some("anything")).is_ok());
    /// ```
    pub fn validate(&self, payload_xml: &str, structure_id: Option<&str>) -> Result<(), Fault> {
        match self {
            Self::None => Ok(()),
            Self::Hl7Xsd {
                flow,
                allowed_structures,
            } => validate_hl7_xsd(*flow, allowed_structures, payload_xml, structure_id),
            Self::Hl7Standard { allowed_structures } => {
                validate_hl7_standard(allowed_structures, payload_xml, structure_id)
            }
            Self::Xsd { path } => validate_xsd(path, payload_xml),
        }
    }
}

/// An allow-list as the fault text renders it: sorted, comma-space separated.
fn sorted(values: &[String]) -> String {
    let mut sorted: Vec<&str> = values.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.join(", ")
}

/// `hl7_xsd_validator.py`.
fn validate_hl7_xsd(
    flow: Flow,
    allowed: &[String],
    payload_xml: &str,
    structure_id: Option<&str>,
) -> Result<(), Fault> {
    let structure_id = non_empty(structure_id)?;

    if !allowed.is_empty() && !allowed.iter().any(|a| a == structure_id) {
        return Err(unsupported(structure_id, allowed));
    }

    if !flow.has_structure(structure_id) {
        return Err(Fault::Request(RequestError::new(
            "Server.Configuration",
            format!(
                "Schema mapping is not configured for group '{}' and structure '{structure_id}'.",
                flow.as_str()
            ),
            500,
        )));
    }

    let failed =
        |reason: String| Fault::Validation(format!("Payload schema validation failed: {reason}"));

    let er7 = xml_to_er7(payload_xml).map_err(|error| failed(error.to_string()))?;
    let result =
        validate_and_convert(&er7, flow, None).map_err(|error| failed(error.to_string()))?;
    if result.is_valid {
        return Ok(());
    }
    Err(failed(result.error.as_ref().map_or_else(
        || "the payload does not conform to its schema".to_string(),
        ToString::to_string,
    )))
}

/// `hl7apy_validator.py`, with `hl7-2` for hl7apy.
fn validate_hl7_standard(
    allowed: &[String],
    payload_xml: &str,
    structure_id: Option<&str>,
) -> Result<(), Fault> {
    let declared = non_empty(structure_id)?;

    let er7 = xml_to_er7(payload_xml).map_err(|error| {
        tracing::error!(%error, "could not convert the payload to ER7 for validation");
        Fault::Validation("Unable to convert XML payload to ER7 format for validation.".to_string())
    })?;
    let message = ihb_validation::parse(&er7).map_err(|error| {
        tracing::error!(%error, "could not parse the converted payload");
        Fault::Validation(
            "Unable to parse converted ER7 payload for structure verification.".to_string(),
        )
    })?;

    // The structure the message itself names, not the one the envelope's
    // element claims: `xml_to_er7` converts whatever segments sit under the
    // root regardless of the root's own name, so a payload could declare an
    // allowed root while its MSH-9 names a disallowed structure.
    let actual =
        structure::resolve(&message, None).map_or_default(|resolved| resolved.structure_id);
    if actual != declared {
        let actual = if actual.is_empty() {
            "unknown"
        } else {
            actual.as_str()
        };
        return Err(Fault::Validation(format!(
            "Declared message structure '{declared}' does not match the structure \
             '{actual}' specified in MSH.9 of the payload."
        )));
    }
    if !allowed.is_empty() && !allowed.iter().any(|a| a == &actual) {
        return Err(unsupported(&actual, allowed));
    }

    let version = ihb_fields::read(&message, "MSH-12");
    validate_standard_parsed(&message, &version)
        .map_err(|error| Fault::Validation(format!("Payload validation failed: {error}")))
}

/// `xsd_validator.py`.
fn validate_xsd(path: &Path, payload_xml: &str) -> Result<(), Fault> {
    match xsd::validate_file(payload_xml, path) {
        Ok(report) if report.is_valid() => Ok(()),
        Ok(report) => Err(Fault::Validation(format!(
            "Payload schema validation failed: {}",
            report
                .first_reason()
                .unwrap_or("the payload does not conform to the schema")
        ))),
        Err(ihb_validation::ValidationError::Schema { .. }) => Err(Fault::Validation(format!(
            "Configured schema file '{}' could not be read",
            path.display()
        ))),
        Err(error) => Err(Fault::Validation(format!(
            "Payload schema validation failed: {error}"
        ))),
    }
}

/// The structure id, or the fault for its absence.
fn non_empty(structure_id: Option<&str>) -> Result<&str, Fault> {
    structure_id.filter(|id| !id.is_empty()).ok_or_else(|| {
        Fault::Validation("Unable to determine HL7 message structure from payload.".to_string())
    })
}

/// `Unsupported HL7 message structure …`.
fn unsupported(structure: &str, allowed: &[String]) -> Fault {
    Fault::Validation(format!(
        "Unsupported HL7 message structure '{structure}'. Allowed values: {}",
        sorted(allowed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A conforming PHW `ADT_A05`, as v2.xml.
    fn phw_xml() -> String {
        let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
                   EVN||20250502092900\r\
                   PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
                   PV1||U";
        ihb_validation::er7_to_xml_for_flow(er7, Flow::Phw, None).unwrap()
    }

    fn hl7_xsd(allowed: &[&str]) -> Validator {
        Validator::Hl7Xsd {
            flow: Flow::Phw,
            allowed_structures: allowed.iter().map(ToString::to_string).collect(),
        }
    }

    fn message(fault: &Fault) -> &str {
        match fault {
            Fault::Validation(message) => message,
            Fault::Request(error) => &error.message,
        }
    }

    #[test]
    fn none_accepts_anything() {
        // test_validators.py — NoOp.
        assert!(Validator::None.validate("garbage", None).is_ok());
    }

    #[test]
    fn hl7_xsd_accepts_a_conforming_payload() {
        // test_validators.py — hl7-xsd success.
        hl7_xsd(&["ADT_A05"])
            .validate(&phw_xml(), Some("ADT_A05"))
            .unwrap();
    }

    #[test]
    fn hl7_xsd_needs_a_structure() {
        let fault = hl7_xsd(&["ADT_A05"])
            .validate(&phw_xml(), None)
            .unwrap_err();
        assert_eq!(
            message(&fault),
            "Unable to determine HL7 message structure from payload."
        );
    }

    #[test]
    fn hl7_xsd_rejects_a_structure_off_the_allow_list_with_a_sorted_list() {
        // test_validators.py — unsupported structure; spec/19 §9.5.
        let fault = hl7_xsd(&["ADT_A39", "ADT_A05"])
            .validate(&phw_xml(), Some("ORU_R01"))
            .unwrap_err();
        assert_eq!(
            message(&fault),
            "Unsupported HL7 message structure 'ORU_R01'. Allowed values: ADT_A05, ADT_A39"
        );
        assert!(matches!(fault, Fault::Validation(_)));
    }

    #[test]
    fn an_allowed_structure_with_no_schema_is_a_500_configuration_fault() {
        // test_validators.py — the Server.Configuration case; spec/19 §9.5.
        let validator = Validator::Hl7Xsd {
            flow: Flow::Phw,
            allowed_structures: vec!["ZZZ_Z99".into()],
        };
        let Fault::Request(error) = validator.validate(&phw_xml(), Some("ZZZ_Z99")).unwrap_err()
        else {
            panic!("a configuration fault is not a validation failure");
        };
        assert_eq!(
            (error.code.as_str(), error.status),
            ("Server.Configuration", 500)
        );
        assert_eq!(
            error.message,
            "Schema mapping is not configured for group 'phw' and structure 'ZZZ_Z99'."
        );
    }

    #[test]
    fn hl7_xsd_rejects_a_payload_that_does_not_fit_its_schema() {
        // test_validators.py — schema failure → ValidationError.
        let without_pid = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
                           EVN||20250502092900\rPV1||U";
        let xml = ihb_validation::er7_to_xml(without_pid).unwrap();
        let fault = hl7_xsd(&["ADT_A05"])
            .validate(&xml, Some("ADT_A05"))
            .unwrap_err();
        assert!(
            message(&fault).starts_with("Payload schema validation failed: "),
            "{fault:?}"
        );
        assert!(matches!(fault, Fault::Validation(_)));
    }

    fn standard(allowed: &[&str]) -> Validator {
        Validator::Hl7Standard {
            allowed_structures: allowed.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn hl7apy_accepts_a_message_that_conforms_to_its_release() {
        // test_validators.py — hl7apy success. A complete ADT_A05 for 2.5.
        let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
                   EVN||20250502092900\r\
                   PID|1||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
                   PV1|1|U";
        let xml = ihb_validation::er7_to_xml(er7).unwrap();
        standard(&["ADT_A05"])
            .validate(&xml, Some("ADT_A05"))
            .unwrap();
    }

    #[test]
    fn hl7apy_checks_the_structure_msh9_names_not_the_one_declared() {
        // spec/19 §9.5 — the allow-list bypass the Python's own comments
        // describe: an allowed root around a disallowed MSH-9.
        let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
                   EVN||20250502092900\rPID|1";
        let xml = ihb_validation::er7_to_xml(er7).unwrap();
        let fault = standard(&["ADT_A39"])
            .validate(&xml, Some("ADT_A39"))
            .unwrap_err();
        assert_eq!(
            message(&fault),
            "Declared message structure 'ADT_A39' does not match the structure 'ADT_A05' \
             specified in MSH.9 of the payload."
        );
    }

    #[test]
    fn hl7apy_enforces_the_allow_list_on_the_actual_structure() {
        let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
                   EVN||20250502092900\rPID|1";
        let xml = ihb_validation::er7_to_xml(er7).unwrap();
        let fault = standard(&["ADT_A39"])
            .validate(&xml, Some("ADT_A05"))
            .unwrap_err();
        assert_eq!(
            message(&fault),
            "Unsupported HL7 message structure 'ADT_A05'. Allowed values: ADT_A39"
        );
    }

    #[test]
    fn hl7apy_rejects_a_message_whose_release_is_not_one_it_can_check() {
        // spec/19 §9.5.2 — the release is the message's own MSH-12, and
        // 2.3.1 is not in `SUPPORTED_VERSIONS`.
        let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.3.1\r\
                   EVN||20250502092900\rPID|1\rPV1|1|U";
        let xml = ihb_validation::er7_to_xml(er7).unwrap();
        let fault = standard(&["ADT_A05"])
            .validate(&xml, Some("ADT_A05"))
            .unwrap_err();
        assert!(
            message(&fault).starts_with("Payload validation failed: "),
            "{fault:?}"
        );
        assert!(
            message(&fault).contains("Unsupported HL7 version"),
            "{fault:?}"
        );
    }

    #[test]
    fn hl7apy_cannot_convert_xml_that_is_not_hl7() {
        let fault = standard(&[])
            .validate("<ADT_A05/>", Some("ADT_A05"))
            .unwrap_err();
        assert!(message(&fault).starts_with("Unable to "), "{fault:?}");
    }

    fn partner_xsd() -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("rest-server-partner-{}.xsd", std::process::id()));
        std::fs::write(
            &path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                 <xs:element name="Doc"><xs:complexType><xs:sequence>
                   <xs:element name="Id" type="xs:string"/>
                 </xs:sequence></xs:complexType></xs:element>
               </xs:schema>"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn xsd_validates_against_the_configured_file() {
        // test_validators.py — xsd success and failure; spec/06 §11.1.
        let validator = Validator::Xsd {
            path: partner_xsd(),
        };
        validator
            .validate("<Doc><Id>1</Id></Doc>", Some("Doc"))
            .unwrap();

        let fault = validator
            .validate("<Doc><Other/></Doc>", Some("Doc"))
            .unwrap_err();
        assert!(
            message(&fault).starts_with("Payload schema validation failed: "),
            "{fault:?}"
        );
    }

    #[test]
    fn xsd_with_an_unreadable_schema_says_so() {
        // test_validators.py — the OSError case.
        let validator = Validator::Xsd {
            path: PathBuf::from("/does/not/exist.xsd"),
        };
        let fault = validator.validate("<Doc/>", Some("Doc")).unwrap_err();
        assert_eq!(
            message(&fault),
            "Configured schema file '/does/not/exist.xsd' could not be read"
        );
    }
}
