//! The receive loop, and what happens to one message inside it.
//!
//! Implements `spec/09-transformers.md` §1.1, §1.2, §1.3, §1.6 and §1.7.

use ihb_bus::message::{Incoming, Outgoing};
use ihb_bus::metadata::LogValues;
use ihb_bus::sink::MessageSink;
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::config::Config;
use crate::{PayloadTransformer, TransformError};

/// What became of one message.
///
/// `spec/09-transformers.md` §1.7 makes this more consequential than it
/// looks: [`Outcome::NotHandled`] abandons the message **and every message
/// behind it in the batch**, because order within a session is the contract
/// (`spec/01-architecture.md` §6.2). There is no "skip this one and carry
/// on".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Transformed and forwarded. Complete it.
    Handled,
    /// Not forwarded. Abandon it, and everything after it.
    NotHandled {
        /// Why, for the log line and the event.
        reason: String,
    },
}

impl Outcome {
    /// Whether the message may be completed.
    #[must_use]
    pub const fn is_handled(&self) -> bool {
        matches!(self, Self::Handled)
    }

    /// A failure carrying `reason`.
    #[must_use]
    pub fn not_handled(reason: impl Into<String>) -> Self {
        Self::NotHandled {
            reason: reason.into(),
        }
    }
}

/// Take one received message through §1.3, in order.
///
/// Separated from the loop so it can be tested against a fake bus without a
/// broker, a health check or a signal handler — which is what
/// `spec/09-transformers.md` §6.7 asks for.
///
/// # The properties are forwarded, not rebuilt
///
/// Step 7, and §1.3.1. This is the single most important line in the
/// function: the `application_properties` that arrived go out unchanged, so
/// the correlation id minted at ingress reaches MPI. Rebuilding them here
/// would produce a message that looks correct and is untraceable.
pub async fn process_one<T, E>(
    transformer: &mut T,
    message: &Incoming,
    egress: &E,
    egress_session_id: Option<&str>,
    events: &EventLogger,
) -> Outcome
where
    T: PayloadTransformer,
    E: MessageSink,
{
    // ---- Step 1: decode. -------------------------------------------
    let Ok(body) = std::str::from_utf8(&message.body) else {
        // Not in §1.6's table because the Python could not reach it: it
        // decoded with `errors="replace"` and mapped the replacement
        // characters. `spec/index.md` §0.6 forbids forwarding a payload
        // that is not valid UTF-8, so it is refused here instead.
        let reason = "the message body is not valid UTF-8".to_string();
        tracing::error!("Unexpected error during message processing: {reason}");
        events.log_message_failed(
            &String::from_utf8_lossy(&message.body),
            &reason,
            Some("Unexpected processing error"),
            None,
        );
        return Outcome::not_handled(reason);
    };

    // ---- Steps 2 and 3: the incoming metadata. ---------------------
    let values = LogValues::from_properties(&message.properties);
    if message.properties.is_empty() {
        tracing::warn!("No application_properties found on message");
    } else {
        tracing::info!(
            correlation_id = %values.correlation_id,
            workflow_id = %values.workflow_id,
            source_system = %values.source_system,
            message_received_at = %values.received_at,
            "received message metadata"
        );
    }
    let correlation_id = ihb_bus::metadata::correlation_id_for_event(&values.correlation_id);

    // ---- Step 4. ---------------------------------------------------
    events.log_message_received(
        body,
        Some(&transformer.received_audit_text()),
        correlation_id,
    );

    // ---- Steps 5 and 6: parse and transform. -----------------------
    //
    // Both live behind `PayloadTransformer` so the PROMs transformer, whose
    // payload is XML rather than ER7, runs this same loop
    // (`spec/16-proms-transformer.md` §1.1). For an ER7 transformer the
    // blanket implementation does the parse, and reports a failure as
    // `Unexpected` so §1.6's second row keeps its own wording.
    let mapped = match transformer.transform_payload(body) {
        Ok(mapped) => mapped,
        Err(TransformError::Unexpected(reason)) => {
            // §1.6's second row.
            let reason = format!("Unexpected error during message processing: {reason}");
            tracing::error!(%reason, "Unexpected error during message processing");
            events.log_message_failed(
                body,
                &reason,
                Some("Unexpected processing error"),
                correlation_id,
            );
            return Outcome::not_handled(reason);
        }
        Err(error) => {
            // §1.6's first row.
            return failed_to_transform(transformer, body, &error, events, correlation_id);
        }
    };

    // ---- Step 7: forward, with the properties as received. ---------
    let outgoing = Outgoing::new(mapped.clone())
        // §1.3.2 — no message id. Only ingress sets one.
        .with_optional_session(egress_session_id)
        .with_properties(message.properties.clone());

    if let Err(error) = egress.send(outgoing).await {
        let reason = format!("Unexpected error during message processing: {error}");
        tracing::error!(%error, "Unexpected error during message processing");
        events.log_message_failed(
            body,
            &reason,
            Some("Unexpected processing error"),
            correlation_id,
        );
        return Outcome::not_handled(reason);
    }

    // ---- Step 8. ---------------------------------------------------
    events.log_message_processed(
        body,
        Some(&transformer.processed_audit_text_for(&mapped)),
        correlation_id,
    );

    // ---- Step 9. ---------------------------------------------------
    Outcome::Handled
}

