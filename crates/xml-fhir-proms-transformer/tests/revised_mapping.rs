//! The revised Referral mapping, one clause per test.
//!
//! Implements `spec/16-proms-transformer.md` §5, §6.2 to §6.11 and §10, as
//! ported from upstream's `tests/test_proms_transformer.py` (#346, #370,
//! #423). `tests/golden.rs` already proves whole bundles equal the
//! Python's; these name the individual rules, so a future reader knows
//! which clause a failing assertion protects and whether it may be deleted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty,
    clippy::unused_async_trait_impl,
    clippy::missing_docs_in_private_items,
    clippy::needless_pass_by_value,
    clippy::needless_collect
)]

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
    assert_eq!(at(&bundle, 0)["sender"]["display"], at(&bundle, 6)["name"]);
    assert_eq!(at(&bundle, 6)["name"], "Hywel Dda University Health Board");
}

#[test]
fn patient_is_active_with_the_ig_profile_a_joined_given_name_and_a_title_prefix() {
    // §6.3, as revised by upstream #435, #439 and #440.
    let patient = at(&referral(), 1);
    assert_eq!(patient["active"], true);
    assert_eq!(
        patient["meta"]["profile"][0],
        "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-patient"
    );
    assert_eq!(
        patient["name"][0]["given"],
        serde_json::json!(["Aneurin Nye"])
    );
    // #435 maps the title again; #439 drops `use`.
    assert_eq!(patient["name"][0]["prefix"], serde_json::json!(["Mr"]));
    assert!(patient["name"][0].get("use").is_none());
}

#[test]
fn the_ig_patient_omits_the_nhs_number_verification_but_other_bundles_keep_it() {
    // §6.3 (upstream #440, story 621936).
    let xml = fixture("REFERRAL_MESSAGE").replace(
        "</PromsEventRequest>",
        "<NHS_CERTIFICATION>01</NHS_CERTIFICATION></PromsEventRequest>",
    );
    let referral = at(&build(&xml), 1);
    assert!(referral["identifier"][0].get("extension").is_none());

    let xml = fixture("SURGERY_MESSAGE").replace(
        "</PromsEventRequest>",
        "<NHS_CERTIFICATION>01</NHS_CERTIFICATION></PromsEventRequest>",
    );
    let surgery = at(&build(&xml), 1);
    assert!(surgery["identifier"][0].get("extension").is_some());
}

#[test]
fn gender_i_is_other() {
    // §7.1 (upstream #439).
    let xml = fixture("REFERRAL_MESSAGE").replace("<gender>M</gender>", "<gender>I</gender>");
    assert_eq!(at(&build(&xml), 1)["gender"], "other");
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
    // §6.3, under the IG's ethnicity system (upstream #435).
    let extension = &at(&referral(), 1)["extension"][0];
    let concept = &extension["valueCodeableConcept"];
    assert_eq!(
        extension["url"],
        "https://fhir.hl7.org.uk/StructureDefinition/Extension-UKCore-EthnicCategory"
    );
    assert_eq!(
        concept["coding"][0]["system"],
        "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-ethnicity"
    );
    assert_eq!(concept["coding"][0]["code"], "A");
    assert_eq!(concept["coding"][0]["display"], "White - British");
    assert_eq!(concept["text"], "White - British");
}

