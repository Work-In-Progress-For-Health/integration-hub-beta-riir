//! The parser against the real WPAS fixtures.
//!
//! Implements part of `spec/16-proms-transformer.md` §8.1: the goldens
//! compare whole bundles, but a bundle can only be right if the parse was,
//! and a parse failure is much easier to read here than as a diff of two
//! FHIR documents.
//!
//! The fixtures are the Python's own, extracted verbatim from
//! `tests/wpas_messages.py`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use xml_fhir_proms_transformer::parse::{event_code, parse};

/// Every fixture, by name.
fn fixtures() -> BTreeMap<String, String> {
    let raw = include_str!("corpus/wpas-messages.json");
    serde_json::from_str(raw).expect("the fixture corpus is valid JSON")
}

#[test]
fn every_fixture_parses() {
    // §2.5 — nineteen real payloads, including the nested and legacy shapes.
    let fixtures = fixtures();
    assert_eq!(
        fixtures.len(),
        19,
        "the corpus should hold nineteen fixtures"
    );

    for (name, xml) in &fixtures {
        let message = parse(xml).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert!(!message.is_empty(), "{name} indexed no fields");

        // §2.1 — the root is **not** required to be `PromsEventRequest`.
        // Ten fixtures use it and `EXPLICIT_EVENT_CODE_MESSAGE` uses
        // `WpasMessage`, routing on its `eventCode` like the rest.
        assert!(
            matches!(
                message.root_tag.as_str(),
                "PromsEventRequest" | "WpasMessage"
            ),
            "{name} has root <{}>",
            message.root_tag
        );
    }
}

#[test]
fn every_routable_fixture_yields_its_event_code() {
    // §2.2's codes (the bare legacy ones and the #370 suffixed ones),
    // plus §2.3's unknown one.
    let expected: BTreeMap<&str, Option<&str>> = [
        ("REFERRAL_MESSAGE", Some("REFERRAL")),
        ("SURGERY_MESSAGE", Some("SURGERY")),
        ("PREOP_MESSAGE", Some("PREOP")),
        ("INPATIENT_MESSAGE", Some("INPATIENT")),
        ("CANCELLED_MESSAGE", Some("CANCELLED")),
        ("PREREAD_MESSAGE", Some("PREREAD")),
        ("MINIMAL_REFERRAL_MESSAGE", Some("REFERRAL")),
        ("NESTED_REFERRAL_MESSAGE", Some("REFERRAL")),
        ("EXPLICIT_EVENT_CODE_MESSAGE", Some("SURGERY")),
        // §2.3 — routed nowhere, and the transform fails.
        ("UNROUTABLE_MESSAGE", Some("UNKNOWN_TYPE")),
        // §2.5.7's legacy alias.
        ("LEGACY_MESSAGE_TYPE_MESSAGE", Some("REFERRAL")),
        ("LEGACY_MESSAGE_TYPE_SURGERY_MESSAGE", Some("SURGERY")),
        ("LEGACY_MESSAGE_TYPE_PREOP_MESSAGE", Some("PREOP")),
        // Upstream #370's suffixed codes (§10.2).
        ("SURGERY_PROC_MESSAGE", Some("SURGERY-PROC")),
        ("SURGERY_PERF_MESSAGE", Some("SURGERY-PERF")),
        ("DISCHARGE_MESSAGE", Some("SURGERY-DN")),
        ("PREOP_AS_MESSAGE", Some("PREOP-AS")),
        ("OUTPATIENT_MESSAGE", Some("PREOP-VIS")),
        ("RESCHEDULE_MESSAGE", Some("PREOP-AR")),
    ]
    .into_iter()
    .collect();

    for (name, xml) in fixtures() {
        let message = parse(&xml).unwrap();
        let found = event_code(&message);

        assert_eq!(
            found.as_deref(),
            *expected.get(name.as_str()).unwrap_or(&None),
            "{name} routed to {found:?}"
        );
    }
}

#[test]
fn the_nested_fixture_reads_the_same_fields_as_the_flat_one() {
    // §2.5.2 — indexing by name wherever an element sits is what makes a
    // nested payload work at all, and `NESTED_REFERRAL_MESSAGE` exists to
    // exercise exactly that.
    let fixtures = fixtures();
    let nested = parse(&fixtures["NESTED_REFERRAL_MESSAGE"]).unwrap();

    // The fields a Referral bundle needs, found despite the nesting.
    assert!(nested.has(&["nhsNumber", "NHS_NUMBER"]), "no NHS number");
    assert!(nested.has(&["patientSurname", "SURNAME"]), "no surname");
    assert_eq!(event_code(&nested).as_deref(), Some("REFERRAL"));
}

