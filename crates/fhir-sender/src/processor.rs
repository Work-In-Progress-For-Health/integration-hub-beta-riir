//! One message, from receipt to disposition.
//!
//! Implements `spec/10-senders.md` §8.5 and §8.6. Upstream:
//! `fhir_sender/application.py :: _process_message`.

use ihb_bus::message::Incoming;
use ihb_bus::metadata::{LogValues, correlation_id_for_event};
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::client::{Clock, FhirClient};
use crate::error::DeliveryError;
use crate::http::Transport;

/// Dead-letter reason for a body that is not a message Bundle.
pub const INVALID_BUNDLE_REASON: &str = "InvalidFhirBundle";

/// Dead-letter reason for a delivery Promptly refused for good.
pub const REJECTED_REASON: &str = "PromptlyRejected";

/// The longest dead-letter description (`spec/10-senders.md` §8.5).
pub const MAX_DEAD_LETTER_DESCRIPTION: usize = 1000;

/// What to do with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Promptly confirmed it. Complete it.
    Complete,
    /// Not delivered, and worth trying again. Abandon it, and everything
    /// behind it in the batch (`spec/03-message-bus.md` §5.2.2).
    Abandon,
    /// It can never be delivered. Dead-letter it and carry on.
    DeadLetter {
        /// The reason.
        reason: String,
        /// Why, for the operator. Never the Bundle.
        description: String,
    },
}

/// Check that `body` is a FHIR `Bundle` of type `message`, before spending a
/// request on it.
///
/// # Errors
///
/// The reason, in upstream's words.
///
/// # Examples
///
/// ```
/// use fhir_sender::processor::validate_bundle;
///
/// assert!(validate_bundle(r#"{"resourceType":"Bundle","type":"message"}"#).is_ok());
/// assert!(validate_bundle(r#"{"resourceType":"Bundle","type":"collection"}"#).is_err());
/// assert!(validate_bundle("<xml/>").is_err());
/// ```
pub fn validate_bundle(body: &str) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| format!("Message body is not valid JSON: {error}"))?;
    let is_message_bundle = value
        .get("resourceType")
        .and_then(serde_json::Value::as_str)
        == Some("Bundle")
        && value.get("type").and_then(serde_json::Value::as_str) == Some("message");
    if is_message_bundle {
        Ok(())
    } else {
        Err("Message body is not a FHIR Bundle of type 'message'".to_string())
    }
}

/// What the audit events carry in place of the Bundle (`§8.6`).
///
/// Upstream logs the whole Bundle. A Bundle is a patient record, and
/// `AGENTS.md` forbids patient data in a log, so the events carry its size.
fn audit_text(body_len: usize) -> String {
    format!("FHIR Bundle, {body_len} bytes (content not logged)")
}

