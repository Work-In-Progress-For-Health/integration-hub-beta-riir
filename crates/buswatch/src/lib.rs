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
//! A Service Bus queue inspector for local development.
//!
//! Implements `spec/14-buswatch.md`.
//!
//! # Read this before deploying it anywhere
//!
//! §1.2: **BusWatch MUST NOT be deployed into an environment holding real
//! patient data.** Its whole purpose is displaying message bodies and it
//! applies no redaction — every other service in this workspace redacts by
//! default (`spec/04-observability.md` §2), and this one deliberately does
//! not, because a developer looking at why a message failed needs to see
//! the message.
//!
//! That is a decision, not an omission, and it is stated in the
//! specification, in this documentation and on the page itself so nobody
//! has to infer it.
//!
//! §1.1: it **peeks**. It does not consume, and the only mutation it offers
//! is clearing a queue, which is an emulator operation.

pub mod config;
pub mod error;
pub mod fake;
pub mod live;
pub mod reader;
pub mod render;
pub mod snapshot;
pub mod web;

pub use config::{Config, discover_queues, discover_session_queues};
pub use error::{QueueError, quota_hint};
pub use reader::{MessageDetail, QueueReader};
pub use snapshot::{MessageSummary, Snapshot};
