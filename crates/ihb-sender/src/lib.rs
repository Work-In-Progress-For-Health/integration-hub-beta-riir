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
//! The sender runtime.
//!
//! Implements `spec/10-senders.md` §1, §2 and §5.
//!
//! Four services — `hl7-sender`, `hl7-subscription-sender`, `soap-sender`
//! and `soap-subscription-sender` — are the same loop with a different
//! [`Delivery`] in the middle and a different receiver at the front.
//!
//! # The rule that governs everything here
//!
//! §1.1: **a sender MUST NOT complete a message it did not deliver.**
//! Anything short of a positive acknowledgement is "not handled", the
//! message is abandoned, and Service Bus redelivers it. A sender that
//! completed on a negative acknowledgement would discard a patient record
//! and report success.
//!
//! That is why there is no "delivered but rejected, so let it go" outcome.
//! Per §3.7.1 an `AE` (or unrecognised, or unparseable) reply is abandoned
//! and redelivered, and only an explicit `AR` is dead-lettered at once
//! ([`Settlement::DeadLetter`]); the SOAP senders keep the single
//! [`Verdict::Rejected`] state and rely on `MaxDeliveryCount`. (An earlier
//! revision of this doc said `AR` was retried too; §3.7.1 supersedes that
//! rule and the pipeline implements the newer one.)

pub mod config;
pub mod main_loop;
pub mod pipeline;
pub mod throttle;

pub use pipeline::{Context, Delivery, DeliveryError, NackClass, Settlement, Verdict, process_one};
pub use throttle::Throttle;
