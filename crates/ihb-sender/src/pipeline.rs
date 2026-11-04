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

/// The audit text on `MESSAGE_RECEIVED` (`spec/10-senders.md` §2 step 4).
/// Kept verbatim from the Python audit text.
pub const RECEIVED_AUDIT_TEXT: &str = "Message received for HL7 sending";

/// Which kind of negative acknowledgement an HL7 receiver sent
/// (`spec/10-senders.md` §3.7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NackClass {
    /// `MSA-1 = AE`, or any code that is not recognised: retry.
    Recoverable,
    /// Malformed, or no `MSA`: retry, but there is no code to report.
    Invalid,
    /// `MSA-1 = AR`: the receiver will never accept it. Dead-letter.
    Reject,
}

impl NackClass {
    /// The word the audit text uses: `AE`, `INVALID` or `AR`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Recoverable => "AE",
            Self::Invalid => "INVALID",
            Self::Reject => "AR",
        }
    }
}

/// Whether the receiver accepted the message.
///
/// `spec/10-senders.md` §3.7.1: an HL7 negative acknowledgement is a
/// [`Verdict::Nack`], which distinguishes a permanent rejection (`AR`) from
/// a retryable one. Every other refusal is [`Verdict::Rejected`], retried
/// like any other failure, because nothing at that layer can tell the two
/// apart; Service Bus's `MaxDeliveryCount` is what eventually gives up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A positive acknowledgement. Complete the message.
    Accepted {
        /// The acknowledgement, for the audit text of §2 step 11.
        acknowledgement: String,
    },
    /// An HL7 negative acknowledgement, classified (§3.7.1).
    Nack {
        /// How to treat it.
        class: NackClass,
        /// `MSA-1`, when there was one to read.
        code: Option<String>,
        /// Why, for the log line.
        reason: String,
        /// The acknowledgement, when there was one to read.
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
            | Self::Nack {
                acknowledgement, ..
            }
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
    /// `host:port` of the receiver, as a metric dimension on a negative
    /// acknowledgement. Empty when the transport has no such notion.
    fn endpoint(&self) -> String {
        String::new()
    }

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

/// What to do with a message once the pipeline is finished with it.
///
/// `Retry` and `DeadLetter` both mean the rest of the batch waits
/// (`spec/10-senders.md` §3.7.1): the caller abandons it rather than
/// delivering later messages past an earlier failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// Positively acknowledged. Complete it.
    Delivered,
    /// Not delivered. Abandon it, and everything behind it.
    Retry,
    /// Rejected for good (`MSA-1 = AR`, `spec/10-senders.md` §3.7.1).
    /// Dead-letter it, and abandon everything behind it.
    DeadLetter {
        /// The dead-letter reason: the acknowledgement code.
        reason: String,
        /// The dead-letter description. Names the control and correlation
        /// ids, never the message.
        description: String,
    },
}

impl Settlement {
    /// Whether the message was delivered.
    #[must_use]
    pub const fn is_delivered(&self) -> bool {
        matches!(self, Self::Delivered)
    }
}

/// Take one received message through §2, in order.
///
/// Returns what to do with it.
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
// The body is the numbered step sequence of §2.1 in order. The steps share
// the delivery, the throttle and the audit context, and several are
// conditional on earlier ones (step 6 on the attempt count, step 11 on the
// verdict); splitting them into helpers would hide that order, which is the
// thing a reader of this function needs to see.
#[allow(clippy::too_many_lines)]
pub async fn process_one<D, S>(
    delivery: &mut D,
    message: &Incoming,
    throttle: &mut Throttle,
    context: &Context<'_, S>,
) -> Settlement
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
        return Settlement::Retry;
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
            // §2.5 — the session the message actually arrived on wins over
            // the deployment's fallback, so replay routing stays correct
            // when one consumer takes messages from several sessions.
            let session = message
                .session_id
                .as_deref()
                .filter(|session| !session.trim().is_empty())
                .unwrap_or(store_session_id);
            store_message(
                store,
                body,
                &values,
                session,
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
            return Settlement::Retry;
        }
    };

    // ---- Steps 10 and 11. -------------------------------------------
    match verdict {
        Verdict::Nack {
            class,
            code,
            reason,
            ..
        } => {
            // §3.7.1 — a negative acknowledgement is a failed delivery, and
            // says so. The pre-`368` behaviour of logging it as processed
            // is gone with the single-state verdict.
            let endpoint = delivery.endpoint();
            let code_label = code.as_deref().unwrap_or("UNKNOWN");
            let attributes = [
                ("ack_code", code_label),
                ("correlation_id", values.correlation_id.as_str()),
                ("message_id", message_id.as_str()),
                ("endpoint", endpoint.as_str()),
            ];
            let failure = format!(
                "{} ACK ({}) received for message {message_id} from {endpoint}",
                if class == NackClass::Reject {
                    "Application Reject"
                } else {
                    "Negative"
                },
                class.label(),
            );
            tracing::error!(%reason, "{failure}");

            if class == NackClass::Reject {
                events.log_message_failed(
                    body,
                    &failure,
                    Some(
                        "Non-recoverable NACK (AR) - message routed to dead-letter/escalation path",
                    ),
                    correlation_id,
                );
                metrics.count(ihb_telemetry::metrics::MESSAGES_NACK_AR, 1, &attributes);
                return Settlement::DeadLetter {
                    reason: "AR".to_string(),
                    description: format!(
                        "Application Reject ACK (AR) for message_id={message_id}, correlation_id={}",
                        values.correlation_id
                    ),
                };
            }

            events.log_message_failed(
                body,
                &failure,
                Some(&format!(
                    "Recoverable NACK ({}) - message will be retried",
                    class.label()
                )),
                correlation_id,
            );
            if class == NackClass::Recoverable {
                metrics.count(ihb_telemetry::metrics::MESSAGES_NACK_AE, 1, &attributes);
                metrics.count(
                    ihb_telemetry::metrics::MESSAGES_RETRY_ATTEMPT,
                    1,
                    &attributes,
                );
            }
            Settlement::Retry
        }
        verdict => {
            // ---- Step 10. -----------------------------------------
            if verdict.is_accepted() {
                metrics.message_sent();
            }

            // ---- Step 11: even when the acknowledgement was negative
            // (§2.1), for the transports that do not classify it.
            events.log_message_processed(
                body,
                Some(&format!(
                    "Message sent successfully, received ACK: {}",
                    verdict.acknowledgement()
                )),
                correlation_id,
            );

            if let Verdict::Rejected { reason, .. } = &verdict {
                // Not a `MESSAGE_FAILED` — §2.1 is explicit that the event
                // above is what the Python emits. The log line is where
                // the reason lives.
                tracing::warn!(message_id = %message_id, %reason, "the receiver did not accept the message");
            }

            // ---- Step 12. -----------------------------------------
            if verdict.is_accepted() {
                Settlement::Delivered
            } else {
                Settlement::Retry
            }
        }
    }
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