#[test]
fn communication_is_the_spoken_language_as_sent_and_unknown_is_omitted() {
    // §6.3.2 — retires §9.9; the IG's system and the `9` rule are #435's.
    let patient = at(&referral(), 1);
    let coding = &patient["communication"][0]["language"]["coding"][0];
    assert_eq!(
        coding["system"],
        "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-spoken-language"
    );
    assert_eq!(coding["code"], "CY");
    assert_eq!(coding["display"], "Welsh");
    assert!(patient["communication"][0].get("preferred").is_none());

    // WPAS code 9 is Unknown: no communication entry at all.
    let xml = fixture("REFERRAL_MESSAGE").replace(
        "<preferred_spoken_language_code>CY</preferred_spoken_language_code>",
        "<preferred_spoken_language_code> 9 </preferred_spoken_language_code>",
    );
    assert!(at(&build(&xml), 1).get("communication").is_none());

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
fn service_request_is_an_order_whose_code_is_the_pathway_and_whose_date_is_plain() {
    // §6.4, as revised by upstream #435.
    let request = at(&referral(), 2);
    assert_eq!(request["intent"], "order");
    assert_eq!(request["doNotPerform"], false);
    assert_eq!(request["identifier"][0]["value"], "7A22867970");
    assert_eq!(
        request["identifier"][0]["system"],
        "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-servicerequest"
    );

    // The pathway, under the pathway system, with no display; the event type
    // is MessageHeader.eventCoding's job. No reasonCode any more.
    let code = &request["code"]["coding"][0];
    assert_eq!(
        code["system"],
        "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-proms-pathway"
    );
    assert_eq!(code["code"], "ORTHO");
    assert!(code.get("display").is_none());
    assert!(request.get("reasonCode").is_none());

    // A plain date: a midnight-UTC time would shift an hour in BST.
    let date = request["occurrenceDateTime"].as_str().unwrap();
    assert_eq!(date.len(), 10, "{date}");
}

#[test]
fn service_request_requester_is_the_referrer_and_performer_the_consultant() {
    // §6.4 (upstream #435): both are Practitioners, not a PractitionerRole.
    let bundle = referral();
    let request = at(&bundle, 2);
    assert_eq!(request["requester"]["type"], "Practitioner");
    assert_eq!(
        request["requester"]["reference"],
        bundle["entry"][3]["fullUrl"]
    );
    assert_eq!(request["performer"][0]["type"], "Practitioner");
    assert_eq!(
        request["performer"][0]["reference"],
        bundle["entry"][5]["fullUrl"]
    );
}

#[test]
fn a_referrals_event_code_is_lowercase_whichever_field_routed_it() {
    // §6.2 (upstream #435): the WPAS IG defines lowercase codes and has no
    // MessageDefinition, so a Referral's header has neither a raw code nor a
    // definition, even when routed by the legacy `MESSAGE_TYPE` field.
    let xml = fixture("REFERRAL_MESSAGE").replace(
        "<eventCode>REFERRAL</eventCode>",
        "<MESSAGE_TYPE>REFERRAL</MESSAGE_TYPE>",
    );
    let header = at(&build(&xml), 0);
    assert_eq!(header["eventCoding"]["code"], "referral");
    assert_eq!(
        header["eventCoding"]["system"],
        "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-event-codes"
    );
    assert!(header.get("definition").is_none());
    assert!(
        header["meta"]["profile"][0]
            .as_str()
            .unwrap()
            .ends_with("wpas-messageheader")
    );
}

#[test]
fn the_consultant_role_carries_the_code_and_a_listed_specialty_only() {
    // §6.8 (upstream #435): the role is the consultant's; the specialty is
    // sent only for a code on the national practice-setting list.
    let bundle = referral();
    let role = at(&bundle, 4);
    assert_eq!(
        role["identifier"][0]["system"],
        "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-practitionerrole"
    );
    assert_eq!(role["identifier"][0]["value"], "HANJO");
    assert_eq!(
        role["practitioner"]["reference"],
        bundle["entry"][5]["fullUrl"]
    );
    assert_eq!(
        role["organization"]["reference"],
        bundle["entry"][6]["fullUrl"]
    );

    // 110 is on the list, and only its first three characters count.
    let coding = &role["specialty"][0]["coding"][0];
    assert_eq!(
        coding["system"],
        "https://fhir.hl7.org.uk/CodeSystem/UKCore-PracticeSettingCode"
    );
    assert_eq!(coding["code"], "110");
    let longer = fixture("REFERRAL_MESSAGE").replace(">110<", ">110A<");
    assert_eq!(
        at(&build(&longer), 4)["specialty"][0]["coding"][0]["code"],
        "110"
    );

    // 650 is not: no specialty at all, rather than one under a wrong system.
    let unlisted = fixture("REFERRAL_MESSAGE").replace(">110<", ">650<");
    assert!(at(&build(&unlisted), 4).get("specialty").is_none());
}

#[test]
fn a_referral_has_a_referrer_and_a_consultant_each_under_the_boards_own_system() {
    // §6.9 (upstream #435): WPAS sends no GMC or GMP numbers, so each code
    // is filed under the sending board's own system; names are free text.
    let bundle = referral();
    let referrer = at(&bundle, 3);
    assert_eq!(
        referrer["identifier"][0]["system"],
        "https://fhir.hduhb.nhs.wales/Id/wpas-referrer-code"
    );
    assert_eq!(referrer["identifier"][0]["value"], "G7654321");
    assert_eq!(referrer["name"][0]["text"], "ARDERN-JONES L");
    assert!(referrer["name"][0].get("family").is_none());

    let consultant = at(&bundle, 5);
    assert_eq!(
        consultant["identifier"][0]["system"],
        "https://fhir.hduhb.nhs.wales/Id/wpas-consultant-code"
    );
    assert_eq!(consultant["identifier"][0]["value"], "HANJO");
    assert_eq!(consultant["name"][0]["text"], "Jones, Hannah");

    // Every other bundle keeps its one Practitioner, with one identifier.
    let surgery = at(&build(&fixture("SURGERY_MESSAGE")), 3);
    assert_eq!(surgery["identifier"].as_array().unwrap().len(), 1);
    assert_eq!(surgery["name"][0]["text"], "Morgan, James");
}

#[test]
fn an_unknown_board_leaves_the_identifier_off_but_keeps_the_name() {
    // §6.9: there is no valid system for an unknown board, so no identifier
    // is filed under a guess (upstream #435).
    let xml = fixture("REFERRAL_MESSAGE")
        .replace("<system_id>149</system_id>", "<system_id>999</system_id>");
    let bundle = build(&xml);
    let referrer = at(&bundle, 3);
    assert!(referrer.get("identifier").is_none());
    assert_eq!(referrer["name"][0]["text"], "ARDERN-JONES L");
}

#[test]
fn betsi_files_both_codes_under_central_pas_identifier() {
    // upstream #438 (stories 621932, 621934).
    let xml = fixture("REFERRAL_MESSAGE")
        .replace("<system_id>149</system_id>", "<system_id>109</system_id>");
    let bundle = build(&xml);
    for index in [3, 5] {
        assert_eq!(
            at(&bundle, index)["identifier"][0]["system"],
            "https://fhir.bcuhb.nhs.wales/Id/central-pas-identifier",
            "entry {index}"
        );
    }
}

#[test]
fn organization_is_the_health_board_by_its_ods_code() {
    // §6.10, §6.10.1 (upstream #435).
    let org = at(&referral(), 6);
    assert_eq!(org["active"], true);
    assert_eq!(
        org["meta"]["profile"][0],
        "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-organization"
    );
    // The board's own ODS code, not the `X98` placeholder in hbCode.
    assert_eq!(org["identifier"][0]["value"], "7A2");
    assert_eq!(org["name"], "Hywel Dda University Health Board");

    // An unknown board falls back to hbCode, and the name to the ODS lookup.
    let xml = fixture("REFERRAL_MESSAGE")
        .replace("<system_id>149</system_id>", "")
        .replace("X98", "7A2");
    let org = at(&build(&xml), 6);
    assert_eq!(org["identifier"][0]["value"], "7A2");
    assert_eq!(org["name"], "HYWEL DDA UNIVERSITY LHB");

    // A known board is enough without an hbCode (it was required before).
    let xml = fixture("REFERRAL_MESSAGE").replace("<hbCode>X98</hbCode>", "");
    let types: Vec<String> = build(&xml)["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["resource"]["resourceType"].as_str().unwrap().to_string())
        .collect();
    assert!(types.contains(&"Organization".to_string()));

    // Neither a board nor an hbCode: no Organization.
    let xml = fixture("REFERRAL_MESSAGE")
        .replace("<system_id>149</system_id>", "")
        .replace("<hbCode>X98</hbCode>", "");
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
    // §6.11. A Referral has no Location since #435, so this is a bundle that
    // does: surgery, whose Location is the last entry.
    let bundle = build(&fixture("SURGERY_MESSAGE"));
    let entries = bundle["entry"].as_array().unwrap();
    let location = &entries[entries.len() - 1]["resource"];
    assert_eq!(location["resourceType"], "Location");
    assert!(location.get("name").is_none());
    assert_eq!(location["status"], "active");
    // The fixture's own location and postcode, space-joined: a location
    // string that already ends in the postcode repeats it (§6.11); the mapping
    // says so and it is reproduced, not tidied.
    assert_eq!(
        location["address"]["text"],
        "UNIVERSITY HOSPITAL OF WALES, HEATH PARK, CARDIFF, CF14 4XW CF14 4XW"
    );
    assert_eq!(location["address"]["postalCode"], "CF14 4XW");

    // The identifier is `referrer_prac`.
    let with_prac = fixture("SURGERY_MESSAGE").replace(
        "</PromsEventRequest>",
        "<referrer_prac>W95001</referrer_prac></PromsEventRequest>",
    );
    let bundle = build(&with_prac);
    let entries = bundle["entry"].as_array().unwrap();
    let identifier = &entries[entries.len() - 1]["resource"]["identifier"][0];
    assert!(identifier["value"].is_string(), "{identifier}");

    // And a Referral carries none.
    let types: Vec<String> = referral()["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["resource"]["resourceType"].as_str().unwrap().to_string())
        .collect();
    assert!(!types.contains(&"Location".to_string()));
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
