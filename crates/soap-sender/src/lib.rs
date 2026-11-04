#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! SOAP egress.
//!
//! Implements `spec/10-senders.md` §4, over the shared pipeline of
//! [`ihb_sender`].

pub mod client;
pub mod envelope;

pub use client::{Endpoint, SoapClient};
pub use envelope::{Format, escape, evaluate, wrap, wrap_wis};
