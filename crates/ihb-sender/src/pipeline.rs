//! What happens to one message on its way out.
//!
//! Implements `spec/10-senders.md` §2.

use std::future::Future;

use ihb_bus::message::{Incoming, Outgoing};
use ihb_bus::metadata::LogValues;
use ihb_bus::sink::MessageSink;
use ihb_bus::store::StoreEvent;
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::throttle::Throttle;

/// The audit text on `MESSAGE_RECEIVED` (§2 step 4).
pub const RECEIVED_AUDIT_TEXT: &str = "Message received for HL7 sending";

/// Whether the receiver accepted the message.
///
/// Two states, deliberately. `spec/10-senders.md` §3.7.1: a negative
/// acknowledgement is retried like any other failure, because nothing at
/// this layer can tell a permanent rejection from a transient one. Service
/// Bus's `MaxDeliveryCount` is what eventually gives up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A positive acknowledgement. Complete the message.
    Accepted {
        /// The acknowledgement, for the audit text of §2 step 11.
        acknowledgement: String,
    },
    /// Anything else. Abandon it.
    Rejected {
        /// Why, for the log line and the audit text.
        reason: String,
        /// The acknowledgement, when there was one to read.
        acknowledgement: String,
    },
}

impl Verdict {
    /// Whether the message may be completed.
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }

    /// What the receiver said, for the audit text.
    #[must_use]
    pub fn acknowledgement(&self) -> &str {
        match self {
            Self::Accepted { acknowledgement }
            | Self::Rejected {
                acknowledgement, ..
            } => acknowledgement,
        }
    }
}

/// A delivery that did not complete.
///
/// The two variants are §2.6's two rows, and the distinction is not
/// cosmetic: they carry different `validation_result` strings into the
/// `MESSAGE_FAILED` event, and an operator's dashboard separates "the
/// receiver is down" from "something is wrong with our code".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    /// A timeout, or a connection that could not be made or kept.
    #[error("{0}")]
    Connection(String),

    /// Anything else.
    #[error("{0}")]
    Unexpected(String),
}

impl DeliveryError {
    /// The `validation_result` §2.6 pairs with this failure.
    #[must_use]
    pub const fn validation_result(&self) -> &'static str {
        match self {
            Self::Connection(_) => "Message sending failed - connection/timeout error",
            Self::Unexpected(_) => "Unexpected processing error",
        }
    }

    /// The log line §2.6 pairs with this failure.
    #[must_use]
    pub fn log_line(&self, message_id: &str) -> String {
        match self {
            Self::Connection(reason) => format!("Failed to send message {message_id}: {reason}"),
            Self::Unexpected(reason) => {
                format!("Unexpected error while processing message: {reason}")
            }
        }
    }
}

/// How a message reaches its destination.
///
/// MLLP (`spec/10-senders.md` §3) and SOAP (§4) implement this; everything
/// above it — the ordering, the throttle, the store write, the events — is
/// shared.
pub trait Delivery: Send {
    /// Deliver `message` and evaluate what came back.
    ///
    /// The implementation owns its own retry (§3.4, §4.3), because "retry
    /// once on a timeout" is a property of the transport rather than of the
    /// pipeline.
    ///
    /// # Errors
    ///
    /// [`DeliveryError`] when the message did not reach the receiver at
    /// all. A receiver that answered *negatively* is a
    /// [`Verdict::Rejected`], not an error — it is a successful exchange
    /// with an unwelcome result.
    fn deliver(
        &mut self,
        message: &str,
    ) -> impl Future<Output = Result<Verdict, DeliveryError>> + Send;
}

