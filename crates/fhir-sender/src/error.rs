//! Delivery failure classes.
//!
//! Implements `spec/10-senders.md` §8.2. Upstream: `fhir_sender/errors.py`.

/// Why a delivery did not complete.
///
/// The processor maps each class to a Service Bus outcome (`§8.5`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    /// Promptly rejected the message (`400`, `404`, `409`, any other `4xx`)
    /// or reported an `error` or `fatal` issue. Retrying cannot help:
    /// dead-letter.
    #[error("HTTP {status}: {detail}")]
    Permanent {
        /// The status of the response that carried the rejection.
        status: u16,
        /// A log-safe summary, at most 500 characters.
        detail: String,
    },
    /// The credentials were rejected (`401`, `403`) or a token could not be
    /// obtained. Alert and abandon: redrive after the credentials are fixed.
    #[error("HTTP {status}: {detail}")]
    Authorization {
        /// The status that was returned.
        status: u16,
        /// What was rejected. Never the credential.
        detail: String,
    },
    /// Delivery could not be confirmed (network, `429`, `5xx`, a polling
    /// timeout) after every attempt. Abandon: Service Bus redelivers.
    #[error("{0}")]
    Transient(String),
}
