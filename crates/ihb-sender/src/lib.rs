#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
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
//! That is why [`Verdict`] has no third state. There is no "delivered but
//! rejected, so let it go" — §3.7.1 is explicit that `AE` and `AR` are both
//! retried, and `MaxDeliveryCount` is what eventually dead-letters them.

pub mod config;
pub mod main_loop;
pub mod pipeline;
pub mod throttle;

pub use pipeline::{Context, Delivery, DeliveryError, Verdict, process_one};
pub use throttle::Throttle;
