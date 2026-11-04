#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! The REST HL7 ingress.
//!
//! Implements `spec/19-rest-servers.md` §1 to §8 and `spec/02-configuration.md`
//! §11.
//!
//! This crate is the pipeline `rest-server` runs for `PIPELINE=hl7` as well
//! as the standalone `hl7-rest-server` service (`spec/19-rest-servers.md`
//! §0.1), so most of what is public here is public for that reason.

pub mod ack;
pub mod config;
pub mod http;
pub mod input;
pub mod process;
pub mod risp;
pub mod routing;
pub mod rules;
pub mod serve;