/// Step 6 — the audit copy (`spec/10-senders.md` §2.5).
///
/// A refusal or failure here is logged and the delivery goes on
/// (AGENTS.md: a failure to persist is swallowed, a failure to forward is not).
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
    // §2.5 — a stored message with no session cannot be replayed onto a
    // flow. Refused, and logged; the delivery goes on.
    if session_id.trim().is_empty() {
        tracing::error!(
            "Failed to send to message store: a non-empty replay-routing session ID is required"
        );
        return;
    }

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

    /// A delivery that always answers with the verdict it was given.
    struct Canned(Verdict);

    impl Delivery for Canned {
        fn endpoint(&self) -> String {
            "mpi.example:2575".to_string()
        }

        // `Delivery::deliver` is an async trait method; this stub has nothing
        // to await.
        #[allow(clippy::unused_async_trait_impl)]
        async fn deliver(&mut self, _message: &str) -> Result<Verdict, DeliveryError> {
            Ok(self.0.clone())
        }
    }

    async fn settle_with(verdict: Verdict) -> Settlement {
        let events = EventLogger::new("wf", "ms");
        let metrics = Metrics::new("wf", "ms", "PHW", "MPI");
        let context: Context<'_, ihb_bus::fake::FakeSender> = Context {
            store: None,
            store_session_id: String::new(),
            microservice_id: "ms".to_string(),
            peer_service: "MPI".to_string(),
            events: &events,
            metrics: &metrics,
        };
        let message = Incoming {
            body: b"MSH|^~\\&|A|B|C|D|20250505||ADT^A31|M1|P|2.5".to_vec(),
            message_id: None,
            session_id: None,
            properties: ihb_bus::Properties::default(),
            delivery_count: None,
            token: ihb_bus::message::Token::fake(1),
        };
        let mut throttle = Throttle::per_minute(None).unwrap();
        process_one(&mut Canned(verdict), &message, &mut throttle, &context).await
    }

    fn nack(class: NackClass, code: Option<&str>) -> Verdict {
        Verdict::Nack {
            class,
            code: code.map(str::to_string),
            reason: "Negative ACK".to_string(),
            acknowledgement: "MSA|..".to_string(),
        }
    }

    #[tokio::test]
    async fn an_application_reject_is_dead_lettered_not_retried() {
        // spec/10-senders.md §3.7.1 — the reason is the code, and the
        // description names ids only; the payload never reaches a
        // dead-letter queue's metadata.
        let settlement = settle_with(nack(NackClass::Reject, Some("AR"))).await;

        let Settlement::DeadLetter {
            reason,
            description,
        } = settlement
        else {
            panic!("expected a dead-letter, got {settlement:?}")
        };
        assert_eq!(reason, "AR");
        assert!(description.contains("message_id=M1"), "{description}");
        assert!(!description.contains("ADT"), "{description}");
    }

    #[tokio::test]
    async fn a_recoverable_or_invalid_nack_is_retried() {
        for verdict in [
            nack(NackClass::Recoverable, Some("AE")),
            nack(NackClass::Invalid, None),
        ] {
            assert_eq!(settle_with(verdict).await, Settlement::Retry);
        }
    }

    #[tokio::test]
    async fn a_positive_acknowledgement_is_delivered() {
        let accepted = Verdict::Accepted {
            acknowledgement: "MSA|AA|M1".to_string(),
        };
        assert_eq!(settle_with(accepted).await, Settlement::Delivered);
    }
}
