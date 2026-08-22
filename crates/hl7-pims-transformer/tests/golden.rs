//! Every fixture, against the Python's own output, byte for byte.
//!
//! Implements `spec/09-transformers.md` §6.6.
//!
//! # How the corpus was made
//!
//! `tests/corpus/python-golden.json` was captured once, by running the
//! Python transformer in its own virtualenv over every message available
//! to it — the fixtures in its test suite and its flow's file in
//! `local/sample_messages/`. Each entry holds the input, the ER7 the Python
//! produced, and the audit text it reported.
//!
//! **Do not re-capture it to make a test pass.** A disagreement means the
//! Rust is wrong, or a deviation has been agreed and recorded in
//! `plan.md` §9. This is the same rule as `spec/06-validation.md` §7's
//! corpus, for the same reason: a golden file regenerated from the code it
//! is meant to check is not a check.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hl7_pims_transformer::Pims;
use ihb_transformer::Transformer;

/// The Python's captured behaviour.
const CORPUS: &str = include_str!("corpus/python-golden.json");

/// Every case, as (name, input, expected output, expected audit text).
fn cases() -> Vec<(String, String, String, String)> {
    let corpus: serde_json::Value = serde_json::from_str(CORPUS).expect("the corpus parses");
    corpus
        .as_object()
        .expect("the corpus is an object")
        .iter()
        .filter_map(|(name, case)| {
            Some((
                name.clone(),
                case["input"].as_str()?.to_string(),
                case.get("output")?.as_str()?.to_string(),
                case.get("audit").and_then(|v| v.as_str())?.to_string(),
            ))
        })
        .collect()
}

#[test]
fn the_corpus_is_not_empty() {
    // A golden test over nothing passes silently, which is the worst
    // outcome available to it.
    assert!(cases().len() >= 4, "the corpus should hold every fixture");
}

#[test]
fn every_message_transforms_exactly_as_the_python_did() {
    // spec/09-transformers.md §6.6.
    for (name, input, expected, _) in cases() {
        let message = ihb_validation::parse(&input)
            .unwrap_or_else(|error| panic!("{name}: the input parses: {error}"));

        let out = Pims
            .map(&message)
            .unwrap_or_else(|error| panic!("{name}: {error}"));

        assert_eq!(
            out.to_er7(),
            expected,
            "{name}: the transformed message differs from the Python's"
        );
    }
}

#[test]
fn every_audit_text_matches_the_pythons() {
    // spec/09-transformers.md §1.5 — the default text, which this
    // transformer does not override.
    for (name, input, _, expected) in cases() {
        let message = ihb_validation::parse(&input).unwrap();

        assert_eq!(Pims.processed_audit_text(&message), expected, "{name}");
    }
}
