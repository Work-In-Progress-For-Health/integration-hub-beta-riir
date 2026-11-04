//! Normalising `messageContent` to ER7.
//!
//! Implements `spec/19-rest-servers.md` §2.2.
//!
//! The endpoint accepts either a raw ER7 message or an HL7 v2.xml document
//! (`urn:hl7-org:v2xml`). Whichever it is, what leaves here has `\r` segment
//! terminators, because that is what the rest of the pipeline relies on.

use ihb_validation::{ValidationError, xml_to_er7};

/// Line endings first (`\r\n`, then any bare `\n`, to `\r`), then, if the
/// trimmed content starts with `<`, v2.xml to ER7.
///
/// # Errors
///
/// [`ValidationError::Xml`] when the content looks like XML and does not
/// convert. The caller answers that as an unparsable message
/// (`spec/19-rest-servers.md` §2.2), not as a bad request.
///
/// # Examples
///
/// ```
/// use hl7_rest_server::input::to_er7;
///
/// assert_eq!(to_er7("MSH|^~\\&|A|B\r\nPID|1")?, "MSH|^~\\&|A|B\rPID|1");
/// assert_eq!(to_er7("MSH|^~\\&|A|B\nPID|1")?, "MSH|^~\\&|A|B\rPID|1");
/// assert_eq!(to_er7("")?, "");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
pub fn to_er7(content: &str) -> Result<String, ValidationError> {
    let normalised = content.replace("\r\n", "\r").replace('\n', "\r");
    if normalised.trim_start().starts_with('<') {
        // `xml_to_er7` emits `\r`-terminated segments.
        return xml_to_er7(&normalised);
    }
    Ok(normalised)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn er7_passthrough_normalises_crlf() {
        // test_message_input_adapter.py::test_er7_passthrough_normalises_crlf
        assert_eq!(
            to_er7("MSH|^~\\&|A|B\r\nPID|1").unwrap(),
            "MSH|^~\\&|A|B\rPID|1"
        );
    }

    #[test]
    fn er7_passthrough_normalises_lf() {
        assert_eq!(
            to_er7("MSH|^~\\&|A|B\nPID|1").unwrap(),
            "MSH|^~\\&|A|B\rPID|1"
        );
    }

    #[test]
    fn er7_already_cr_is_unchanged() {
        let content = "MSH|^~\\&|A|B\rPID|1";
        assert_eq!(to_er7(content).unwrap(), content);
    }

    #[test]
    fn xml_is_converted_to_er7() {
        // test_message_input_adapter.py::test_xml_content_is_delegated_to_converter
        let xml = r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><MSH><MSH.1>|</MSH.1><MSH.2>^~\&amp;</MSH.2>
            <MSH.9><MSG.1>ADT</MSG.1><MSG.2>A28</MSG.2></MSH.9><MSH.10>M1</MSH.10></MSH></ADT_A05>"#;
        let er7 = to_er7(xml).unwrap();
        assert!(er7.starts_with("MSH|^~\\&"), "{er7}");
        assert!(er7.contains("ADT^A28"), "{er7}");
    }

    #[test]
    fn xml_with_leading_whitespace_is_detected() {
        // test_message_input_adapter.py::test_xml_with_leading_whitespace_is_detected
        // — proven by the failure: a non-XML path would have returned Ok.
        assert!(to_er7("   \n<ADT_A28/>").is_err());
    }

    #[test]
    fn empty_content_is_treated_as_er7() {
        assert_eq!(to_er7("").unwrap(), "");
    }
}
