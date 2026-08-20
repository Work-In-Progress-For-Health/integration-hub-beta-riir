//! What an event actually looks like once it has been emitted.
//!
//! Implements `spec/17-testing-and-quality.md` §6.2 for
//! `spec/04-observability.md` §1.
//!
//! The unit tests in `events.rs` assert on the [`LogEvent`] a logger builds.
//! That is not quite the thing that matters: what reaches Azure Monitor is
//! the `tracing` record, and a field dropped between the two would pass
//! every unit test and break every Log Analytics query. This drives a real
//! subscriber and reads the record back.
//!
//! It is also where the patient-safety claim is checked end to end: a
//! payload goes in one side, and the marker inside it is asserted absent
//! from everything that came out the other.

// An integration test is its own crate, so the crate-level allow in
// `src/lib.rs` does not reach it. Asserting is what this file is for.
// See spec/17-testing-and-quality.md §2.3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ihb_telemetry::clock::FixedClock;
use ihb_telemetry::events::{EVENT_MESSAGE, EventLogger};
use time::macros::datetime;
use tracing::field::{Field, Visit};
use tracing::subscriber::with_default;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;

/// One captured `tracing` record.
#[derive(Debug, Clone)]
struct Record {
    /// The record's message text, from its `message` field.
    message: String,
    /// Every other field, by name.
    fields: BTreeMap<String, String>,
}

impl Record {
    /// A field's value, or `None` if the field was not recorded at all.
    fn get(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// Everything in this record as one string, for absence assertions.
    fn flattened(&self) -> String {
        let mut all = self.message.clone();
        for (name, value) in &self.fields {
            all.push('\u{1}');
            all.push_str(name);
            all.push('=');
            all.push_str(value);
        }
        all
    }
}

/// Collects every field of a record, by name.
#[derive(Default)]
struct Collector {
    /// Fields seen so far.
    fields: BTreeMap<String, String>,
}

impl Visit for Collector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }
}

/// A `tracing` layer that keeps every record it sees.
#[derive(Clone, Default)]
struct Capture {
    /// The records, in the order they were emitted.
    records: Arc<Mutex<Vec<Record>>>,
}

impl<S: tracing::Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut collector = Collector::default();
        event.record(&mut collector);

        // `tracing` records the macro's message text under `message`.
        let message = collector
            .fields
            .remove("message")
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        self.records.lock().unwrap().push(Record {
            message,
            fields: collector.fields,
        });
    }
}

impl Capture {
    /// Everything captured so far.
    fn records(&self) -> Vec<Record> {
        self.records.lock().unwrap().clone()
    }
}

/// A payload carrying a marker that must never reach a log.
const MESSAGE: &str = concat!(
    "MSH|^~\\&|252|252|100|100|20260729095037|SECRET|ADT^A28^ADT_A05|MSG1|P|2.5\r",
    "PID|1||NEEDLE0612^^^328^PI||NEEDLESURNAME^NEEDLEFORENAME||20010909|M"
);

/// Run `body` with a capturing subscriber installed, and return what it saw.
fn captured(body: impl FnOnce(&EventLogger)) -> Vec<Record> {
    let capture = Capture::default();
    let subscriber = Registry::default().with(capture.clone());

    let logger = EventLogger::new("phw-to-mpi", "phw_hl7_server")
        .with_clock(Arc::new(FixedClock::at(datetime!(2026-08-20 09:15:00 UTC))));

    with_default(subscriber, || body(&logger));
    capture.records()
}

#[test]
fn an_emitted_event_carries_every_field_the_contract_names() {
    // spec/04-observability.md §1.2
    let records = captured(|logger| {
        logger.log_message_received(MESSAGE, Some("Message received"), Some("3f6d"));
    });

    assert_eq!(records.len(), 1);
    let record = &records[0];

    assert_eq!(record.message, EVENT_MESSAGE);
    assert_eq!(record.get("workflow_id"), Some("phw-to-mpi"));
    assert_eq!(record.get("microservice_id"), Some("phw_hl7_server"));
    assert_eq!(record.get("event_type"), Some("MESSAGE_RECEIVED"));
    assert_eq!(record.get("timestamp"), Some("2026-08-20T09:15:00+00:00"));
    assert_eq!(record.get("validation_result"), Some("Message received"));
    assert_eq!(record.get("correlation_id"), Some("3f6d"));
    assert!(record.get("message_content").is_some());
}

