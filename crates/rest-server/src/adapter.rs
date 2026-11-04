//! Content adapters: unwrapping a request and shaping its response.
//!
//! Implements `spec/19-rest-servers.md` §9.4.
//!
//! An adapter owns both halves of a transport — the extraction (a SOAP
//! envelope, a plain document) and the response that goes with it (a SOAP
//! fault, a small XML error) — so a SOAP caller always gets SOAP back, and
//! one processor serves every profile.

use roxmltree::Document;

use crate::xml::{escape, find_first_text, local_name, standalone};

/// The SOAP envelope namespace.
pub const SOAP_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";

/// A request-level failure: an HTTP status and an adapter-specific fault code
/// (`spec/19-rest-servers.md` §9.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RequestError {
    /// The fault code, e.g. `Client` or `Client.Authorization`.
    pub code: String,
    /// What is wrong. Never carries a field value (`spec/index.md` §0.8).
    pub message: String,
    /// The HTTP status.
    pub status: u16,
}

impl RequestError {
    /// A failure with an explicit code and status (`spec/19-rest-servers.md`
    /// §9.3).
    ///
    /// `code` becomes the SOAP `faultcode` or the `<code>` element, and
    /// `message` its text; neither may quote the payload.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>, status: u16) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            status,
        }
    }

    /// A 400 `Client` failure: the request itself was unusable (malformed or
    /// missing XML), as opposed to `Client.Validation`, which is a payload
    /// that parsed but failed a rule (`spec/19-rest-servers.md` §9.3, §9.4).
    #[must_use]
    pub fn client(message: impl Into<String>) -> Self {
        Self::new("Client", message, 400)
    }
}

/// The business payload extracted from a request, and what routes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The payload as XML text.
    pub payload_xml: String,
    /// Its root element's local name.
    pub structure_id: Option<String>,
    /// Who sent it.
    pub source_identifier: Option<String>,
    /// For the Service Bus message id and the response.
    pub message_control_id: Option<String>,
    /// `MSH-9.2`, the trigger event (`spec/19-rest-servers.md` §10).
    pub message_type: Option<String>,
}

/// Which adapter a deployment uses (`CONTENT_ADAPTER`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentAdapter {
    /// A SOAP 1.1 envelope around the payload.
    Soap,
    /// The request body is the payload.
    XmlRaw {
        /// `SOURCE_IDENTIFIER_LOCATOR`.
        source_identifier_path: Option<Vec<String>>,
        /// `MESSAGE_CONTROL_ID_LOCATOR`.
        message_control_id_path: Option<Vec<String>>,
    },
}

impl ContentAdapter {
    /// The response content type.
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        match self {
            Self::Soap => "text/xml; charset=utf-8",
            Self::XmlRaw { .. } => "application/xml; charset=utf-8",
        }
    }

    /// The adapter's name, as `CONTENT_ADAPTER` spells it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Soap => "soap",
            Self::XmlRaw { .. } => "xml-raw",
        }
    }

    /// Unwrap a request body.
    ///
    /// # Errors
    ///
    /// [`RequestError`] with the text and code `spec/19-rest-servers.md`
    /// §9.4 gives each malformed shape.
    ///
    /// # Examples
    ///
    /// ```
    /// use rest_server::adapter::ContentAdapter;
    ///
    /// let request = concat!(
    ///     r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>"#,
    ///     r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><MSH><MSH.3><HD.1>328</HD.1></MSH.3>"#,
    ///     r#"<MSH.9><MSG.2>A31</MSG.2></MSH.9><MSH.10>M1</MSH.10></MSH></ADT_A05>"#,
    ///     r#"</s:Body></s:Envelope>"#,
    /// );
    ///
    /// let extracted = ContentAdapter::Soap.extract(request)?;
    ///
    /// assert_eq!(extracted.structure_id.as_deref(), Some("ADT_A05"));
    /// assert_eq!(extracted.source_identifier.as_deref(), Some("328"));
    /// assert_eq!(extracted.message_control_id.as_deref(), Some("M1"));
    /// assert_eq!(extracted.message_type.as_deref(), Some("A31"));
    /// # Ok::<(), rest_server::adapter::RequestError>(())
    /// ```
    pub fn extract(&self, raw_body: &str) -> Result<Extracted, RequestError> {
        match self {
            Self::Soap => extract_soap(raw_body),
            Self::XmlRaw {
                source_identifier_path,
                message_control_id_path,
            } => extract_xml_raw(
                raw_body,
                source_identifier_path.as_deref(),
                message_control_id_path.as_deref(),
            ),
        }
    }

    /// The success body in this adapter's format (`spec/19-rest-servers.md`
    /// §9.4); `message_control_id` is XML-escaped here (§9.4.1), because it
    /// came from the sender.
    #[must_use]
    pub fn success_response(&self, message_control_id: &str) -> String {
        let id = escape(message_control_id);
        match self {
            Self::Soap => format!(
                "<soapenv:Envelope xmlns:soapenv=\"{SOAP_NS}\" xmlns:tns=\"urn:inthub:rest-server\">\
                 <soapenv:Body><tns:AckResponse><Status>Success</Status>\
                 <MessageControlId>{id}</MessageControlId></tns:AckResponse></soapenv:Body>\
                 </soapenv:Envelope>"
            ),
            Self::XmlRaw { .. } => {
                format!(
                    "<ack><status>Success</status><messageControlId>{id}</messageControlId></ack>"
                )
            }
        }
    }

    /// The error body in this adapter's format (`spec/19-rest-servers.md`
    /// §9.4). `code` and `message` are escaped (§9.4.1) so a message that
    /// echoes a structure name from the request cannot inject markup.
    #[must_use]
    pub fn error_response(&self, code: &str, message: &str) -> String {
        let (code, message) = (escape(code), escape(message));
        match self {
            Self::Soap => format!(
                "<soapenv:Envelope xmlns:soapenv=\"{SOAP_NS}\"><soapenv:Body><soapenv:Fault>\
                 <faultcode>{code}</faultcode><faultstring>{message}</faultstring>\
                 </soapenv:Fault></soapenv:Body></soapenv:Envelope>"
            ),
            Self::XmlRaw { .. } => {
                format!("<error><code>{code}</code><message>{message}</message></error>")
            }
        }
    }
}