#[test]
fn the_minimal_fixture_is_missing_the_fields_that_drop_entries() {
    // §3.2 and §9.1 — this is *why* the minimal referral produces five
    // entries rather than seven. Asserted here so the reason is visible
    // before the bundle diff makes it a puzzle.
    let fixtures = fixtures();
    let minimal = parse(&fixtures["MINIMAL_REFERRAL_MESSAGE"]).unwrap();
    let full = parse(&fixtures["REFERRAL_MESSAGE"]).unwrap();

    // §6.9 — the Practitioner comes from these, for a Referral.
    assert!(full.has(&["referrer_code", "referrerCode"]));
    assert!(
        !minimal.has(&["referrer_code", "referrerCode"])
            && !minimal.has(&["referrer_name", "referrerName"]),
        "the minimal fixture should carry no referrer, which is what drops \
         the Practitioner entry"
    );

    // §6.10 — the Organization comes from these.
    assert!(
        !minimal.has(&["referrer_org", "referrerOrg"])
            && !minimal.has(&["dhaCode", "DHA_CODE", "dha_code"]),
        "and no organisation, which drops the Organization entry"
    );
}

#[test]
fn a_fixture_carries_both_dialects_without_two_lookups() {
    // §6.0 — the normalisation that lets one set of mappers serve both
    // naming conventions.
    let fixtures = fixtures();
    let referral = parse(&fixtures["REFERRAL_MESSAGE"]).unwrap();

    let by_camel = referral.get(&["nhsNumber"]);
    let by_snake = referral.get(&["NHS_NUMBER"]);

    assert!(by_camel.is_some(), "the fixture carries an NHS number");
    assert_eq!(by_camel, by_snake, "both dialects reach the same value");
}

/// Every captured bundle, by fixture name.
fn goldens() -> BTreeMap<String, serde_json::Value> {
    let raw = include_str!("corpus/python-golden.json");
    serde_json::from_str(raw).expect("the golden corpus is valid JSON")
}

