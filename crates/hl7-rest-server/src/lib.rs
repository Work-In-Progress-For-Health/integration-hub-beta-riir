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
//! The REST HL7 ingress.
//!
//! Implements `spec/19-rest-servers.md` §1 to §8 and `spec/02-configuration.md`
//! §11.
//!
//! This crate is the pipeline `rest-server` runs for `PIPELINE=hl7` as well
//! as the standalone `hl7-rest-server` service (`spec/19-rest-servers.md`
//! §0.1), so most of what is public here is public for that reason.

pub mod ack;
pub mod config;
pub mod http;
pub mod input;
pub mod process;
pub mod risp;
pub mod routing;
pub mod rules;
pub mod serve;
