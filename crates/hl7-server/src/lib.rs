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
        clippy::unreachable
    )
)]
//! MLLP ingress: receive an HL7 v2 message, validate it, forward it, and only
//! then acknowledge it.
//!
//! Implements `spec/07-hl7-server.md`. One process serves one flow; which
//! flow is entirely environment configuration ([`config`]).
//!
//! # How the modules fit
//!
//! - [`listener`] frames MLLP off the socket and enforces the size limit
//!   (§2).
//! - [`validate`] runs the three validation layers (§3, §7).
//! - [`pipeline`] orders the work for one message, builds the routing
//!   properties ([`routing`]) and decides the outcome (§5, §6).
//! - [`ack`] builds the `AA` / `AR` / `AE` reply (§4).
//!
//! # The invariant everything serves
//!
//! An `AA` is written only after the message was handed to Service Bus. A
//! failed store write is swallowed; a failed forward is not
//! (`AGENTS.md`, `spec/07-hl7-server.md` §5). The source system retries on
//! anything but `AA`, so the unsafe direction is acknowledging early.

pub mod ack;
pub mod config;
pub mod listener;
pub mod pipeline;
pub mod routing;
pub mod validate;
