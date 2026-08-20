//! Redaction, compared against the Python's own output.
//!
//! Implements `spec/17-testing-and-quality.md` §4.4 for
//! `spec/04-observability.md` §2.
//!
//! The unit tests beside the code check each rule in isolation. This checks
//! the thing that actually matters: that a real message from a real flow
//! comes out of the Rust byte-for-byte as it came out of the Python.
//!
//! # The corpus
//!
//! `tests/corpus/` holds the six sample messages from
//! `Integration-Hub-Beta/local/sample_messages/`, and beside each one a
//! `.redacted` file captured by running the Python's
//! `event_logger_lib.redaction.redact_hl7_message` over it. The capture is a
//! deliberate, one-off act; CI compares against the committed file and needs
//! no Python runtime (`spec/17-testing-and-quality.md` §4.4.2).
//!
//! **Do not re-capture a golden file to make a test pass.** A disagreement
//! here means either the Rust is wrong or a deviation has been agreed and
//! recorded in `plan.md` §9. Neither is fixed by moving the target
//! (§4.4.3).

// An integration test is its own crate, so the crate-level allow in
// `src/lib.rs` does not reach it. Asserting is what this file is for.
// See spec/17-testing-and-quality.md §2.3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use ihb_telemetry::redact::redact_hl7;

/// Where the fixtures live.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

/// Every message fixture in the corpus.
fn messages() -> Vec<PathBuf> {
    let corpus = corpus();
    let entries = fs::read_dir(&corpus)
        .unwrap_or_else(|error| panic!("reading {}: {error}", corpus.display()));

    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.expect("a corpus directory entry").path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("hl7"))
        .collect();
    // Sorted so a failure names the same file every run.
    paths.sort();
    paths
}

/// Read a message the way a service receives one: whatever line ending the
/// file happens to carry, normalised to `\r`.
fn read_message(path: &Path) -> String {
    let raw = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("reading {}: {error}", path.display());
    });
    raw.replace("\r\n", "\r")
        .replace('\n', "\r")
        .trim()
        .to_string()
}

#[test]
fn every_sample_message_redacts_exactly_as_the_python_did() {
    let mut compared = 0;

    for path in messages() {
        let golden_path = path.with_extension("hl7.redacted");
        let golden = fs::read_to_string(&golden_path).unwrap_or_else(|error| {
            panic!(
                "reading the golden file {}: {error}\n\
                 Every .hl7 in the corpus needs a .hl7.redacted beside it.",
                golden_path.display()
            )
        });

        let actual = redact_hl7(&read_message(&path));

        assert_eq!(
            actual,
            golden,
            "\nredaction differs from the Python for {}\n\
             This is either a defect or an agreed deviation (plan.md §9).\n\
             It is not a reason to re-capture the golden file.",
            path.file_name().unwrap_or_default().to_string_lossy()
        );

        compared += 1;
    }

    assert_eq!(
        compared, 6,
        "expected the six sample messages from local/sample_messages/"
    );
}

#[test]
fn no_sample_message_leaks_a_patient_identifier_from_its_body() {
    // spec/index.md §0.8.4, spec/04-observability.md §2.12.
    //
    // Values taken from the sample messages themselves: a surname, an NHS
    // number, a local patient identifier, a date of birth and an address
    // line. If any of these survives redaction the control has failed,
    // whatever the golden files say.
    //
    // Scoped to the segments after MSH because of the open finding in
    // spec/04-observability.md §2.13: the allow-listed MSH-10 legitimately
    // carries an NHS number for the PHW flow. That case is asserted
    // separately, below, so it is documented rather than hidden here.
    const MARKERS: &[&str] = &["MYSURNAME", "4444444444", "8888888", "19990101", "MY ROAD"];

    for path in messages() {
        let redacted = redact_hl7(&read_message(&path));
        let body = redacted.split_once('\r').map_or("", |(_, rest)| rest);

        for marker in MARKERS {
            assert!(
                !body.contains(marker),
                "{} leaked {marker} out of its body through redaction",
                path.display()
            );
        }
    }
}

#[test]
fn msh_10_carries_an_nhs_number_for_the_phw_flow() {
    // spec/04-observability.md §2.13 — an open finding, pinned by a test so
    // it cannot quietly change while it is being decided.
    //
    // PHW builds its message control id as <timestamp><nhs-number>, and
    // MSH-10 is on the §2.6 allow-list, so the NHS number survives
    // redaction. This asserts the CURRENT behaviour, which matches the
    // Python. It is not an endorsement: if §2.13 is resolved by narrowing
    // the allow-list, this test changes with it.
    let path = corpus().join("phw-to-mpi.sample.hl7");
    let redacted = redact_hl7(&read_message(&path));

    let control_id = redacted
        .split('\r')
        .next()
        .and_then(|header| header.split('|').nth(9))
        .expect("the PHW sample has an MSH-10");

    assert_eq!(control_id, "202505052323364444444444");
    assert!(
        control_id.contains("4444444444"),
        "if this no longer holds, spec/04-observability.md §2.13 has been \
         resolved and this test should record how"
    );
}

#[test]
fn the_message_stays_readable_as_a_message() {
    // spec/04-observability.md §2.6 — redaction that destroys routing
    // metadata would be safe and useless. Every sample must still show
    // which system sent it, what type it was, and its control id.
    for path in messages() {
        let original = read_message(&path);
        let redacted = redact_hl7(&original);

        let original_header: Vec<&str> = original
            .split('\r')
            .next()
            .unwrap_or_default()
            .split('|')
            .collect();
        let redacted_header: Vec<&str> = redacted
            .split('\r')
            .next()
            .unwrap_or_default()
            .split('|')
            .collect();

        // MSH-3 sending application, MSH-9 message type, MSH-10 control id.
        for (index, label) in [(2, "MSH-3"), (8, "MSH-9"), (9, "MSH-10")] {
            assert_eq!(
                redacted_header.get(index),
                original_header.get(index),
                "{} lost {label}, which support needs to find the message",
                path.display()
            );
        }

        // The segment count is unchanged: the shape survives.
        assert_eq!(
            redacted.split('\r').count(),
            original.split('\r').count(),
            "{} changed segment count",
            path.display()
        );
    }
}
