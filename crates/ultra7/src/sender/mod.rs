//! The three protocol senders.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§7`.

mod http;
mod mllp;

use std::time::Duration;

pub use http::{send_rest, send_soap, url_host_port, wrap_in_envelope};
pub use mllp::{extract_ack_code, send_mllp, to_er7};

use crate::model::{Endpoint, EndpointKind, Message};

/// The outcome of one send (`§7.1`). A transport failure is a value here,
/// not an `Err`, so a run can report it and carry on.
#[derive(Debug, Clone, PartialEq)]
pub struct SendResult {
    /// Whether the endpoint accepted the message.
    pub ok: bool,
    /// Wall time of the exchange.
    pub latency_ms: f64,
    /// The reply, for the invoking terminal only (`§1.3`).
    pub response_summary: String,
    /// Why it failed, when it did.
    pub error: Option<String>,
}

impl SendResult {
    /// A failed result with a reason.
    pub(crate) fn failure(
        latency_ms: f64,
        response_summary: String,
        error: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            latency_ms,
            response_summary,
            error: Some(error.into()),
        }
    }
}

/// Send `message` to `endpoint` by the endpoint's protocol.
///
/// Never panics and never returns an error: every outcome, including a
/// refused connection, is a [`SendResult`] (`§7.1`).
#[must_use]
pub fn send(endpoint: &Endpoint, message: &Message) -> SendResult {
    match endpoint.kind {
        EndpointKind::Mllp => send_mllp(endpoint, message),
        EndpointKind::Rest => send_rest(endpoint, message),
        EndpointKind::Soap => send_soap(endpoint, message),
    }
}

/// The endpoint timeout as a `Duration`; a zero, negative or non-finite
/// value falls back to the 5 s default rather than panicking.
pub(crate) fn timeout_of(endpoint: &Endpoint) -> Duration {
    Duration::try_from_secs_f64(endpoint.timeout_seconds)
        .ok()
        .filter(|d| !d.is_zero())
        .unwrap_or(Duration::from_secs(5))
}
