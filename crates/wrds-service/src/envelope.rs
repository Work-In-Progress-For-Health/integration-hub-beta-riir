//! The request and response documents.
//!
//! Implements `spec/09-transformers.md` §9.2 and §9.3.

use std::fmt::Write as _;

use roxmltree::{Document, Node};

use crate::{Error, Row};

/// The SOAP 1.1 envelope namespace.
const SOAP_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
/// The namespace of the operation and its response.
const MES_NS: &str = "http://www.wales.nhs.uk/namespaces/MessageRelease2";
/// The namespace every attribute name and the lookup table sit in.
const NRDS_NS: &str = "http://www.wales.nhs.uk/nrds";

/// Escape `&`, `<` and `>` (§9.2).
///
/// Nothing here is placed in an attribute, so quotes need no treatment.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The `GetResultSetRequest` envelope.
///
/// # Examples
///
/// ```
/// let xml = wrds_service::build_request(
///     "FioranoCodeTranslation",
///     &[("FromSystem", "349")],
///     &["FromCode"],
///     true,
/// );
/// assert!(xml.contains("<Name>FromSystem</Name>"));
/// assert!(xml.contains("<Value>349</Value>"));
/// assert!(xml.contains("<ExactMatch>1</ExactMatch>"));
/// ```
#[must_use]
pub fn build_request(
    lookup_table: &str,
    attributes: &[(&str, &str)],
    retrieve: &[&str],
    exact_match: bool,
) -> String {
    let mut pairs = String::new();
    for (name, value) in attributes {
        let _ = write!(
            pairs,
            "\n      <AttributeValuePair>\n        <Attribute>\n          <Id></Id>\n          \
             <Name>{}</Name>\n          <Namespace>{NRDS_NS}</Namespace>\n          \
             <Description></Description>\n        </Attribute>\n        \
             <Value>{}</Value>\n      </AttributeValuePair>",
            escape(name),
            escape(value)
        );
    }
    let mut wanted = String::new();
    for name in retrieve {
        let _ = write!(
            wanted,
            "\n      <AttributeToRetrieve>\n        <Id></Id>\n        <Name>{}</Name>\n        \
             <Namespace>{NRDS_NS}</Namespace>\n      </AttributeToRetrieve>",
            escape(name)
        );
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <soap:Envelope xmlns:soap=\"{SOAP_NS}\">\n  <soap:Body>\n    \
         <GetResultSetRequest xmlns=\"{MES_NS}\">{pairs}\n      <LookupTable>\n        <Id></Id>\n        \
         <Name>{}</Name>\n        <Namespace>{NRDS_NS}</Namespace>\n      </LookupTable>{wanted}\n      \
         <ExactMatch>{}</ExactMatch>\n    </GetResultSetRequest>\n  </soap:Body>\n</soap:Envelope>",
        escape(lookup_table),
        i32::from(exact_match)
    )
}

/// The first child of `node` in `namespace` named `local`.
fn child<'a, 'input>(
    node: Node<'a, 'input>,
    namespace: &str,
    local: &str,
) -> Option<Node<'a, 'input>> {
    node.children().find(|candidate| {
        candidate.is_element()
            && candidate.tag_name().name() == local
            && candidate.tag_name().namespace() == Some(namespace)
    })
}

/// The text of `node`, or empty.
fn text(node: Node<'_, '_>) -> String {
    node.text().unwrap_or_default().to_string()
}