/// `soap_adapter.py :: SoapContentAdapter.extract` (`spec/19-rest-servers.md`
/// §9.4, `soap`).
///
/// Elements are matched on local name so any namespace prefix works. The
/// `Body` is found by descendant search rather than as a direct child, as
/// upstream did. The first candidate with text wins as the source
/// identifier, in the spec's order: sending application, sending facility,
/// patient identifier authority.
///
/// # Errors
///
/// A `Client` 400 for each structural fault, and `Client.Validation` 400
/// when no source identifier can be found.
fn extract_soap(raw_body: &str) -> Result<Extracted, RequestError> {
    let document = Document::parse(raw_body)
        .map_err(|_| RequestError::client("Malformed SOAP XML request."))?;
    let root = document.root_element();

    if local_name(root) != "Envelope" {
        return Err(RequestError::client("SOAP Envelope element is missing."));
    }

    let body = root
        .descendants()
        .find(|node| node.is_element() && local_name(*node) == "Body")
        .ok_or_else(|| RequestError::client("SOAP Body element is missing."))?;

    let mut payloads = body.children().filter(roxmltree::Node::is_element);
    let (Some(payload), None) = (payloads.next(), payloads.next()) else {
        return Err(RequestError::client(
            "SOAP Body must contain exactly one business payload element.",
        ));
    };

    // The assigning authority: sending application, then facility, then the
    // patient identifier's authority. The first that has a value.
    let candidates: [&[&str]; 3] = [
        &["MSH", "MSH.3", "HD.1"],
        &["MSH", "MSH.4", "HD.1"],
        &["PID", "PID.3", "CX.4", "HD.1"],
    ];
    let source = candidates
        .iter()
        .find_map(|path| find_first_text(payload, path))
        .ok_or_else(|| {
            RequestError::new(
                "Client.Validation",
                "Unable to determine assigning authority from payload.",
                400,
            )
        })?;

    Ok(Extracted {
        payload_xml: standalone(raw_body, payload),
        structure_id: Some(local_name(payload).to_string()),
        source_identifier: Some(source),
        message_control_id: find_first_text(payload, &["MSH", "MSH.10"]),
        // MSH.9.2, the trigger event; for subscription filtering (§10).
        message_type: find_first_text(payload, &["MSH", "MSH.9", "MSG.2"]),
    })
}

