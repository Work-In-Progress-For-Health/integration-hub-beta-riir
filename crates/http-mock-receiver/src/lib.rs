#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! A SOAP endpoint that accepts anything vaguely SOAP-shaped and answers.
//!
//! Implements `spec/11-mock-receivers.md` §3.

pub mod server;
pub mod soap;
