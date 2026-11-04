#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! The REST ingress server.
//!
//! Implements `spec/19-rest-servers.md` §9 and §10, and `spec/02-configuration.md`
//! §12.
//!
//! One image, two pipelines (`PIPELINE`): `generic`, which unwraps a SOAP or
//! plain-XML request, validates it and forwards it; and `hl7`, which is
//! `hl7-rest-server`'s pipeline, served through the same listener
//! (`spec/19-rest-servers.md` §0.1).

pub mod adapter;
pub mod config;
pub mod http;
pub mod process;
pub mod validator;
pub mod xml;