/// Parse a `GetResultSetResponse` envelope into rows.
///
/// A DTD is refused (§9.3): `roxmltree` does not expand external entities,
/// and its default options reject a document type declaration outright,
/// which is the posture the Python took with `defusedxml`.
///
/// # Errors
///
/// [`Error::Malformed`] for XML that does not parse, has no SOAP body, or
/// has no `GetResultSetResponse`; [`Error::Fault`] for a SOAP fault.
pub fn parse_response(body: &str) -> Result<Vec<Row>, Error> {
    // The error text of the parser names a line and column, never content.
    let document = Document::parse(body)
        .map_err(|error| Error::Malformed(format!("not valid XML: {error}")))?;

    let soap_body = child(document.root_element(), SOAP_NS, "Body")
        .ok_or_else(|| Error::Malformed("missing SOAP Body".to_string()))?;

    if let Some(fault) = child(soap_body, SOAP_NS, "Fault").or_else(|| {
        // `faultstring` is unqualified, but the Fault itself is the SOAP one.
        soap_body
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "Fault")
    }) {
        let faultstring = fault
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "faultstring")
            .map(text)
            .unwrap_or_default();
        return Err(Error::Fault(faultstring));
    }

    let response = child(soap_body, MES_NS, "GetResultSetResponse")
        .ok_or_else(|| Error::Malformed("missing GetResultSetResponse".to_string()))?;

    let mut rows = Vec::new();
    for row in response.children().filter(|n| {
        n.is_element() && n.tag_name().name() == "Row" && n.tag_name().namespace() == Some(MES_NS)
    }) {
        let mut values = Row::new();
        for pair in row.children().filter(|n| {
            n.is_element()
                && n.tag_name().name() == "AttributeValuePair"
                && n.tag_name().namespace() == Some(MES_NS)
        }) {
            let name = child(pair, MES_NS, "Attribute")
                .and_then(|attribute| child(attribute, MES_NS, "Name"))
                .map(text)
                .unwrap_or_default();
            let value = child(pair, MES_NS, "Value").map(text).unwrap_or_default();
            if !name.is_empty() {
                values.insert(name, value);
            }
        }
        rows.push(values);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESPONSE: &str = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GetResultSetResponse xmlns="http://www.wales.nhs.uk/namespaces/MessageRelease2">
      <Row>
        <AttributeValuePair><Attribute><Id></Id><Name>FromCode</Name><Namespace>n</Namespace></Attribute><Value>1</Value></AttributeValuePair>
        <AttributeValuePair><Attribute><Id></Id><Name>Type</Name><Namespace>n</Namespace></Attribute><Value>Sex</Value></AttributeValuePair>
        <AttributeValuePair><Attribute><Id></Id><Name>ToCode</Name><Namespace>n</Namespace></Attribute><Value>M</Value></AttributeValuePair>
      </Row>
      <Row>
        <AttributeValuePair><Attribute><Name>FromCode</Name></Attribute><Value>2</Value></AttributeValuePair>
        <AttributeValuePair><Attribute><Name></Name></Attribute><Value>nameless</Value></AttributeValuePair>
        <AttributeValuePair><Attribute><Name>ToCode</Name></Attribute></AttributeValuePair>
      </Row>
    </GetResultSetResponse>
  </soap:Body>
</soap:Envelope>"#;

    #[test]
    fn the_request_carries_every_part_of_the_operation() {
        // §9.2.
        let xml = build_request(
            "FioranoCodeTranslation",
            &[("FromSystem", "349"), ("ToSystem", "100")],
            &["FromCode", "type", "ToCode"],
            true,
        );
        let document = Document::parse(&xml).expect("the request is well-formed XML");
        let names: Vec<_> = document
            .descendants()
            .filter(|n| n.tag_name().name() == "Name")
            .map(|n| text(n))
            .collect();

        assert_eq!(
            names,
            [
                "FromSystem",
                "ToSystem",
                "FioranoCodeTranslation",
                "FromCode",
                "type",
                "ToCode"
            ]
        );
        assert!(xml.contains("<Value>349</Value>") && xml.contains("<Value>100</Value>"));
        assert!(xml.contains("<ExactMatch>1</ExactMatch>"));
        assert!(xml.contains(NRDS_NS) && xml.contains(MES_NS) && xml.contains(SOAP_NS));
    }

    #[test]
    fn exact_match_false_is_zero() {
        assert!(build_request("t", &[], &[], false).contains("<ExactMatch>0</ExactMatch>"));
    }

    #[test]
    fn markup_in_a_value_is_escaped() {
        // §9.2 — a value must not be able to alter the document.
        let xml = build_request("t", &[("a", "x</Value><Evil/>&")], &[], true);
        assert!(xml.contains("x&lt;/Value&gt;&lt;Evil/&gt;&amp;"), "{xml}");
        assert!(Document::parse(&xml).is_ok());
    }

    #[test]
    fn rows_are_parsed_by_name_and_value() {
        // §9.3.
        let rows = parse_response(RESPONSE).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["FromCode"], "1");
        assert_eq!(rows[0]["Type"], "Sex");
        assert_eq!(rows[0]["ToCode"], "M");
    }

    #[test]
    fn a_missing_value_is_empty_and_a_nameless_pair_is_skipped() {
        // §9.3.
        let rows = parse_response(RESPONSE).unwrap();
        assert_eq!(rows[1]["ToCode"], "");
        assert_eq!(rows[1].len(), 2, "the nameless pair is dropped");
    }

    #[test]
    fn a_fault_is_an_error_carrying_the_faultstring() {
        // §9.3.
        let fault = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><soap:Fault><faultcode>soap:Server</faultcode><faultstring>table unknown</faultstring></soap:Fault></soap:Body></soap:Envelope>"#;
        let error = parse_response(fault).unwrap_err();
        assert!(
            matches!(&error, Error::Fault(text) if text == "table unknown"),
            "{error}"
        );
    }

    #[test]
    fn a_dtd_is_refused() {
        // §9.3 — the Python's `defusedxml` posture.
        let hostile = r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY e "boom">]><soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body/></soap:Envelope>"#;
        assert!(matches!(parse_response(hostile), Err(Error::Malformed(_))));
    }

    #[test]
    fn not_xml_is_malformed() {
        assert!(matches!(
            parse_response("nonsense"),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn a_response_without_a_body_or_a_result_is_malformed() {
        let no_body = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"/>"#;
        assert!(matches!(parse_response(no_body), Err(Error::Malformed(_))));

        let empty_body = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body/></soap:Envelope>"#;
        assert!(matches!(
            parse_response(empty_body),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn unqualified_result_elements_are_not_accepted() {
        // The real service qualifies them; the Python looked them up
        // namespace-qualified, so an unqualified one is "missing".
        let unqualified = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><GetResultSetResponse/></soap:Body></soap:Envelope>"#;
        assert!(matches!(
            parse_response(unqualified),
            Err(Error::Malformed(_))
        ));
    }
}