#[test]
fn no_marker_from_the_payload_reaches_the_emitted_record() {
    // spec/index.md §0.8.1, spec/17-testing-and-quality.md §6.2.
    //
    // The whole record, not just message_content: a field added later that
    // happens to carry the payload would be caught here.
    let records = captured(|logger| {
        logger.log_message_received(MESSAGE, None, None);
        logger.log_message_processed(MESSAGE, None, None);
        logger.log_message_failed(MESSAGE, "boom", None, None);
        logger.log_validation_result(MESSAGE, "valid", true, None);
        logger.log_validation_result(MESSAGE, "invalid", false, None);
    });

    assert_eq!(records.len(), 5);
    for record in &records {
        let all = record.flattened();
        for marker in ["NEEDLE", "20010909", "SECRET"] {
            assert!(
                !all.contains(marker),
                "a {} event leaked {marker}:\n{all}",
                record.get("event_type").unwrap_or("?")
            );
        }
    }
}

#[test]
fn an_absent_optional_field_is_not_emitted_as_none() {
    // A field recorded as the literal "None" is worse than an absent one:
    // it looks like a value in a query. `tracing` skips a `None`, and this
    // asserts we are relying on that correctly.
    let records = captured(|logger| {
        logger.log_message_received(MESSAGE, None, None);
    });

    let record = &records[0];
    assert_eq!(record.get("validation_result"), None);
    assert_eq!(record.get("error_details"), None);
    assert_eq!(record.get("correlation_id"), None);
    assert!(!record.flattened().contains("None"));
}

#[test]
fn a_failure_event_carries_its_reason_to_the_log() {
    // spec/04-observability.md §1.2
    let records = captured(|logger| {
        logger.log_message_failed(
            MESSAGE,
            "PID.7 (Date of birth) is required for PHW.",
            Some("XML schema validation failed"),
            Some("3f6d"),
        );
    });

    let record = &records[0];
    assert_eq!(record.get("event_type"), Some("MESSAGE_FAILED"));
    assert_eq!(
        record.get("error_details"),
        Some("PID.7 (Date of birth) is required for PHW.")
    );
    assert_eq!(
        record.get("validation_result"),
        Some("XML schema validation failed")
    );
}

#[test]
fn a_not_applicable_correlation_id_never_reaches_the_log() {
    // spec/03-message-bus.md §4.3 — the literal "N/A" must not become a
    // correlation id thousands of unrelated events share.
    let records = captured(|logger| {
        logger.log_message_received(MESSAGE, None, Some("N/A"));
    });

    assert_eq!(records[0].get("correlation_id"), None);
}

#[test]
fn the_event_types_reach_the_log_with_their_wire_spellings() {
    // spec/04-observability.md §1.1 — alert rules match on these strings.
    let records = captured(|logger| {
        logger.log_message_received(MESSAGE, None, None);
        logger.log_message_processed(MESSAGE, None, None);
        logger.log_message_failed(MESSAGE, "boom", None, None);
        logger.log_validation_result(MESSAGE, "ok", true, None);
        logger.log_validation_result(MESSAGE, "no", false, None);
    });

    let types: Vec<&str> = records
        .iter()
        .map(|r| r.get("event_type").unwrap_or("?"))
        .collect();

    assert_eq!(
        types,
        [
            "MESSAGE_RECEIVED",
            "MESSAGE_PROCESSED",
            "MESSAGE_FAILED",
            "VALIDATION_SUCCESS",
            "VALIDATION_FAILED",
        ]
    );
}
