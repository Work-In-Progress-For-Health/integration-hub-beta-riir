//! The revised Referral mapping, one clause per test.
//!
//! Implements `spec/16-proms-transformer.md` §5, §6.2 to §6.11 and §10, as
//! ported from upstream's `tests/test_proms_transformer.py` (#346, #370,
//! #423). `tests/golden.rs` already proves whole bundles equal the
//! Python's; these name the individual rules, so a future reader knows
//! which clause a failing assertion protects and whether it may be deleted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use serde_json::Value;
use xml_fhir_proms_transformer::identity::SequentialUuids;
use xml_fhir_proms_transformer::reference_data::StaticResolver;
use xml_fhir_proms_transformer::transform::transform;

fn fixture(name: &str) -> String {
    let all: BTreeMap<String, String> =
        serde_json::from_str(include_str!("corpus/wpas-messages.json")).unwrap();
    all[name].clone()
}

fn build_with(xml: &str, destination: Option<&str>) -> Value {
    transform(
        xml,
        &SequentialUuids::new(),
        &StaticResolver,
        &|_| None,
        destination,
    )
    .unwrap()
}

fn build(xml: &str) -> Value {
    build_with(xml, None)
}

/// The resource at `index` of the bundle's entries.
fn at(bundle: &Value, index: usize) -> Value {
    bundle["entry"][index]["resource"].clone()
}

fn referral() -> Value {
    build(&fixture("REFERRAL_MESSAGE"))
}

#[test]
fn destination_endpoint_can_be_overridden() {
    // §6.2, §10.3 — upstream `test_destination_endpoint_can_be_overridden_by_environment`.
    let bundle = build_with(
        &fixture("REFERRAL_MESSAGE"),
        Some("https://promptly.example/fhir"),
    );
    assert_eq!(
        at(&bundle, 0)["destination"][0]["endpoint"],
        "https://promptly.example/fhir"
    );
    assert!(
        at(&referral(), 0)["destination"][0]["endpoint"]
            .as_str()
            .unwrap()
            .ends_with("/fhir/r4/$process-message")
    );
}

#[test]
fn source_has_name_and_endpoint_for_a_known_board() {
    // §5.2, §5.2.2.
    let source = &at(&referral(), 0)["source"];
    assert_eq!(source["name"], "Hywel Dda University Health Board");
    assert_eq!(
        source["endpoint"],
        "https://fhir.nhs.uk/source-system/wpas/149"
    );
}

#[test]
fn sender_display_is_the_organization_name() {
    // §6.2, §6.10.
    let bundle = referral();
    assert_eq!(at(&bundle, 0)["sender"]["display"], at(&bundle, 5)["name"]);
    assert_eq!(at(&bundle, 5)["name"], "Hywel Dda University Health Board");
}

#[test]
fn patient_is_active_with_the_wpas_profile_and_a_joined_given_name() {
    // §6.3.
    let patient = at(&referral(), 1);
    assert_eq!(patient["active"], true);
    assert_eq!(
        patient["meta"]["profile"][0],
        "https://fhir.nhs.wales/StructureDefinition/wpas-patient"
    );
    assert_eq!(
        patient["name"][0]["given"],
        serde_json::json!(["Aneurin Nye"])
    );
    assert!(
        patient["name"][0].get("prefix").is_none(),
        "title is not mapped"
    );
}