/// Take one received message through §2, in order.
///
/// Returns whether it may be completed.
///
/// # Steps that are easy to get wrong
///
/// **Step 6 is conditional** (§2.2). The store write happens on the *first*
/// delivery attempt only; a retry skips it, so a message redelivered twenty
/// times is stored once rather than twenty times.
///
/// **Step 11 fires even on a negative acknowledgement** (§2.1). That reads
/// oddly and is the Python's behaviour: a rejected message appears as a
/// `MESSAGE_PROCESSED` whose audit text names the code, not as a
/// `MESSAGE_FAILED`. The verdict travels in the return value.
pub async fn process_one<D, S>(
    delivery: &mut D,
    message: &Incoming,
    throttle: &mut Throttle,
    context: &Context<'_, S>,
) -> bool
where
    D: Delivery,
    S: MessageSink,
{
    let Context {
        store,
        store_session_id,
        microservice_id,
        peer_service,
        events,
        metrics,
    } = context;
    let (store, events, metrics) = (*store, *events, *metrics);

    // ---- Step 1: decode. -------------------------------------------
    let Ok(body) = std::str::from_utf8(&message.body) else {
        let error = DeliveryError::Unexpected("the message body is not valid UTF-8".to_string());
        tracing::error!("{}", error.log_line("unknown"));
        events.log_message_failed(
            &String::from_utf8_lossy(&message.body),
            &error.to_string(),
            Some(error.validation_result()),
            None,
        );
        return false;
    };

    // ---- Steps 2 and 3: the received metadata. ---------------------
    let values = LogValues::from_properties(&message.properties);
    tracing::info!(
        correlation_id = %values.correlation_id,
        workflow_id = %values.workflow_id,
        source_system = %values.source_system,
        message_received_at = %values.received_at,
        "received message metadata"
    );
    let correlation_id = ihb_bus::metadata::correlation_id_for_event(&values.correlation_id);

    // ---- Step 4. ---------------------------------------------------
    events.log_message_received(body, Some(RECEIVED_AUDIT_TEXT), correlation_id);

    // ---- Step 5: the control id. -----------------------------------
    let message_id = ihb_validation::parse(body)
        .map(|parsed| ihb_fields::read(&parsed, "MSH-10"))
        .unwrap_or_default();
    tracing::info!(message_id = %message_id, "sending message");

    // ---- Step 6: the store, first attempt only (§2.2). --------------
    if let Some(store) = store {
        // §2.2 — first-attempt detection, including the rule that an
        // unknown count counts as the first. That is
        // `Incoming::is_first_delivery`, which `spec/03-message-bus.md`
        // §5.3 already specifies and tests; naming it twice would be two
        // places for one rule to drift.
        if message.is_first_delivery() {
            store_message(
                store,
                body,
                &values,
                store_session_id,
                microservice_id,
                peer_service,
                events,
                correlation_id,
            )
            .await;
        } else {
            // Logged rather than silent: an operator looking at why a
            // message is not in the store twice needs to see this.
            tracing::info!(
                correlation_id = %values.correlation_id,
                delivery_count = ?message.delivery_count,
                "not a first delivery attempt; skipping the message store"
            );
        }
    }

    // ---- Step 7: throttle. -----------------------------------------
    throttle.acquire().await;

    // ---- Steps 8 and 9: deliver, and read the reply. ---------------
    let verdict = match delivery.deliver(body).await {
        Ok(verdict) => verdict,
        Err(error) => {
            // §2.6.
            tracing::error!("{}", error.log_line(&message_id));
            events.log_message_failed(
                body,
                &error.to_string(),
                Some(error.validation_result()),
                correlation_id,
            );
            return false;
        }
    };

    // ---- Step 10. --------------------------------------------------
    if verdict.is_accepted() {
        metrics.message_sent();
    }

    // ---- Step 11: even when the acknowledgement was negative (§2.1).
    events.log_message_processed(
        body,
        Some(&format!(
            "Message sent successfully, received ACK: {}",
            verdict.acknowledgement()
        )),
        correlation_id,
    );

    if let Verdict::Rejected { reason, .. } = &verdict {
        // Not a `MESSAGE_FAILED` — §2.1 is explicit that the event above is
        // what the Python emits. The log line is where the reason lives.
        tracing::warn!(message_id = %message_id, %reason, "the receiver did not accept the message");
    }

    // ---- Step 12. --------------------------------------------------
    verdict.is_accepted()
}