#[test]
fn the_slot_model_reproduces_every_golden_entry_order() {
    // spec/16 §3, §3.2 and §8.3, against the Python's own output rather
    // than against my reading of it.
    //
    // This is the check that the slot model is right *before* any mapper
    // exists: the entry order and the UUID numbering are decided by
    // `bundle`, so if they match here the mappers only have to fill slots.
    use xml_fhir_proms_transformer::bundle::{Builder, slots_for};
    use xml_fhir_proms_transformer::identity::SequentialUuids;
    use xml_fhir_proms_transformer::message_type::resolve;
    use xml_fhir_proms_transformer::parse::{event_code, parse};

    for (name, golden) in goldens() {
        // §2.3's unroutable fixture produces no bundle at all.
        if golden.get("__error__").is_some() {
            continue;
        }

        let xml = &fixtures()[&name];
        let message = parse(xml).unwrap();
        let message_type = resolve(event_code(&message).as_deref(), &message.root_tag).unwrap();

        // What the Python actually emitted, in order.
        let emitted: Vec<&str> = golden["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["resource"]["resourceType"].as_str().unwrap())
            .collect();

        // Fill exactly those slots, leaving the rest empty.
        let uuids = SequentialUuids::new();
        let mut builder = Builder::new(message_type, &uuids);
        for slot in slots_for(message_type) {
            let resource_type = slot.resource_type(message_type.clinical);
            if emitted.contains(&resource_type) {
                builder.put(slot, serde_json::json!({ "resourceType": resource_type }));
            }
        }
        let built = builder.finish();

        // The entry order matches.
        let ours: Vec<&str> = built["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["resource"]["resourceType"].as_str().unwrap())
            .collect();
        assert_eq!(ours, emitted, "{name}: entry order");

        // And so does every `fullUrl` — which is the part that would go
        // wrong if UUIDs were allocated per *entry* rather than per slot.
        // §3.2's minimal referral is the case that catches it: its
        // `Location` carries the seventh UUID at the fifth entry.
        let their_urls: Vec<&str> = golden["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["fullUrl"].as_str().unwrap())
            .collect();
        let our_urls: Vec<&str> = built["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["fullUrl"].as_str().unwrap())
            .collect();
        assert_eq!(our_urls, their_urls, "{name}: UUID allocation");

        // The envelope, while we are here.
        assert_eq!(built["resourceType"], golden["resourceType"], "{name}");
        assert_eq!(built["type"], golden["type"], "{name}");
    }
}

#[test]
fn the_unroutable_fixture_is_the_only_one_that_fails_to_route() {
    // §2.3 and §8.2 — the fixture exists to prove the failure path, and
    // nothing else should reach it.
    use xml_fhir_proms_transformer::message_type::resolve;
    use xml_fhir_proms_transformer::parse::{event_code, parse};

    for (name, xml) in fixtures() {
        let message = parse(&xml).unwrap();
        let routed = resolve(event_code(&message).as_deref(), &message.root_tag);

        if name == "UNROUTABLE_MESSAGE" {
            let error = routed.expect_err("this fixture must not route");
            assert!(error.to_string().contains("UNKNOWN_TYPE"), "{error}");
        } else {
            assert!(routed.is_ok(), "{name} did not route: {routed:?}");
        }
    }
}

#[test]
fn the_date_helpers_reproduce_every_date_in_the_goldens() {
    // spec/16 §6.1, measured against the Python's own output rather than
    // against my reading of `mapping_utils.py`.
    //
    // Each row is (raw WPAS value, what the Python emitted). Together they
    // cover §6.1.1's date-only path, §6.1.2's date-plus-time path, and the
    // midnight default a date-only value takes when a `dateTime` is wanted.
    use xml_fhir_proms_transformer::mapping::{to_fhir_date, to_fhir_datetime};

    // `birthDate` — a FHIR `date`, kept as one.
    for (raw, expected) in [
        ("1985-03-15", "1985-03-15"),
        ("1940-09-02", "1940-09-02"),
        ("1897-11-15", "1897-11-15"),
    ] {
        assert_eq!(to_fhir_date(raw).as_deref(), Some(expected), "{raw}");
    }

    // `occurrenceDateTime` — `eventDate` alone, so midnight UTC.
    assert_eq!(
        to_fhir_datetime("2024-06-12").as_deref(),
        Some("2024-06-12T00:00:00Z")
    );

    // `performedDateTime` and `start`/`end` — `appointmentDate` and
    // `appointmentTime` concatenated with a space, which is §6.5 and §6.6.
    for (date, at, expected) in [
        ("2024-07-20", "09:30", "2024-07-20T09:30:00Z"),
        ("2024-08-15", "14:00", "2024-08-15T14:00:00Z"),
        ("2024-10-15", "11:00", "2024-10-15T11:00:00Z"),
    ] {
        let combined = format!("{date} {at}");
        assert_eq!(
            to_fhir_datetime(&combined).as_deref(),
            Some(expected),
            "{combined}"
        );
    }
}

#[test]
fn practitioner_names_are_the_raw_field_as_free_text() {
    // §6.9 and §6.9.2 — upstream #423 stopped parsing the name, so the
    // golden's `Practitioner.name.text` must equal the raw WPAS field.
    // (`split_person_name` itself is still unit-tested in `mapping`.)
    let mut checked = 0;
    for (name, bundle) in &goldens() {
        if bundle.get("__error__").is_some() {
            continue;
        }
        for entry in bundle["entry"].as_array().unwrap() {
            let resource = &entry["resource"];
            if resource["resourceType"] != "Practitioner" {
                continue;
            }
            let Some(emitted) = resource["name"].as_array().and_then(|n| n.first()) else {
                continue;
            };

            // The raw field this came from, per §6.9.
            let xml = &fixtures()[name];
            let raw = if name.contains("REFERRAL") {
                between(xml, "referrer_name")
            } else {
                between(xml, "clinicianName")
            };
            let Some(raw) = raw else { continue };

            assert_eq!(emitted["text"].as_str(), Some(raw.as_str()), "{name}");
            assert!(emitted.get("family").is_none(), "{name}: name was parsed");
            checked += 1;
        }
    }

    assert!(checked > 0, "no Practitioner names were checked");
}

/// The text of the first `<tag>…</tag>` in a payload.
fn between(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}
