//! Ultra7: a developer tool that sends synthetic test messages.
//!
//! Where it fits: Ultra7 sits *outside* the hub's message flow. It plays the
//! part of a source system, putting a hand-written HL7, XML or JSON body on
//! the wire at an MLLP, REST or SOAP endpoint (typically `hl7-server` or
//! `hl7-soap-server`) and showing what came back. Nothing in the hub depends
//! on it.
//!
//! Layout: [`model`] is the project file, [`storage`] reads and writes it,
//! [`iteration`] mutates one field per repeat, [`sender`] speaks the three
//! protocols, [`run`] is the repeat loop, and [`mod@format`] and [`export`] are
//! the editing helpers. The binary in `main.rs` is a thin shell over these.
//!
//! Implements `spec/ultra7-test-message-sender.md`. The Python original is a
//! Tk GUI; this crate is its headless logic plus a terminal binary
//! (`§2.2`). It is a developer tool and ships in no image (`§1.1`).
//!
//! # Not for real patient data
//!
//! `§1.2`: nothing is redacted, because putting a body on the wire is the
//! point. Use synthetic messages only. The crate has no `tracing`
//! dependency on purpose (`§1.3`).

// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable,
        clippy::float_arithmetic,
        clippy::float_cmp,
        clippy::lossy_float_literal
    )
)]

pub mod export;
pub mod format;
pub mod iteration;
pub mod model;
pub mod run;
pub mod sender;
pub mod storage;
