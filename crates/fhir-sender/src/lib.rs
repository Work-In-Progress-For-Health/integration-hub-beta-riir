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
//! The FHIR sender.
//!
//! Implements `spec/10-senders.md` §8. Upstream: `senders/fhir_sender`
//! (`#441`, `#442`, `#448`).
//!
//! One queue session in, one `$process-message` delivery to Promptly out,
//! using the WPAS-Promptly IG's asynchronous pattern: `POST` the message
//! Bundle, expect `202` and a `Content-Location`, poll that URL until it
//! answers `200` with an `OperationOutcome`.
//!
//! # The rules that govern it
//!
//! - **A message is completed only when Promptly reported a non-error
//!   `OperationOutcome`** (`§8.1`, `spec/10-senders.md` §1.1).
//! - **Credentials are never sent anywhere but the configured origin**
//!   (`§8.4`): redirects are not followed and the status URL must share the
//!   endpoint's scheme and host.
//! - **No Bundle reaches a log or an event** (`§8.6`); the audit events carry
//!   only its size.
//!
//! | Module | Owns |
//! | --- | --- |
//! | [`config`] | the environment (`§8.7`) |
//! | [`error`] | the three failure classes (`§8.2`) |
//! | [`http`] | one request, over plain TCP or TLS, behind a trait |
//! | [`auth`] | API key and OAuth2 client credentials (`§8.3`) |
//! | [`client`] | the `POST`, the poll and the retry policy (`§8.2`, `§8.4`) |
//! | [`processor`] | one message, start to disposition (`§8.5`) |
//! | [`main_loop`] | assembling and running the service |

pub mod auth;
pub mod client;
pub mod config;
pub mod error;
pub mod http;
pub mod main_loop;
pub mod processor;

pub use client::{Clock, FhirClient, TokioClock};
pub use config::AppConfig;
pub use error::DeliveryError;
pub use processor::{Disposition, process_batch, process_one};
