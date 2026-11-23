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
//! An MLLP endpoint that acknowledges what it is sent and forwards it to a
//! queue: the Integration Hub's stand-in for a downstream system under
//! test.
//!
//! Implements `spec/11-mock-receivers.md` §1 and §2.
//!
//! # Adopted, not rewritten
//!
//! This began as `Integration-Hub-Beta/hl7_mock_receiver_rust`, a Rust
//! service that already existed alongside the Python. It has been moved
//! into the workspace and re-pointed at the shared crates —
//! [`ihb_bus`] for the queue, [`ihb_config`] for the environment,
//! [`ihb_runtime`] for shutdown — so there is one implementation of each of
//! those rather than two that can drift.
//!
//! Its behaviour is unchanged, and `spec/11-mock-receivers.md` §4.1 exists
//! to keep it that way: services under test parse these acknowledgements
//! and their tests assert on the fields.

pub mod ack;
pub mod config;
pub mod server;
pub mod sink;
pub mod topic;
