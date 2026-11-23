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
//! MLLP egress.
//!
//! Implements `spec/10-senders.md` §3, over the shared pipeline of
//! [`ihb_sender`].
//!
//! Two binaries, differing only in where they read from
//! (`spec/10-senders.md` §5.1):
//!
//! - `hl7-sender` reads a queue and writes to the message store.
//! - `hl7-subscription-sender` reads a topic subscription and does not
//!   (§5.2 — the publisher already stored it).

pub mod ack;
pub mod client;

pub use ack::evaluate;
pub use client::{Destination, MllpClient};
