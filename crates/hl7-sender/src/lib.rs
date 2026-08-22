#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
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
