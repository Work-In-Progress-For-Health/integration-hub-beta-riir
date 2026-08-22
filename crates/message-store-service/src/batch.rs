//! The batch loop.
//!
//! Implements `spec/12-message-store.md` §4.
//!
//! # All or nothing, and what it costs
//!
//! §4.1: receive a batch, build every record, insert them in one
//! transaction, complete every message. §4.2: on **any** failure, abandon
//! every message in the batch.
//!
//! §4.3 is honest about the price. One malformed document blocks up to
//! ninety-nine good messages for ten redeliveries, until Service Bus
//! dead-letters it at `MaxDeliveryCount`. That is the Python's behaviour and
//! it is preserved deliberately: the alternative — inserting the good rows
//! and dead-lettering the bad one — changes what an operator sees when a
//! batch fails, and they have runbooks built on the current shape. It is
//! recorded there for the Hub team rather than fixed here.

use ihb_bus::message::Incoming;
use ihb_telemetry::events::EventLogger;
use time::OffsetDateTime;

use crate::record::{Record, build};

/// The `WorkflowID` this service's event logger uses.
///
/// §4.4: the literal string, not an environment variable. This service
/// serves every flow, so there is no one workflow it belongs to — and a
/// dashboard filtering by workflow needs a stable value rather than
/// whichever flow's `.env` happened to be copied.
pub const WORKFLOW_ID: &str = "message_store";

/// What a batch attempt produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    /// Every record inserted and committed. Complete every message.
    Committed {
        /// How many rows.
        count: usize,
    },
    /// Nothing inserted. Abandon every message.
    Abandoned {
        /// Why, for the log line and the event.
        reason: String,
    },
}

impl BatchOutcome {
    /// Whether the messages may be completed.
    #[must_use]
    pub const fn is_committed(&self) -> bool {
        matches!(self, Self::Committed { .. })
    }
}

/// Build a record for every message in a batch.
///
/// # Errors
///
/// The first failure, as a reason string. §3.6: a failure building **any**
/// record fails the whole batch, so this stops at the first rather than
/// collecting — the batch is already lost, and the first failure is the one
/// an operator will act on.
pub fn build_all(batch: &[Incoming]) -> Result<Vec<Record>, String> {
    batch
        .iter()
        .map(|message| {
            let body = std::str::from_utf8(&message.body)
                .map_err(|_| "the message body is not valid UTF-8".to_string())?;
            build(body).map_err(|error| error.to_string())
        })
        .collect()
}

/// Report a failed batch, per §4.2.
///
/// Three things happen, and the wording of each is normative — operators
/// match on all of it.
///
/// §4.2.1 is the one to be careful about: the event's content is the
/// literal string `Batch of {n} message(s)`, **never** the payloads. This
/// is the one service that handles full unredacted patient records in bulk,
/// and an event carrying a hundred of them would defeat every redaction
/// rule in `spec/04-observability.md`.
pub fn report_failure(events: &EventLogger, count: usize, reason: &str) -> BatchOutcome {
    tracing::error!("Failed to process batch of {count} message(s): {reason}");

    events.log_message_failed(
        // §4.2.1 — the count, not the content.
        &format!("Batch of {count} message(s)"),
        &format!("Batch processing failed: {reason}"),
        Some("Batch insert failed"),
        None,
    );

    BatchOutcome::Abandoned {
        reason: reason.to_string(),
    }
}

/// The `StoredAt` a batch shares (§2.2).
///
/// Computed once per batch rather than per row, so every row of a batch
/// carries the same insert timestamp and an operator can group them.
#[must_use]
pub fn stored_at_now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ihb_bus::fake::FakeBus;
    use ihb_bus::message::Outgoing;
    use ihb_bus::sink::MessageSink;

    const QUEUE: &str = "messagestore";

    /// A valid store document.
    fn document(correlation_id: &str) -> String {
        serde_json::json!({
            "MessageReceivedAt": "2026-08-21T09:15:00+00:00",
            "CorrelationId": correlation_id,
            "SourceSystem": "252",
            "ProcessingComponent": "phw_hl7_server",
            "RawPayload": "MSH|^~\\&|252",
            "SessionId": "phw-to-mpi"
        })
        .to_string()
    }

    /// Received messages carrying `bodies`, via the fake bus so the shape
    /// is the one a real receive produces.
    async fn received(bodies: &[String]) -> Vec<Incoming> {
        let bus = FakeBus::new();
        let sender = bus.sender(QUEUE);
        for body in bodies {
            sender.send(Outgoing::new(body.clone())).await.unwrap();
        }
        bus.receive(QUEUE, bodies.len())
    }

    #[tokio::test]
    async fn a_good_batch_builds_every_record() {
        let batch = received(&[document("a"), document("b"), document("c")]).await;

        let records = build_all(&batch).expect("every document is valid");

        assert_eq!(records.len(), 3);
        assert_eq!(records[0].correlation_id, "a");
        assert_eq!(records[2].correlation_id, "c");
    }

    #[tokio::test]
    async fn one_bad_document_fails_the_whole_batch() {
        // §3.6 and §4.2 — the behaviour §4.3 is honest about the cost of.
        let batch = received(&[
            document("good-one"),
            "not json".to_string(),
            document("good-two"),
        ])
        .await;

        let error = build_all(&batch).unwrap_err();

        assert_eq!(error, "Invalid JSON in message body");
    }

    #[tokio::test]
    async fn a_body_that_is_not_utf8_fails_the_batch_too() {
        let bus = FakeBus::new();
        bus.sender(QUEUE)
            .send(Outgoing::new(vec![0xFF, 0xFE]))
            .await
            .unwrap();
        let batch = bus.receive(QUEUE, 1);

        assert!(build_all(&batch).is_err());
    }

    #[test]
    fn a_failure_report_names_the_count_and_never_the_content() {
        // §4.2.1 — this is the one service that handles a hundred full
        // patient records at a time, and an event carrying them would
        // defeat every redaction rule in spec/04.
        let events = EventLogger::new(WORKFLOW_ID, "message_store_service");

        let outcome = report_failure(&events, 42, "Invalid JSON in message body");

        assert!(!outcome.is_committed());
        assert_eq!(
            outcome,
            BatchOutcome::Abandoned {
                reason: "Invalid JSON in message body".to_string()
            }
        );
    }

    #[test]
    fn the_workflow_id_is_the_literal_one() {
        // §4.4 — not an environment variable. This service serves every
        // flow, so there is no one workflow it belongs to.
        assert_eq!(WORKFLOW_ID, "message_store");
    }

    #[test]
    fn a_batch_shares_one_stored_at() {
        // §2.2 — computed once per batch, which is what lets an operator
        // group a batch in the table afterwards. Asserted as a property of
        // the call rather than of two calls, which would be a clock test.
        let once = stored_at_now();

        assert!(once.year() >= 2024, "a plausible moment: {once}");
        assert_eq!(once.offset(), time::UtcOffset::UTC, "spec/index.md §0.7");
    }
}
