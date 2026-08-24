//! Every fixture, against the Python's bundle.
//!
//! Implements `spec/16-proms-transformer.md` §8.1.
//!
//! # This is the test that matters
//!
//! Everything else in this crate checks a piece. This checks the whole
//! thing against the output of the service being replaced, for all eleven
//! fixtures, with the deterministic UUID factory of §4.4.
//!
//! The comparison is **canonical**: object key order is not significant in
//! JSON and the corpus was captured with sorted keys, so `serde_json::Value`
//! equality is the right test. Array order **is** significant and is
//! compared as-is — §3's entry order is the whole point.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use xml_fhir_proms_transformer::identity::SequentialUuids;
use xml_fhir_proms_transformer::reference_data::StaticResolver;
use xml_fhir_proms_transformer::transform::transform;

fn fixtures() -> BTreeMap<String, String> {
    serde_json::from_str(include_str!("corpus/wpas-messages.json")).expect("valid JSON")
}

fn goldens() -> BTreeMap<String, serde_json::Value> {
    serde_json::from_str(include_str!("corpus/python-golden.json")).expect("valid JSON")
}

/// Transform a fixture the way the corpus was captured.
fn run(xml: &str) -> Result<serde_json::Value, String> {
    transform(xml, &SequentialUuids::new(), &StaticResolver, &|_| None).map_err(|e| e.to_string())
}

#[test]
fn every_fixture_matches_the_pythons_bundle() {
    // §8.1 — the whole of it.
    let fixtures = fixtures();
    let goldens = goldens();
    let mut compared = 0;

    for (name, golden) in &goldens {
        // §2.3's unroutable fixture produced an error, not a bundle.
        if golden.get("__error__").is_some() {
            continue;
        }

        let ours = run(&fixtures[name]).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(&ours, golden, "{name} differs from the Python's bundle");
        compared += 1;
    }

    assert_eq!(compared, 10, "ten routable fixtures");
}

#[test]
fn the_unroutable_fixture_fails_the_way_the_python_failed() {
    // §2.3 — the Python raised, and the capture recorded the message.
    let golden = &goldens()["UNROUTABLE_MESSAGE"];
    let their_error = golden["__error__"].as_str().unwrap();

    let error = run(&fixtures()["UNROUTABLE_MESSAGE"]).unwrap_err();

    // Both name the offending code and what would have worked.
    assert!(their_error.contains("UNKNOWN_TYPE"), "{their_error}");
    assert!(error.contains("UNKNOWN_TYPE"), "{error}");
    for code in [
        "REFERRAL",
        "SURGERY",
        "PREOP",
        "INPATIENT",
        "CANCELLED",
        "PREREAD",
    ] {
        assert!(error.contains(code), "{code} missing from: {error}");
    }
}

#[test]
fn no_reference_dangles_in_any_fixture() {
    // §4.3 and §8.4, across the corpus rather than one hand-built case.
    for (name, xml) in fixtures() {
        let Ok(bundle) = run(&xml) else { continue };

        let urls: Vec<String> = bundle["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["fullUrl"].as_str().unwrap().to_string())
            .collect();

        let mut found = 0;
        visit(&bundle, &mut |object| {
            if let Some(reference) = object.get("reference").and_then(serde_json::Value::as_str) {
                assert!(
                    urls.contains(&reference.to_string()),
                    "{name}: {reference} dangles; entries are {urls:?}"
                );
                found += 1;
            }
        });

        assert!(found > 0, "{name}: no references at all");
    }
}

#[test]
fn every_full_url_is_unique_within_its_bundle() {
    // §4.1 — every resource gets its **own** UUID. Two entries sharing one
    // would make a reference ambiguous, and the slot model allocates per
    // slot so a bug here would be a slot reused rather than a collision.
    for (name, xml) in fixtures() {
        let Ok(bundle) = run(&xml) else { continue };

        let mut urls: Vec<&str> = bundle["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["fullUrl"].as_str().unwrap())
            .collect();
        let count = urls.len();
        urls.sort_unstable();
        urls.dedup();

        assert_eq!(urls.len(), count, "{name} reuses a fullUrl");
    }
}

#[test]
fn no_bundle_carries_a_null() {
    // §6.0.1 — an absent source leaves its element **unset**, not null. A
    // `null` in a FHIR resource is a validation error, and it is what an
    // `Option` serialised carelessly produces.
    for (name, xml) in fixtures() {
        let Ok(bundle) = run(&xml) else { continue };

        visit(&bundle, &mut |object| {
            for (key, value) in object.as_object().unwrap() {
                assert!(!value.is_null(), "{name}: {key} is null");
            }
        });
    }
}

/// Visit every object in a document.
fn visit(value: &serde_json::Value, f: &mut impl FnMut(&serde_json::Value)) {
    match value {
        serde_json::Value::Object(map) => {
            f(value);
            for child in map.values() {
                visit(child, f);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                visit(child, f);
            }
        }
        _ => {}
    }
}
