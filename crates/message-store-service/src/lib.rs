#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! The message store service.
//!
//! Implements `spec/12-message-store.md`.
//!
//! The only writer of `monitoring.Message`, and therefore the only place
//! patient data is stored deliberately (§2.3, and `spec/index.md` §0.8
//! clause 2).
//!
//! # All or nothing
//!
//! §1.1: a batch is inserted in one transaction, and either every record
//! commits or none does and the whole batch is abandoned. §4.3 records what
//! that costs — one poison document blocks up to ninety-nine good ones for
//! ten redeliveries — and says it is preserved deliberately rather than
//! fixed, because changing it changes the failure semantics operators are
//! used to.

pub mod batch;
pub mod insert;
pub mod record;

pub use batch::{BatchOutcome, WORKFLOW_ID, build_all};
pub use record::{Record, RecordError, build};
