#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
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
//! A SOAP endpoint that accepts anything vaguely SOAP-shaped and answers.
//!
//! Implements `spec/11-mock-receivers.md` §3.
//!
//! A test stand-in for a downstream SOAP system, used behind `soap-sender`:
//! [`soap`] reads a request and builds replies, [`server`] is the HTTP
//! listener that forwards the payload (optionally) and answers. Nothing here
//! parses XML into a tree; requests are scanned as text. That is what makes
//! it forgiving (§3.2) and, as a side effect, immune to the DTD and entity
//! expansion attacks that `spec/11-mock-receivers.md` §3.8 requires a real
//! parser to be hardened against.

pub mod fhir;
pub mod server;
pub mod soap;
