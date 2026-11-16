//! The payload path the container runs: `Proms` as a `PayloadTransformer`.
//!
//! Implements `spec/16-proms-transformer.md` §1.1 and §2.5; ports upstream's
//! `TestProcessMessageEndToEnd` (`#452`, `4d78384`).
//!
//! Upstream's bug was that the shared processor always ran an HL7 parse, so
//! the PROMs XML failed with "Invalid message" and was dead-lettered. The Rust
//! runtime has had a separate payload path from the start
//! (`ihb_transformer::PayloadTransformer`); this pins that the **real** type
//! the binary runs takes WPAS XML through it, including the one body shape
//! upstream found awkward.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ihb_transformer::PayloadTransformer;
use serde_json::Value;
use xml_fhir_proms_transformer::Proms;

/// The declaration says `utf-16` but the body is UTF-8 text, exactly as WPAS
/// delivers it (upstream `WPAS_UTF16_DECLARED_REFERRAL`).
const UTF16_DECLARED_REFERRAL: &str = r#"<?xml version="1.0" encoding="utf-16"?>
<PromsEventRequest>
<system_id>149</system_id><unique_code>1268889121</unique_code><crn>H9211785</crn>
<nhsNumber>1268889121</nhsNumber><patientTitle>Lady</patientTitle>
<patientFirstname>SCHEMA TEST</patientFirstname><patientMiddlename></patientMiddlename>
<patientSurname>JONES</patientSurname><dob>1977-03-01</dob><dod></dod><gender>F</gender>
<buildingName>1 Ocean Buildings</buildingName><streetRoadName>Bute Crescent</streetRoadName>
<postTown>Cardiff</postTown><postalCounty>Cardiff</postalCounty><postCode>CF10 5AY</postCode>
<hbCode>X98</hbCode><letterID></letterID>
<preferred_spoken_language_code>9</preferred_spoken_language_code><spoken_language>Unknown</spoken_language>
<preferred_written_language_code>9</preferred_written_language_code><written_language>Unknown</written_language>
<ethnic_origin_code>ZZ</ethnic_origin_code><ethnic_origin>Not stated</ethnic_origin>
<religion_code>Z</religion_code><religion>Not known</religion>
<marital_status_code>9</marital_status_code><marital_status>Not known</marital_status>
<home_phone></home_phone><work_phone></work_phone><mobile_phone>0745592833911</mobile_phone>
<email>ok@aol.com</email><referrer_code>G9505126</referrer_code><referrer_prac>W95065</referrer_prac>
<referrer_name>GRIFFITHS HM</referrer_name>
<referrer_location>Oakfield Surgery         Oakfield Street         Ystrad Mynach          Hengoed</referrer_location>
<referrer_postcode>CF82 7WX</referrer_postcode><referrer_org>4WN58</referrer_org>
<eventCode>REFERRAL</eventCode><eventPathway>GENERIC</eventPathway><eventDate>2026-10-07</eventDate>
<consultant_code>HANJO</consultant_code><clinicianName>Jones, Hannah</clinicianName>
<consultant_specialty>650100</consultant_specialty><specialty_name>MSK (Physiotherapy)</specialty_name>
<main_specialty_name>Physiotherapy</main_specialty_name><pathway>7A22868085</pathway>
<activityNotekey>31689017</activityNotekey>
</PromsEventRequest>"#;

#[test]
fn wpas_xml_becomes_a_bundle_through_the_payload_path() {
    let output = Proms::from_env()
        .transform_payload(UTF16_DECLARED_REFERRAL)
        .expect("the PROMs XML is not run through an HL7 parser");

    let bundle: Value = serde_json::from_str(&output).expect("the output is JSON, not ER7");
    assert_eq!(bundle["resourceType"], "Bundle");
    assert_eq!(bundle["type"], "message");
}

#[test]
fn a_body_that_is_not_proms_xml_is_a_transform_failure_not_a_panic() {
    for body in ["", "MSH|^~\\&|a|b", "<not-proms/>", "<PromsEventRequest>"] {
        assert!(
            Proms::from_env().transform_payload(body).is_err(),
            "{body:?}"
        );
    }
}
