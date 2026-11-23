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
//! Replaying stored messages back onto the bus.
//!
//! Implements `spec/13-message-replay.md`.
//!
//! # A job, not a service
//!
//! §1.1: it runs to completion and exits — 0 on success, 1 on failure. No
//! health check, no signal handling. Everything else in this workspace is a
//! long-lived process; this one is not, and the difference shows in the
//! error handling: there is no backoff and no retry loop, because a job
//! that cannot do its work should stop and let an operator look.
//!
//! # What an operator is trusting it with
//!
//! §1.2: which messages belong to a replay batch is chosen in SQL, by a
//! person, before the job runs. The job does not select — it delivers what
//! it was given. That is deliberate: a replay puts patient records back
//! onto a live clinical feed, and the decision of *which* records belongs
//! to someone who can be asked why.

pub mod job;
pub mod page;
pub mod retry;
pub mod sql;

pub use job::{JobError, fetch, mark, send};
pub use page::{Row, message_for};
pub use retry::{SendFailure, classify};
pub use sql::{FETCH_SQL, MAX_IDENTIFIERS_PER_STATEMENT, Status, mark_sql};
