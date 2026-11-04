//! Ported from `test_detect.py` and `test_pretty_print.py`.
//! `spec/ultra7-test-message-sender.md` `§5`.
#![allow(missing_docs, clippy::unwrap_used)]

use ultra7::format::{detect_format, pretty_print};
use ultra7::model::MessageFormat::{Hl7, Json, Xml};

#[test]
fn detects_json_object_and_array() {
    assert_eq!(detect_format(r#"{"a": 1}"#), Json);
    assert_eq!(detect_format("[1, 2, 3]"), Json);
}

#[test]
fn detects_xml_with_and_without_declaration() {
    assert_eq!(detect_format("<root><child/></root>"), Xml);
    assert_eq!(detect_format(r#"<?xml version="1.0"?><root/>"#), Xml);
}

#[test]
fn falls_back_to_hl7() {
    let c = "MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20250703120000||ADT^A01|MSG1|P|2.5";
    assert_eq!(detect_format(c), Hl7);
}

#[test]
fn empty_input_defaults_to_hl7() {
    assert_eq!(detect_format(""), Hl7);
    assert_eq!(detect_format("   "), Hl7);
}

#[test]
fn malformed_json_and_xml_fall_back() {
    assert_eq!(detect_format("{not valid json"), Hl7);
    assert_eq!(detect_format("<root><unclosed>"), Hl7);
}

#[test]
fn indents_compact_json() {
    let r = pretty_print(r#"{"a":1,"b":[1,2,3]}"#, Json).unwrap();
    assert_eq!(
        r,
        "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2,\n    3\n  ]\n}"
    );
}

#[test]
fn invalid_json_is_an_error() {
    assert!(
        pretty_print("{not valid", Json)
            .unwrap_err()
            .starts_with("Invalid JSON")
    );
}

#[test]
fn json_numbers_round_trip() {
    // `§5.3`: arbitrary precision keeps a 25-digit number exact.
    let r = pretty_print(r#"{"n":1234567890123456789012345}"#, Json).unwrap();
    assert!(r.contains("1234567890123456789012345"));
}

#[test]
fn indents_compact_xml() {
    let r = pretty_print("<root><child>value</child></root>", Xml).unwrap();
    assert_eq!(r, "<root>\n  <child>value</child>\n</root>");
}

#[test]
fn xml_keeps_attributes_namespaces_and_escapes() {
    let r = pretty_print(
        r#"<a:r xmlns:a="urn:x" k="1&amp;2"><a:c/>t &lt; u<!--c--></a:r>"#,
        Xml,
    )
    .unwrap();
    assert_eq!(
        r,
        "<a:r xmlns:a=\"urn:x\" k=\"1&amp;2\">\n  <a:c/>\n  t &lt; u\n  <!--c-->\n</a:r>"
    );
}

#[test]
fn invalid_xml_is_an_error() {
    assert!(
        pretty_print("<root><unclosed>", Xml)
            .unwrap_err()
            .starts_with("Invalid XML")
    );
}

#[test]
fn hl7_is_unsupported() {
    assert!(pretty_print("MSH|^~\\&|...", Hl7).is_err());
}
