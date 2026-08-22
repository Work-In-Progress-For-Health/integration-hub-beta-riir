#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! The HL7 SOAP ingress server.
//!
//! Implements `spec/08-hl7-soap-server.md`.

pub mod config;
pub mod http;
pub mod payload;
pub mod process;
pub mod serve;
