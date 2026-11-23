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
//! The REST ingress server.
//!
//! Implements `spec/19-rest-servers.md` §9 and §10, and `spec/02-configuration.md`
//! §12.
//!
//! One image, two pipelines (`PIPELINE`): `generic`, which unwraps a SOAP or
//! plain-XML request, validates it and forwards it; and `hl7`, which is
//! `hl7-rest-server`'s pipeline, served through the same listener
//! (`spec/19-rest-servers.md` §0.1).

pub mod adapter;
pub mod config;
pub mod http;
pub mod process;
pub mod validator;
pub mod xml;
