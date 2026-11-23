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
//! SOAP egress.
//!
//! Implements `spec/10-senders.md` §4, over the shared pipeline of
//! [`ihb_sender`].

pub mod client;
pub mod envelope;

pub use client::{Endpoint, SoapClient};
pub use envelope::{Format, escape, evaluate, wrap, wrap_wis};