/// `xml_raw_adapter.py :: XmlRawContentAdapter.extract` (`spec/19-rest-servers.md`
/// §9.4, `xml-raw`).
///
/// The whole body is the payload and is kept byte for byte, since the
/// message store records it. The locators are optional, so the source
/// identifier and control id may be absent; `message_type` never is set
/// here (§10 applies to the `soap` adapter only).
///
/// # Errors
///
/// A `Client` 400 when the body is not well-formed XML.
fn extract_xml_raw(
    raw_body: &str,
    source_identifier_path: Option<&[String]>,
    message_control_id_path: Option<&[String]>,
) -> Result<Extracted, RequestError> {
    let document =
        Document::parse(raw_body).map_err(|_| RequestError::client("Malformed XML request."))?;
    let root = document.root_element();

    Ok(Extracted {
        payload_xml: raw_body.to_string(),
        structure_id: Some(local_name(root).to_string()),
        source_identifier: source_identifier_path.and_then(|path| find_first_text(root, path)),
        message_control_id: message_control_id_path.and_then(|path| find_first_text(root, path)),
        message_type: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SOAP request with an HL7 payload.
    fn soap(payload: &str) -> String {
        format!(
            "<soapenv:Envelope xmlns:soapenv=\"{SOAP_NS}\"><soapenv:Body>{payload}</soapenv:Body></soapenv:Envelope>"
        )
    }

    const ADT: &str = r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><MSH><MSH.3><HD.1>328</HD.1></MSH.3><MSH.9><MSG.1>ADT</MSG.1><MSG.2>A31</MSG.2></MSH.9><MSH.10>MSG-1</MSH.10></MSH></ADT_A05>"#;

    fn xml_raw() -> ContentAdapter {
        ContentAdapter::XmlRaw {
            source_identifier_path: Some(vec!["Header".into(), "SourceSystem".into()]),
            message_control_id_path: Some(vec!["Header".into(), "MessageId".into()]),
        }
    }

    #[test]
    fn soap_extracts_the_payload_and_its_routing_metadata() {
        // test_content_adapters.py — the soap success case.
        let extracted = ContentAdapter::Soap.extract(&soap(ADT)).unwrap();
        assert_eq!(extracted.structure_id.as_deref(), Some("ADT_A05"));
        assert_eq!(extracted.source_identifier.as_deref(), Some("328"));
        assert_eq!(extracted.message_control_id.as_deref(), Some("MSG-1"));
        assert_eq!(extracted.message_type.as_deref(), Some("A31"));
        assert!(extracted.payload_xml.starts_with("<ADT_A05"));
    }

    #[test]
    fn soap_message_type_is_absent_when_the_payload_has_none() {
        // spec/19 §10.1 — absent, not empty.
        let no_type = ADT.replace("<MSG.2>A31</MSG.2>", "");
        let extracted = ContentAdapter::Soap.extract(&soap(&no_type)).unwrap();
        assert_eq!(extracted.message_type, None);
    }

    #[test]
    fn soap_authority_falls_back_to_facility_then_to_the_patient_authority() {
        // test_content_adapters.py — the three candidate paths, in order.
        let facility = ADT.replace(
            "<MSH.3><HD.1>328</HD.1></MSH.3>",
            "<MSH.4><HD.1>111</HD.1></MSH.4>",
        );
        assert_eq!(
            ContentAdapter::Soap
                .extract(&soap(&facility))
                .unwrap()
                .source_identifier
                .as_deref(),
            Some("111")
        );

        let patient = r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><MSH><MSH.10>M</MSH.10></MSH><PID><PID.3><CX.4><HD.1>222</HD.1></CX.4></PID.3></PID></ADT_A05>"#;
        assert_eq!(
            ContentAdapter::Soap
                .extract(&soap(patient))
                .unwrap()
                .source_identifier
                .as_deref(),
            Some("222")
        );
    }

    #[test]
    fn soap_without_any_authority_is_a_validation_failure() {
        let bare = r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><MSH><MSH.10>M</MSH.10></MSH></ADT_A05>"#;
        let error = ContentAdapter::Soap.extract(&soap(bare)).unwrap_err();
        assert_eq!(error.code, "Client.Validation");
        assert_eq!(error.status, 400);
        assert_eq!(
            error.message,
            "Unable to determine assigning authority from payload."
        );
    }

    #[test]
    fn soap_structural_failures_are_client_400s_with_upstreams_text() {
        // spec/19 §9.4.
        let cases = [
            ("not xml", "Malformed SOAP XML request."),
            ("<Other/>", "SOAP Envelope element is missing."),
            (
                "<Envelope><NoBody/></Envelope>",
                "SOAP Body element is missing.",
            ),
            (
                "<Envelope><Body/></Envelope>",
                "SOAP Body must contain exactly one business payload element.",
            ),
            (
                "<Envelope><Body><a/><b/></Body></Envelope>",
                "SOAP Body must contain exactly one business payload element.",
            ),
        ];
        for (request, message) in cases {
            let error = ContentAdapter::Soap.extract(request).unwrap_err();
            assert_eq!(
                (error.code.as_str(), error.status),
                ("Client", 400),
                "{request}"
            );
            assert_eq!(error.message, message, "{request}");
        }
    }

    #[test]
    fn soap_payload_declared_on_the_envelope_still_parses_standalone() {
        // The real LIMS shape: `ns0` declared on the envelope.
        let request = format!(
            r#"<SOAP-ENV:Envelope xmlns:SOAP-ENV="{SOAP_NS}" xmlns:ns0="urn:hl7-org:v2xml"><SOAP-ENV:Body><ns0:ADT_A05><ns0:MSH><ns0:MSH.3><ns0:HD.1>328</ns0:HD.1></ns0:MSH.3></ns0:MSH></ns0:ADT_A05></SOAP-ENV:Body></SOAP-ENV:Envelope>"#
        );
        let extracted = ContentAdapter::Soap.extract(&request).unwrap();
        assert!(
            roxmltree::Document::parse(&extracted.payload_xml).is_ok(),
            "{}",
            extracted.payload_xml
        );
        assert_eq!(extracted.source_identifier.as_deref(), Some("328"));
    }

    #[test]
    fn soap_responses_are_envelopes_with_escaped_values() {
        // spec/19 §9.4, §9.4.1.
        let success = ContentAdapter::Soap.success_response("A&B");
        assert!(
            success.contains("<MessageControlId>A&amp;B</MessageControlId>"),
            "{success}"
        );
        assert!(success.contains("xmlns:tns=\"urn:inthub:rest-server\""));
        assert!(roxmltree::Document::parse(&success).is_ok());

        let fault = ContentAdapter::Soap.error_response("Client", "bad <thing>");
        assert!(fault.contains("<faultcode>Client</faultcode>"), "{fault}");
        assert!(
            fault.contains("<faultstring>bad &lt;thing&gt;</faultstring>"),
            "{fault}"
        );
        assert!(roxmltree::Document::parse(&fault).is_ok());
    }

    #[test]
    fn xml_raw_uses_the_locators_and_keeps_the_body_verbatim() {
        // test_content_adapters.py — xml-raw.
        let body = "<Doc><Header><SourceSystem>SRC</SourceSystem><MessageId>ID-9</MessageId></Header></Doc>";
        let extracted = xml_raw().extract(body).unwrap();
        assert_eq!(extracted.payload_xml, body);
        assert_eq!(extracted.structure_id.as_deref(), Some("Doc"));
        assert_eq!(extracted.source_identifier.as_deref(), Some("SRC"));
        assert_eq!(extracted.message_control_id.as_deref(), Some("ID-9"));
        assert_eq!(extracted.message_type, None, "spec/19 §10.3");
    }

    #[test]
    fn xml_raw_without_locators_extracts_neither() {
        let plain = ContentAdapter::XmlRaw {
            source_identifier_path: None,
            message_control_id_path: None,
        };
        let extracted = plain.extract("<Doc/>").unwrap();
        assert_eq!(
            (extracted.source_identifier, extracted.message_control_id),
            (None, None)
        );
    }

    #[test]
    fn xml_raw_malformed_is_a_client_400() {
        let error = xml_raw().extract("<a>").unwrap_err();
        assert_eq!((error.code.as_str(), error.status), ("Client", 400));
        assert_eq!(error.message, "Malformed XML request.");
    }

    #[test]
    fn xml_raw_responses_are_the_small_xml_forms() {
        assert_eq!(
            xml_raw().success_response("ID"),
            "<ack><status>Success</status><messageControlId>ID</messageControlId></ack>"
        );
        assert_eq!(
            xml_raw().error_response("Client", "m"),
            "<error><code>Client</code><message>m</message></error>"
        );
        assert_eq!(
            ContentAdapter::Soap.content_type(),
            "text/xml; charset=utf-8"
        );
        assert_eq!(xml_raw().content_type(), "application/xml; charset=utf-8");
    }

    #[test]
    fn external_entities_are_not_resolved() {
        // spec/19 §9.4.2 — the XXE case.
        let body = r#"<!DOCTYPE d [<!ENTITY x SYSTEM "file:///etc/passwd">]><Doc><Header><SourceSystem>&x;</SourceSystem></Header></Doc>"#;
        let outcome = xml_raw().extract(body);
        assert!(
            outcome.is_err()
                || outcome
                    .unwrap()
                    .source_identifier
                    .is_none_or(|s| !s.contains("root:")),
            "the file must never be read"
        );
    }
}
