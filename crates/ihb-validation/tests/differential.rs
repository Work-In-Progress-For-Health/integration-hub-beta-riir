//! Every corpus message, through both implementations.
//!
//! Implements `spec/06-validation.md` §7 and
//! `spec/17-testing-and-quality.md` §4.4.
//!
//! This is the evidence that makes the rewrite's central substitution
//! defensible. The Python validated a message's v2.xml rendering against
//! per-flow XSDs with `xmlschema`; Rust validates it against a dictionary
//! compiled from those same XSDs. Whether that is equivalent is not a matter
//! of argument — it is a matter of running both over the same messages and
//! comparing.
//!
//! # The corpus
//!
//! `corpus/python-verdicts.json` holds 30 cases: the six sample messages
//! from `Integration-Hub-Beta/local/sample_messages/`, each in five
//! variants — as sent, and with the `MSH`, `EVN` or `PID` segment removed,
//! and with an unknown segment appended. Each entry carries the ER7, the
//! flow, the Python's verdict, and the XML the Python produced.
//!
//! It was captured once, by running the Python
//! (`spec/17-testing-and-quality.md` §4.4.2). CI compares against the
//! committed file and needs no Python runtime.
//!
//! **Do not re-capture it to make a test pass** (§4.4.3). A disagreement
//! means the Rust is wrong, or a deviation has been agreed and recorded in
//! `plan.md` §9.

// An integration test is its own crate, so the crate-level allow in
// `src/lib.rs` does not reach it. Asserting is what this file is for.
// See spec/17-testing-and-quality.md §2.3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use ihb_validation::{Flow, ValidationError, validate_and_convert};
use serde_json::Value;

/// The captured Python behaviour.
const CORPUS: &str = include_str!("corpus/python-verdicts.json");

/// One case: what went in, and what the Python made of it.
struct Case {
    /// `fixture|flow|variant`, for failure messages.
    key: String,
    /// The message.
    er7: String,
    /// The schema group it was validated against.
    flow: Flow,
    /// `valid`, `invalid`, or `error`.
    verdict: String,
    /// The structure the Python resolved, when it got that far.
    structure_id: Option<String>,
}

/// Every case in the corpus.
fn cases() -> Vec<Case> {
    let parsed: BTreeMap<String, Value> =
        serde_json::from_str(CORPUS).expect("the committed corpus is valid JSON");

    parsed
        .into_iter()
        .map(|(key, entry)| {
            let flow_name = entry["flow"].as_str().expect("every case names a flow");
            Case {
                key,
                er7: entry["er7"]
                    .as_str()
                    .expect("every case carries ER7")
                    .to_string(),
                flow: flow_name.parse().expect("the flow is a schema group"),
                verdict: entry["verdict"]
                    .as_str()
                    .expect("every case has a verdict")
                    .to_string(),
                structure_id: entry["structure_id"].as_str().map(str::to_string),
            }
        })
        .collect()
}

/// What Rust makes of a case, in the Python's vocabulary.
fn rust_verdict(case: &Case) -> (String, Option<String>) {
    match validate_and_convert(&case.er7, case.flow, None) {
        Ok(result) if result.is_ok() => ("valid".into(), Some(result.structure_id)),
        Ok(result) => ("invalid".into(), Some(result.structure_id)),
        // The Python raises for a message it cannot read or place at all,
        // which is the same category as an `Err` here.
        Err(ValidationError::Parse { .. } | ValidationError::Structure) => ("error".into(), None),
        Err(_) => ("invalid".into(), None),
    }
}

#[test]
fn the_corpus_is_the_size_it_should_be() {
    // A corpus that silently shrank would make every other test in this
    // file pass for the wrong reason.
    let cases = cases();
    assert_eq!(cases.len(), 30, "six fixtures, five variants each");
}

#[test]
fn every_case_gets_the_same_verdict_as_the_python() {
    // spec/06-validation.md §7.2 — the blocking requirement on Phase 2.
    let mut disagreements = Vec::new();

    for case in cases() {
        let (verdict, _) = rust_verdict(&case);
        if verdict != case.verdict {
            disagreements.push(format!(
                "  {}\n      python: {}\n      rust:   {}",
                case.key, case.verdict, verdict
            ));
        }
    }

    assert!(
        disagreements.is_empty(),
        "{} of 30 cases disagree with the Python:\n{}\n\n\
         Each is either a defect in the Rust or a deviation that belongs in \
         plan.md §9. Re-capturing the corpus is not a fix.",
        disagreements.len(),
        disagreements.join("\n")
    );
}

#[test]
fn every_readable_message_resolves_to_the_same_structure() {
    // spec/06-validation.md §2 — the structure names the schema, so
    // agreeing on the verdict while disagreeing on the structure would mean
    // agreeing by accident.
    for case in cases() {
        let Some(expected) = case.structure_id.as_deref() else {
            continue;
        };
        let (_, actual) = rust_verdict(&case);
        assert_eq!(
            actual.as_deref(),
            Some(expected),
            "{} resolved to a different structure",
            case.key
        );
    }
}

#[test]
fn every_valid_case_produces_xml_in_the_hl7_namespace() {
    // spec/06-validation.md §4.1. The XML goes to the message store, so it
    // has to be a v2.xml document rather than merely something.
    for case in cases() {
        if case.verdict != "valid" {
            continue;
        }
        let result = validate_and_convert(&case.er7, case.flow, None)
            .unwrap_or_else(|error| panic!("{}: {error}", case.key));

        assert!(
            result.xml.contains("urn:hl7-org:v2xml"),
            "{} produced XML outside the HL7 namespace",
            case.key
        );
        assert!(
            result.xml.contains(&format!("<{}", result.structure_id)),
            "{} produced XML whose root is not its structure",
            case.key
        );
    }
}

#[test]
fn no_error_ever_quotes_the_message() {
    // spec/index.md §0.8.4, spec/06-validation.md §8.2.
    //
    // Every failing case in the corpus is put through the error path and
    // the rendered error is searched for values from its own message. This
    // is the guarantee that a rejected message's diagnosis can be logged.
    for case in cases() {
        let markers = patient_markers(&case.er7);
        if markers.is_empty() {
            continue;
        }

        let rendered = match validate_and_convert(&case.er7, case.flow, None) {
            Ok(result) => result.error.map(|error| error.to_string()),
            Err(error) => Some(error.to_string()),
        };

        let Some(rendered) = rendered else { continue };

        for marker in &markers {
            assert!(
                !rendered.contains(marker.as_str()),
                "{} leaked {marker:?} into an error:\n  {rendered}",
                case.key
            );
        }
    }
}

/// Values from a message that must never appear in an error about it.
///
/// Taken from the message itself rather than hard-coded, so a fixture added
/// later is covered without anyone remembering to extend a list. Short
/// values are skipped: a two-character token would match inside a structure
/// id or a path and make this assert nothing useful.
fn patient_markers(er7: &str) -> Vec<String> {
    er7.split('\r')
        .filter(|segment| segment.starts_with("PID") || segment.starts_with("PD1"))
        .flat_map(|segment| segment.split(['|', '^', '~', '&']))
        .map(str::trim)
        .filter(|value| value.len() >= 6)
        .map(str::to_string)
        .collect()
}
