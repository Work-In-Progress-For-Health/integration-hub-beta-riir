//! The SOAP ingress path, against the real LIMS payload.
//!
//! Implements `spec/06-validation.md` §5.8 and §5.9.
//!
//! `hl7_soap_server` takes a v2.xml payload out of a SOAP envelope and
//! forwards it as ER7. Everything MPI receives from LIMS goes through that
//! conversion, so it gets its own test file against the actual sample rather
//! than a synthetic one — the sample is padded and namespace-prefixed, and
//! both of those turned out to matter.

// An integration test is its own crate, so the crate-level allow in
// `src/lib.rs` does not reach it. Asserting is what this file is for.
// See spec/17-testing-and-quality.md §2.3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ihb_validation::convert::xml_to_er7;

/// The real payload, as captured from the LIMS flow.
const SOAP: &str = include_str!("corpus/lims-to-mpi.sample.xml");

/// The business payload, out of the SOAP body.
fn payload() -> &'static str {
    let start = SOAP
        .find("<ns0:ADT_A05")
        .expect("the sample carries an ADT_A05");
    let end = SOAP.find("</ns0:ADT_A05>").expect("and closes it") + "</ns0:ADT_A05>".len();
    &SOAP[start..end]
}

#[test]
fn a_namespace_prefixed_payload_converts() {
    // spec/06-validation.md §5.8. The sample uses `ns0:`, and the converter
    // reads element names verbatim, so without normalisation every LIMS
    // message would be rejected outright.
    let er7 = xml_to_er7(payload()).expect("the real LIMS payload converts");
    assert!(er7.starts_with("MSH|^~\\&"), "{er7}");
}

#[test]
fn component_positions_survive_the_conversion() {
    // spec/06-validation.md §5.9 — the finding that matters most here.
    //
    // PID-3 in the source is `B0000010612^^^328^PI`: the identifier in
    // CX.1, the assigning authority in CX.4, the type in CX.5. If empty
    // components are dropped positionally rather than by name, 328 lands in
    // CX.2 and MPI reads a patient identifier as an authority code.
    let er7 = xml_to_er7(payload()).unwrap();
    let message = hl7_2::parse(&er7).unwrap();

    assert_eq!(
        message.get("PID-3.1").unwrap().as_deref(),
        Some("B0000010612"),
        "the patient identifier"
    );
    assert_eq!(
        message.get("PID-3.4.1").unwrap().as_deref(),
        Some("328"),
        "the assigning authority must stay in CX.4"
    );
    assert_eq!(
        message.get("PID-3.5").unwrap().as_deref(),
        Some("PI"),
        "the identifier type must stay in CX.5"
    );
}

#[test]
fn padding_does_not_become_a_field_full_of_deletion_markers() {
    // spec/06-validation.md §5.8. The sample is schema-shaped, so nearly
    // every declared position is present and empty. Rendering those as HL7
    // explicit nulls would tell MPI to delete most of the record.
    let er7 = xml_to_er7(payload()).unwrap();
    assert!(
        !er7.contains(r#""""#),
        "explicit nulls leaked into the forwarded message:\n{er7}"
    );
}

#[test]
fn the_forwarded_message_carries_what_the_sender_sent() {
    // The values a human would check first.
    let er7 = xml_to_er7(payload()).unwrap();
    let message = hl7_2::parse(&er7).unwrap();

    assert_eq!(message.get("MSH-3.1").unwrap().as_deref(), Some("328"));
    assert_eq!(
        message.get("MSH-10").unwrap().as_deref(),
        Some("6778031837018553261z82215")
    );
    assert_eq!(message.get("MSH-9.3").unwrap().as_deref(), Some("ADT_A05"));
    assert_eq!(
        message.get("PID-5.1.1").unwrap().as_deref(),
        Some("Lims2val")
    );
    assert_eq!(message.get("PID-7.1").unwrap().as_deref(), Some("20010909"));
}

#[test]
fn the_address_keeps_its_component_positions() {
    // PID-11 in the source is
    // `Welsh Parliament^Cardiff Bay^CARDIFF^^CF99 1SN`: the postcode is
    // XAD.5, with XAD.4 empty. Dropping the empty one positionally would
    // move a postcode into the county field.
    let er7 = xml_to_er7(payload()).unwrap();
    let message = hl7_2::parse(&er7).unwrap();

    assert_eq!(
        message.get("PID-11.3").unwrap().as_deref(),
        Some("CARDIFF"),
        "the city"
    );
    assert_eq!(
        message.get("PID-11.5").unwrap().as_deref(),
        Some("CF99 1SN"),
        "the postcode must stay in XAD.5"
    );
}
