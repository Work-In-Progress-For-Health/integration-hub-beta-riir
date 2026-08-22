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

use hl7_phw_transformer::Phw;

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
    assert!(cases().len() >= 2, "the corpus should hold every fixture");
}

#[test]
fn every_message_transforms_exactly_as_the_python_did() {
    // spec/09-transformers.md §6.6.
    for (name, input, expected, _) in cases() {
        let message = ihb_validation::parse(&input)
            .unwrap_or_else(|error| panic!("{name}: the input parses: {error}"));

        let (out, _) = Phw::default()
            .transform_reporting(&message)
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
    // spec/09-transformers.md §3.5. The audit text is what tells an
    // operator that the message MPI received is not the one PHW sent, so
    // it is compared as carefully as the message.
    for (name, input, _, expected) in cases() {
        let message = ihb_validation::parse(&input).unwrap();
        let (_, applied) = Phw::default().transform_reporting(&message).unwrap();

        let ours = applied.audit_text().unwrap_or_else(|| {
            // The default of §1.5, which the corpus records when neither
            // transformation ran.
            use ihb_transformer::Transformer;
            Phw::default().processed_audit_text(&message)
        });

        assert_eq!(ours, expected, "{name}");
    }
}