/// Take one message through `§8.5`.
///
/// The message is **completed only when Promptly confirmed delivery**
/// (`spec/10-senders.md` §1.1).
pub async fn process_one<T: Transport, C: Clock>(
    message: &Incoming,
    client: &mut FhirClient<T, C>,
    events: &EventLogger,
    metrics: &Metrics,
) -> Disposition {
    let values = LogValues::from_properties(&message.properties);
    let correlation = correlation_id_for_event(&values.correlation_id);

    let Ok(body) = std::str::from_utf8(&message.body) else {
        tracing::error!(correlation_id = %values.correlation_id, "message body is not valid UTF-8");
        return Disposition::DeadLetter {
            reason: INVALID_BUNDLE_REASON.into(),
            description: "Message body is not valid UTF-8".into(),
        };
    };
    let audit = audit_text(body.len());

    tracing::info!(
        correlation_id = %values.correlation_id,
        workflow_id = %values.workflow_id,
        source_system = %values.source_system,
        "message received for FHIR sending"
    );
    events.log_message_received(
        &audit,
        Some("Message received for FHIR sending"),
        correlation,
    );

    if let Err(reason) = validate_bundle(body) {
        events.log_message_failed(
            &audit,
            &truncate(&reason, MAX_DEAD_LETTER_DESCRIPTION),
            Some("Invalid FHIR Bundle - dead-lettered"),
            correlation,
        );
        return Disposition::DeadLetter {
            reason: INVALID_BUNDLE_REASON.into(),
            description: truncate(&reason, MAX_DEAD_LETTER_DESCRIPTION),
        };
    }

    match client.send_bundle(body).await {
        Ok(()) => {
            metrics.message_sent();
            events.log_message_processed(
                &audit,
                Some("Message delivered to Promptly"),
                correlation,
            );
            tracing::info!(correlation_id = %values.correlation_id, "message delivered to Promptly");
            Disposition::Complete
        }
        Err(error @ DeliveryError::Permanent { .. }) => {
            tracing::error!(correlation_id = %values.correlation_id, %error, "Promptly rejected the message");
            events.log_message_failed(
                &audit,
                &error.to_string(),
                Some("Promptly rejected the message - dead-lettered"),
                correlation,
            );
            Disposition::DeadLetter {
                reason: REJECTED_REASON.into(),
                description: truncate(&error.to_string(), MAX_DEAD_LETTER_DESCRIPTION),
            }
        }
        Err(error @ DeliveryError::Authorization { .. }) => {
            tracing::error!(%error, "ALERT: Promptly credential/authorisation failure");
            events.log_message_failed(
                &audit,
                &error.to_string(),
                Some("ALERT: Promptly credential/authorisation failure"),
                correlation,
            );
            Disposition::Abandon
        }
        Err(error @ DeliveryError::Transient(_)) => {
            tracing::error!(correlation_id = %values.correlation_id, %error, "delivery to Promptly failed");
            events.log_message_failed(
                &audit,
                &error.to_string(),
                Some("Delivery to Promptly failed"),
                correlation,
            );
            Disposition::Abandon
        }
    }
}

/// Process a batch in order.
///
/// An [`Disposition::Abandon`] is the last entry; the tail is left
/// unattempted and abandoned by the caller. A dead letter does not stop the
/// batch.
pub async fn process_batch<T: Transport, C: Clock>(
    batch: &[Incoming],
    client: &mut FhirClient<T, C>,
    events: &EventLogger,
    metrics: &Metrics,
) -> Vec<Disposition> {
    let mut outcomes = Vec::with_capacity(batch.len());
    for message in batch {
        metrics.message_received();
        let outcome = process_one(message, client, events, metrics).await;
        let stop = outcome == Disposition::Abandon;
        outcomes.push(outcome);
        if stop {
            break;
        }
    }
    outcomes
}

