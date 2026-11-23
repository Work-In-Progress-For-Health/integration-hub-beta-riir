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
//! The HL7 SOAP ingress server.
//!
//! Implements `spec/08-hl7-soap-server.md`.
//!
//! The LIMS flow's ingress (`spec/01-architecture.md` §1.3): an HTTP
//! endpoint taking HL7 v2.xml inside a SOAP envelope, validating it, and
//! forwarding it to Service Bus as ER7. The modules are layered so that each
//! can be tested without the one above it:
//!
//! - [`config`] reads the environment.
//! - [`payload`] slices the business payload out of the request text.
//! - [`process`] takes one request through `spec/08` §3 and yields either an
//!   acceptance or a fault, with no HTTP in it.
//! - [`http`] admits and routes requests (`spec/08` §1, §2) and renders the
//!   result; it checks sizes before reading bodies.
//! - [`serve`] owns the listener, TLS and the drain on shutdown.

pub mod config;
pub mod http;
pub mod payload;
pub mod process;
pub mod serve;
