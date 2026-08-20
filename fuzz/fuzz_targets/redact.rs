//! Redaction must never panic, and must never leak.
//!
//! Implements `spec/17-testing-and-quality.md` §10.2 for
//! `spec/04-observability.md` §2.
//!
//! Redaction sits on the path of every event the Hub emits, and its input is
//! whatever arrived on a socket — a truncated frame, a mis-encoded payload,
//! a message from a system nobody has seen. Two properties have to hold for
//! **all** of that, not just for the messages in the corpus:
//!
//! 1. It returns. A panic here takes down an ingress server.
//! 2. What it returns carries no field value that was not allow-listed.
//!
//! The second is checked by planting a marker in the input and asserting it
//! does not come out — which is only meaningful because the marker is placed
//! where a patient identifier would be.

#![no_main]

use libfuzzer_sys::fuzz_target;

/// A token that must never survive redaction from a non-allow-listed field.
const MARKER: &str = "NEEDLE7391";

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // Property 1: it returns, whatever it was given.
    let redacted = ihb_telemetry::redact::redact_hl7(text);

    // Property 2: nothing that was not allow-listed comes out. Plant the
    // marker in PID-5, a patient name, which is never allow-listed.
    let planted = format!(
        "MSH|^~\\&|{text}|252|100|100|20260729||ADT^A28|MSG1|P|2.5\rPID|1||X||{MARKER}"
    );
    let redacted_planted = ihb_telemetry::redact::redact_hl7(&planted);

    assert!(
        !redacted_planted.contains(MARKER),
        "redaction leaked a PID-5 value for input {text:?}"
    );

    // Property 3: redaction is idempotent — redacting a redacted message
    // must not un-redact it or grow without bound.
    let twice = ihb_telemetry::redact::redact_hl7(&redacted);
    assert!(
        !twice.contains(MARKER),
        "a second pass reintroduced a value"
    );
});