/// At most `max` characters of `text`.
fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
#[allow(
    clippy::unnecessary_wraps,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::unused_async_trait_impl,
    reason = "scripted test doubles: the trait's shape, and a panic is a failed test"
)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use crate::client::Settings;
    use crate::client::test_support::{FakeClock, FakeTransport};
    use crate::http::{Response, TransportError};
    use ihb_bus::fake::FakeBus;
    use ihb_bus::message::Outgoing;
    use ihb_config::Secret;

    const BUNDLE: &str = r#"{"resourceType":"Bundle","type":"message","entry":[{"resource":{"name":"SECRET_PATIENT"}}]}"#;
    const OK: &str = r#"{"resourceType":"OperationOutcome","issue":[{"severity":"information","code":"informational"}]}"#;

    fn reply(status: u16, body: &str) -> Result<Response, TransportError> {
        Ok(Response {
            status,
            headers: vec![],
            body: body.as_bytes().to_vec(),
        })
    }

    fn client(
        script: Vec<Result<Response, TransportError>>,
    ) -> FhirClient<FakeTransport, FakeClock> {
        FhirClient::new(
            "https://p.example/x",
            Auth::api_key(Secret::new("k")).unwrap(),
            Settings::default(),
            FakeTransport::new(script),
            FakeClock::new(),
        )
        .unwrap()
    }

    fn inbound(body: impl Into<Vec<u8>>) -> Incoming {
        let bus = FakeBus::new();
        let props = [("CorrelationId", "corr-1"), ("WorkflowID", "wpas-to-proms")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        bus.send("in", Outgoing::new(body).with_properties(props))
            .unwrap();
        bus.receive("in", 1).remove(0)
    }

    async fn run(
        body: &str,
        script: Vec<Result<Response, TransportError>>,
    ) -> (Disposition, usize) {
        let mut client = client(script);
        let outcome = process_one(
            &inbound(body),
            &mut client,
            &EventLogger::new("w", "m"),
            &Metrics::new("w", "m", "h", "p"),
        )
        .await;
        (outcome, client.transport_requests())
    }

    #[tokio::test]
    async fn a_confirmed_delivery_completes() {
        // spec/10-senders.md §8.5.
        let (outcome, requests) = run(BUNDLE, vec![reply(200, OK)]).await;
        assert_eq!(outcome, Disposition::Complete);
        assert_eq!(requests, 1);
    }

    #[tokio::test]
    async fn a_body_that_is_not_a_message_bundle_is_dead_lettered_without_a_request() {
        for body in [
            "not json",
            r#"{"resourceType":"Patient"}"#,
            r#"{"resourceType":"Bundle","type":"batch"}"#,
        ] {
            let (outcome, requests) = run(body, vec![]).await;
            let Disposition::DeadLetter { reason, .. } = outcome else {
                panic!("expected a dead letter for {body}");
            };
            assert_eq!(reason, "InvalidFhirBundle");
            assert_eq!(requests, 0);
        }
    }

    #[tokio::test]
    async fn a_body_that_is_not_utf8_is_dead_lettered() {
        let mut client = client(vec![]);
        let outcome = process_one(
            &inbound(b"\xff\xfe".to_vec()),
            &mut client,
            &EventLogger::new("w", "m"),
            &Metrics::new("w", "m", "h", "p"),
        )
        .await;
        assert!(matches!(outcome, Disposition::DeadLetter { .. }));
    }

    #[tokio::test]
    async fn a_rejection_is_dead_lettered_with_a_bounded_description_that_omits_the_bundle() {
        let (outcome, _) = run(
            BUNDLE,
            vec![reply(400, r#"{"resourceType":"OperationOutcome","issue":[{"severity":"error","code":"invalid"}]}"#)],
        )
        .await;
        let Disposition::DeadLetter {
            reason,
            description,
        } = outcome
        else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "PromptlyRejected");
        assert!(
            description.starts_with("HTTP 400: error/invalid"),
            "{description}"
        );
        assert!(description.chars().count() <= MAX_DEAD_LETTER_DESCRIPTION);
        assert!(!description.contains("SECRET_PATIENT"));
    }

    #[tokio::test]
    async fn a_credential_failure_abandons() {
        assert_eq!(
            run(BUNDLE, vec![reply(403, "")]).await.0,
            Disposition::Abandon
        );
        assert_eq!(
            run(BUNDLE, vec![reply(401, "")]).await.0,
            Disposition::Abandon
        );
    }

    #[tokio::test]
    async fn exhausted_retries_abandon() {
        let script = vec![reply(503, ""), reply(503, ""), reply(503, "")];
        assert_eq!(run(BUNDLE, script).await.0, Disposition::Abandon);
    }

    #[tokio::test]
    async fn the_batch_stops_at_an_abandon_but_not_at_a_dead_letter() {
        let events = EventLogger::new("w", "m");
        let metrics = Metrics::new("w", "m", "h", "p");

        let mut rejecting = client(vec![reply(400, ""), reply(400, "")]);
        let batch = vec![inbound(BUNDLE), inbound(BUNDLE)];
        let out = process_batch(&batch, &mut rejecting, &events, &metrics).await;
        assert_eq!(out.len(), 2);

        let mut failing = client(vec![reply(403, "")]);
        let out = process_batch(&batch, &mut failing, &events, &metrics).await;
        assert_eq!(out, [Disposition::Abandon]);
    }

    #[test]
    fn the_audit_text_carries_the_size_and_nothing_of_the_bundle() {
        let text = audit_text(BUNDLE.len());
        assert!(!text.contains("SECRET_PATIENT"));
        assert!(text.contains(&BUNDLE.len().to_string()));
    }
}
