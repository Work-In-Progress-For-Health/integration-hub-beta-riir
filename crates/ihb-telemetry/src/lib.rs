//! Events, redaction, metrics and tracing for the Integration Hub.
//!
//! Implements `spec/04-observability.md`.
//!
//! Every name this crate emits — event types, metric names, dimension keys,
//! field names — is consumed by Log Analytics queries, by the NOC dashboard,
//! and by the alert rules in `Integration-Hub-Beta/docs/ALERT_STRATEGY.md`.
//! They are frozen: a rename here silently breaks an alert somebody is
//! relying on to notice a stalled flow.
//!
//! # Modules
//!
//! - [`redact`] — removing patient detail from a payload before it is
//!   logged. Read this one first; it is a patient-safety control and
//!   everything else in this crate goes through it.
//! - [`clock`] — where timestamps come from, and how they are written.
//! - [`events`] — the audit events every component emits.
//! - [`metrics`] — the counters and histograms every component emits.
//! - [`otel`] — logging setup, W3C trace context across a queue, and the
//!   handler span.

#![forbid(unsafe_code)]
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
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

pub mod clock;
pub mod events;
pub mod metrics;
pub mod otel;
pub mod redact;

pub use clock::{Clock, FixedClock, SharedClock, SystemClock};
pub use events::{EventLogger, EventType, LogEvent};
pub use metrics::{Dimensions, Metrics};
pub use otel::{Config as OtelConfig, handler_span};
pub use redact::redact_hl7;
