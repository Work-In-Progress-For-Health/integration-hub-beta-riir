//! Validation must never panic, and must never quote the message.
//!
//! Implements `spec/17-testing-and-quality.md` §10.2 for
//! `spec/06-validation.md` §3 and §8.2.
//!
//! An ingress server validates whatever arrives. Two properties:
//!
//! 1. It returns a verdict or an error — never a panic.
//! 2. Whatever it says, it does not say it by quoting the patient.
//!
//! The second is the one that needs fuzzing rather than examples: the error
//! text comes from a dependency, through several layers, and a value can
//! reach it by a path nobody wrote a test for.

#![no_main]

use ihb_validation::Flow;
use libfuzzer_sys::fuzz_target;

/// A token planted where a patient identifier goes.
const MARKER: &str = "NEEDLE7391";

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    for flow in Flow::ALL {
        // Property 1: arbitrary input yields a result, not a panic.
        let _ = ihb_validation::validate_and_convert(text, flow, None);
    }

    // Property 2: a message carrying a marker in PID-3 must not have it
    // echoed back in any error. The fuzzer varies the rest of the message,
    // so this reaches failure paths examples do not.
    let planted = format!(
        "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
         EVN||20250502092900\r\
         PID|||{MARKER}^^^252^PI||{text}\r\
         PV1||U"
    );

    let rendered = match ihb_validation::validate_and_convert(&planted, Flow::Phw, None) {
        Ok(result) => result.error.map(|error| error.to_string()),
        Err(error) => Some(error.to_string()),
    };

    if let Some(rendered) = rendered {
        assert!(
            !rendered.contains(MARKER),
            "validation quoted a patient identifier: {rendered}"
        );
    }
});