/// §1.6's first row, in one place so the two strings cannot drift apart.
fn failed_to_transform<T: PayloadTransformer>(
    transformer: &T,
    body: &str,
    error: &TransformError,
    events: &EventLogger,
    correlation_id: Option<&str>,
) -> Outcome {
    let name = transformer.name();
    let reason = format!("Failed to transform {name} message: {error}");

    tracing::error!(%error, transformer = name, "Failed to transform message");
    events.log_message_failed(
        body,
        &reason,
        Some(&format!("{name} transformation failed")),
        correlation_id,
    );

    Outcome::not_handled(reason)
}

/// The pieces a running transformer holds.
///
/// Assembled by each transformer's binary; `spec/09-transformers.md` §1.1.
pub struct Runtime<T, E> {
    /// This deployment.
    pub config: Config,
    /// The mapping.
    pub transformer: T,
    /// Where transformed messages go.
    pub egress: E,
    /// Events.
    pub events: EventLogger,
    /// Counters.
    pub metrics: Metrics,
}

impl<T: PayloadTransformer, E: MessageSink> Runtime<T, E> {
    /// Assemble a runtime.
    #[must_use]
    pub fn new(config: Config, transformer: T, egress: E) -> Self {
        let events = EventLogger::new(&config.workflow_id, &config.microservice_id)
            .with_redaction(config.redaction_enabled);
        let metrics = Metrics::new(
            &config.workflow_id,
            &config.microservice_id,
            &config.health_board,
            &config.peer_service,
        );
        Self {
            config,
            transformer,
            egress,
            events,
            metrics,
        }
    }

    /// Process one batch, stopping at the first failure.
    ///
    /// Implements `spec/03-message-bus.md` §5.2.1 and §5.2.2 over
    /// `spec/09-transformers.md` §1.7. Returns the outcome per message, in
    /// order; a [`Outcome::NotHandled`] is the **last** entry, because
    /// everything behind it is abandoned unattempted.
    pub async fn process_batch(&mut self, batch: &[Incoming]) -> Vec<Outcome> {
        let mut outcomes = Vec::with_capacity(batch.len());

        for message in batch {
            self.metrics.message_received();
            let outcome = process_one(
                &mut self.transformer,
                message,
                &self.egress,
                self.config.egress_session_id.as_deref(),
                &self.events,
            )
            .await;

            let handled = outcome.is_handled();
            if handled {
                self.metrics.message_sent();
            }
            outcomes.push(outcome);

            if !handled {
                // §1.7 — not "continue with the next". Within a session,
                // order is the contract, and a message that jumped ahead of
                // one that failed would reach MPI out of sequence.
                tracing::warn!(
                    remaining = batch.len() - outcomes.len(),
                    "abandoning the rest of the batch behind a failed message"
                );
                break;
            }
        }

        outcomes
    }
}