/// Everything a sender holds for its lifetime, as against per message.
///
/// A struct because the pipeline needs nine things and an argument list
/// that long is one nobody reads — and because six of the nine never change
/// between messages, which the shape now says.
pub struct Context<'a, S> {
    /// The audit copy, when this variant writes one
    /// (`spec/10-senders.md` §5.2).
    pub store: Option<&'a S>,
    /// The sender's own session, which the store event carries so replay
    /// restores the message onto the right flow (§2.5).
    pub store_session_id: String,
    /// `MICROSERVICE_ID`, for the store event's `ProcessingComponent`.
    pub microservice_id: String,
    /// `PEER_SERVICE`, for its `TargetSystem`.
    pub peer_service: String,
    /// Events.
    pub events: &'a EventLogger,
    /// Counters.
    pub metrics: &'a Metrics,
}

/// Step 6 — the audit copy (§2.5).
#[expect(
    clippy::too_many_arguments,
    reason = "every one is a distinct field of the store event; grouping \
              them into a struct would move the argument list rather than \
              shorten it"
)]
async fn store_message<S: MessageSink>(
    store: &S,
    body: &str,
    values: &LogValues,
    session_id: &str,
    microservice_id: &str,
    peer_service: &str,
    events: &EventLogger,
    correlation_id: Option<&str>,
) {
    // §2.5 — the schema-unaware conversion, and a failure here is not
    // fatal. A message with no XML rendering is still worth storing; the
    // raw payload is what replay uses.
    let xml = match ihb_validation::er7_to_xml(body) {
        Ok(xml) => Some(xml),
        Err(error) => {
            let detail = format!("Failed to generate XML payload for message store: {error}");
            tracing::error!(%error, "failed to generate XML payload for message store");
            events.log_validation_result(body, &detail, false, correlation_id);
            None
        }
    };

    let event = StoreEvent {
        message_received_at: values.received_at.clone(),
        correlation_id: values.correlation_id.clone(),
        source_system: values.source_system.clone(),
        processing_component: microservice_id.to_string(),
        target_system: Some(peer_service.to_string()),
        raw_payload: body.to_string(),
        xml_payload: xml,
        session_id: session_id.to_string(),
    };

    let json = match event.to_json() {
        Ok(json) => json,
        Err(error) => {
            tracing::error!(%error, "Failed to send to message store");
            return;
        }
    };

    let outgoing = Outgoing::new(json)
        .with_message_id(values.correlation_id.clone())
        .with_session(session_id.to_string());

    if let Err(error) = store.send(outgoing).await {
        // Non-blocking, as at ingress: losing the audit copy must not stop
        // a clinical message reaching MPI.
        tracing::error!(%error, "Failed to send to message store: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_failure_rows_of_section_two_six() {
        // spec/10-senders.md §2.6 — both strings, because an operator's
        // dashboard separates "the receiver is down" from "something is
        // wrong with our code".
        let connection = DeliveryError::Connection("timed out".to_string());
        assert_eq!(
            connection.validation_result(),
            "Message sending failed - connection/timeout error"
        );
        assert_eq!(
            connection.log_line("M1"),
            "Failed to send message M1: timed out"
        );

        let unexpected = DeliveryError::Unexpected("a bug".to_string());
        assert_eq!(
            unexpected.validation_result(),
            "Unexpected processing error"
        );
        assert_eq!(
            unexpected.log_line("M1"),
            "Unexpected error while processing message: a bug"
        );
    }

    #[test]
    fn a_verdict_carries_the_acknowledgement_either_way() {
        // §2 step 11 — the audit text names it whether or not it was
        // positive.
        let accepted = Verdict::Accepted {
            acknowledgement: "MSA|AA|M1".to_string(),
        };
        assert!(accepted.is_accepted());
        assert_eq!(accepted.acknowledgement(), "MSA|AA|M1");

        let rejected = Verdict::Rejected {
            reason: "Negative ACK received: AE for: M1".to_string(),
            acknowledgement: "MSA|AE|M1".to_string(),
        };
        assert!(!rejected.is_accepted());
        assert_eq!(rejected.acknowledgement(), "MSA|AE|M1");
    }
}