#[test]
fn patient_telecom_maps_phones_and_email_in_order() {
    // §6.3 — upstream `test_telecom_maps_phone_numbers_and_email`.
    let patient = at(&referral(), 1);
    let telecom: Vec<(String, Option<String>, String)> = patient["telecom"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["system"].as_str().unwrap().to_string(),
                t["use"].as_str().map(ToString::to_string),
                t["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();

    assert_eq!(
        telecom,
        vec![
            ("phone".into(), Some("home".into()), "07555566771-2".into()),
            ("phone".into(), Some("work".into()), "07555566771-1".into()),
            (
                "phone".into(),
                Some("mobile".into()),
                "07555566771-3".into()
            ),
            ("email".into(), None, "simphel@aol.com".into()),
        ]
    );

    let minimal = at(&build(&fixture("MINIMAL_REFERRAL_MESSAGE")), 1);
    assert!(minimal.get("telecom").is_none());
}

#[test]
fn ethnic_category_extension_carries_code_display_and_text() {
    // §6.3.
    let extension = &at(&referral(), 1)["extension"][0];
    let concept = &extension["valueCodeableConcept"];
    assert_eq!(
        extension["url"],
        "https://fhir.hl7.org.uk/StructureDefinition/Extension-UKCore-EthnicCategory"
    );
    assert_eq!(
        concept["coding"][0]["system"],
        "https://fhir.nhs.wales/CodeSystem/wpas-ethnicity"
    );
    assert_eq!(concept["coding"][0]["code"], "A");
    assert_eq!(concept["coding"][0]["display"], "White - British");
    assert_eq!(concept["text"], "White - British");
}

#[test]
fn communication_is_the_spoken_language_as_sent() {
    // §6.3.2 — retires §9.9.
    let patient = at(&referral(), 1);
    let coding = &patient["communication"][0]["language"]["coding"][0];
    assert_eq!(
        coding["system"],
        "https://fhir.nhs.wales/CodeSystem/wpas-spoken-language"
    );
    assert_eq!(coding["code"], "CY");
    assert_eq!(coding["display"], "Welsh");
    assert!(patient["communication"][0].get("preferred").is_none());

    let minimal = at(&build(&fixture("MINIMAL_REFERRAL_MESSAGE")), 1);
    assert!(minimal.get("communication").is_none());
}

#[test]
fn deceased_follows_dod() {
    // §6.3.3 — upstream `test_deceased_*`.
    let base = fixture("REFERRAL_MESSAGE");
    let with = |tag: &str| {
        base.replace(
            "</PromsEventRequest>",
            &format!("{tag}</PromsEventRequest>"),
        )
    };

    let none = at(&referral(), 1);
    assert!(none.get("deceasedDateTime").is_none() && none.get("deceasedBoolean").is_none());

    let dead = at(&build(&with("<DOD>2024-05-01</DOD>")), 1);
    assert_eq!(dead["deceasedDateTime"], "2024-05-01T00:00:00Z");
    assert!(dead.get("deceasedBoolean").is_none());

    let empty = at(&build(&with("<DOD></DOD>")), 1);
    assert!(empty.get("deceasedDateTime").is_none() && empty.get("deceasedBoolean").is_none());

    // Non-empty but unparseable: deceased, date unknown (never silently alive).
    let unknown = at(&build(&with("<DOD>sometime</DOD>")), 1);
    assert_eq!(unknown["deceasedBoolean"], true);
}

#[test]
fn service_request_is_an_order_with_event_code_and_pathway_reason() {
    // §6.4.
    let request = at(&referral(), 2);
    assert_eq!(request["intent"], "order");
    assert_eq!(request["doNotPerform"], false);
    assert_eq!(request["identifier"][0]["value"], "7A22867970");
    assert_eq!(
        request["identifier"][0]["system"],
        "https://fhir.nhs.wales/Id/wpas-servicerequest"
    );

    let code = &request["code"]["coding"][0];
    assert_eq!(
        code["system"],
        "https://fhir.nhs.wales/CodeSystem/proms-event"
    );
    assert_eq!(code["code"], "REFERRAL");
    assert_eq!(code["display"], "Referral");

    let reason = &request["reasonCode"][0]["coding"][0];
    assert_eq!(
        reason["system"],
        "https://fhir.nhs.wales/CodeSystem/wpas-event-pathway"
    );
    assert_eq!(reason["code"], "ORTHO");
    assert!(reason.get("display").is_none());
}

#[test]
fn service_request_code_survives_a_legacy_routing_field() {
    // §6.4 — upstream `test_code_present_when_routed_by_message_type_alias`.
    let xml = fixture("REFERRAL_MESSAGE").replace(
        "<eventCode>REFERRAL</eventCode>",
        "<MESSAGE_TYPE>REFERRAL</MESSAGE_TYPE>",
    );
    let request = at(&build(&xml), 2);
    assert_eq!(request["code"]["coding"][0]["code"], "REFERRAL");
}

#[test]
fn practitioner_role_is_the_referrer_under_gmc_with_a_three_character_specialty() {
    // §6.8.
    let role = at(&referral(), 3);
    assert_eq!(
        role["identifier"][0]["system"],
        "https://fhir.hl7.org.uk/Id/gmc-number"
    );
    assert_eq!(role["identifier"][0]["value"], "G7654321");

    let longer = fixture("REFERRAL_MESSAGE").replace(">110<", ">110A<");
    assert_eq!(
        at(&build(&longer), 3)["specialty"][0]["coding"][0]["code"],
        "110"
    );
}

#[test]
fn practitioner_has_two_identifiers_for_a_referral_and_free_text_name() {
    // §6.9.
    let practitioner = at(&referral(), 4);
    let identifiers: BTreeMap<String, String> = practitioner["identifier"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["system"].as_str().unwrap().into(),
                i["value"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        identifiers["https://fhir.nhs.wales/Id/wpas-practitioner"],
        "HANJO"
    );
    assert_eq!(
        identifiers["https://fhir.hl7.org.uk/Id/gmc-number"],
        "G7654321"
    );
    assert_eq!(practitioner["name"][0]["text"], "ARDERN-JONES L");
    assert!(practitioner["name"][0].get("family").is_none());

    let surgery = at(&build(&fixture("SURGERY_MESSAGE")), 3);
    assert_eq!(surgery["identifier"].as_array().unwrap().len(), 1);
    assert_eq!(surgery["name"][0]["text"], "Morgan, James");
}

#[test]
fn organization_is_the_health_board_from_hb_code() {
    // §6.10, §6.10.1.
    let org = at(&referral(), 5);
    assert_eq!(org["active"], true);
    assert_eq!(org["identifier"][0]["value"], "X98");

    // Without a system_id the name falls back to the ODS-code lookup.
    let xml = fixture("REFERRAL_MESSAGE")
        .replace("<system_id>149</system_id>", "")
        .replace("X98", "7A2");
    let org = at(&build(&xml), 5);
    assert_eq!(org["identifier"][0]["value"], "7A2");
    assert_eq!(org["name"], "HYWEL DDA UNIVERSITY LHB");

    // `dhaCode` and `referrer_org` are ignored; no hbCode means no entry.
    let xml = fixture("REFERRAL_MESSAGE")
        .replace("<hbCode>X98</hbCode>", "")
        .replace(
            "<referrer_org></referrer_org>",
            "<referrer_org>Some Practice</referrer_org>",
        );
    let types: Vec<String> = build(&xml)["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["resource"]["resourceType"].as_str().unwrap().to_string())
        .collect();
    assert!(!types.contains(&"Organization".to_string()));
}

#[test]
fn location_has_no_name_and_joins_location_and_postcode() {
    // §6.11.
    let location = at(&referral(), 6);
    assert!(location.get("name").is_none());
    assert_eq!(location["status"], "active");
    assert_eq!(location["identifier"][0]["value"], "W95001");
    assert_eq!(
        location["address"]["text"],
        "YSBYTY GWYNEDD, PENRHOSGARNEDD, BANGOR, GWYNEDD, LL57 2PW LL57 2PW"
    );
    assert_eq!(location["address"]["postalCode"], "LL57 2PW");
}

#[test]
fn legacy_bare_codes_keep_their_code_but_take_the_original_scenario() {
    // §2.4.1, §10.2.
    let header = at(&build(&fixture("SURGERY_MESSAGE")), 0);
    assert_eq!(header["eventCoding"]["code"], "SURGERY");
    assert_eq!(header["eventCoding"]["display"], "Procedure Performed");

    let header = at(&build(&fixture("PREOP_AS_MESSAGE")), 0);
    assert_eq!(header["eventCoding"]["code"], "PREOP-AS");
}

#[test]
fn appointment_patient_participant_status_follows_confirm_appt() {
    // §6.6 — `Y` accepted, `N` declined, otherwise tentative.
    let base = fixture("PREOP_AS_MESSAGE");
    let status = |tag: &str| {
        let xml = base.replace(
            "</PromsEventRequest>",
            &format!("{tag}</PromsEventRequest>"),
        );
        build(&xml)["entry"][2]["resource"]["participant"][0]["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status("<CONFIRM_APPT>Y</CONFIRM_APPT>"), "accepted");
    assert_eq!(status("<CONFIRM_APPT>n</CONFIRM_APPT>"), "declined");
    assert_eq!(status(""), "tentative");
}
