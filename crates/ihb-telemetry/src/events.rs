//! The audit events every Integration Hub component emits.
//!
//! Implements `spec/04-observability.md` §1.
//!
//! An event says: this component, in this flow, did this to this message, at
//! this time. Together they are the audit trail — how an operator answers
//! "what happened to the message the ward is asking about?" — so the names
//! and the shape are a contract with Log Analytics, the NOC dashboard, and
//! the alert rules, not an implementation detail.
//!
//! **Every payload passes through [`crate::redact`] on the way in**, unless
//! redaction is explicitly disabled. That is the single most important line
//! in this module.
//!
//! # Examples
//!
//! ```
//! use ihb_telemetry::events::EventLogger;
//!
//! let logger = EventLogger::new("phw-to-mpi", "phw_hl7_server");
//!
//! logger.log_message_received(
//!     "MSH|^~\\&|252|252|100|100|20260729095037||ADT^A28|MSG1|P|2.5\rPID|1||NHS123",
//!     Some("Message received for HL7 sending"),
//!     Some("3f6d-…"),
//! );
//! ```

use std::sync::Arc;

use crate::clock::{SharedClock, SystemClock};
use crate::redact::redact_hl7;

/// The message text every event carries.
///
/// Log Analytics queries match on this literal, so it is frozen
/// (`spec/04-observability.md` §1.2.1).
pub const EVENT_MESSAGE: &str = "Integration Hub Event";

/// What happened to a message.
///
/// The serialised spellings are the contract
/// (`spec/04-observability.md` §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    /// A message arrived at a component.
    MessageReceived,
    /// A component finished with a message successfully.
    MessageProcessed,
    /// Reserved. Emitted by no current component, kept because the Python
    /// defines it and a Log Analytics query may already reference it.
    MessageSent,
    /// A component could not process a message.
    MessageFailed,
    /// A validation step passed.
    ValidationSuccess,
    /// A validation step failed.
    ValidationFailed,
}

impl EventType {
    /// The wire spelling.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_telemetry::events::EventType;
    ///
    /// assert_eq!(EventType::MessageReceived.as_str(), "MESSAGE_RECEIVED");
    /// assert_eq!(EventType::ValidationFailed.as_str(), "VALIDATION_FAILED");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MessageReceived => "MESSAGE_RECEIVED",
            Self::MessageProcessed => "MESSAGE_PROCESSED",
            Self::MessageSent => "MESSAGE_SENT",
            Self::MessageFailed => "MESSAGE_FAILED",
            Self::ValidationSuccess => "VALIDATION_SUCCESS",
            Self::ValidationFailed => "VALIDATION_FAILED",
        }
    }

    /// The event type for a validation outcome.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_telemetry::events::EventType;
    ///
    /// assert_eq!(EventType::for_validation(true), EventType::ValidationSuccess);
    /// assert_eq!(EventType::for_validation(false), EventType::ValidationFailed);
    /// ```
    #[must_use]
    pub const fn for_validation(is_success: bool) -> Self {
        if is_success {
            Self::ValidationSuccess
        } else {
            Self::ValidationFailed
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One audit event, ready to emit.
///
/// Built by [`EventLogger`], which is also what redacts `message_content`.
/// Constructing one directly bypasses that, so do not: the only reason this
/// type is public is that tests and future exporters need to read one.
///
/// See `spec/04-observability.md` §1.2 for the field contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEvent {
    /// `WORKFLOW_ID`.
    pub workflow_id: String,
    /// `MICROSERVICE_ID`.
    pub microservice_id: String,
    /// What happened.
    pub event_type: EventType,
    /// When, per `spec/index.md` §0.7.
    pub timestamp: String,
    /// The message this event is about — **already redacted**.
    pub message_content: String,
    /// A human-readable outcome, where there is one.
    pub validation_result: Option<String>,
    /// Why it failed, on [`EventType::MessageFailed`].
    pub error_details: Option<String>,
    /// The Hub correlation id, when known.
    pub correlation_id: Option<String>,
}

/// Emits [`LogEvent`]s for one component.
///
/// Cheap to clone: a clone shares the identity and the clock, so a service
/// hands one to every task rather than threading a reference.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::events::EventLogger;
///
/// let logger = EventLogger::new("phw-to-mpi", "phw_hl7_server");
/// assert!(logger.redaction_enabled());
///
/// // Redaction can be turned off for local debugging, and only there.
/// let noisy = EventLogger::new("phw-to-mpi", "phw_hl7_server").with_redaction(false);
/// assert!(!noisy.redaction_enabled());
/// ```
#[derive(Clone)]
pub struct EventLogger {
    /// `WORKFLOW_ID`.
    workflow_id: Arc<str>,
    /// `MICROSERVICE_ID`.
    microservice_id: Arc<str>,
    /// Whether payloads are redacted. True unless deliberately disabled.
    redaction_enabled: bool,
    /// Where timestamps come from.
    clock: SharedClock,
}

impl std::fmt::Debug for EventLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLogger")
            .field("workflow_id", &self.workflow_id)
            .field("microservice_id", &self.microservice_id)
            .field("redaction_enabled", &self.redaction_enabled)
            .finish_non_exhaustive()
    }
}

impl EventLogger {
    /// A logger for one component, redacting, on the system clock.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_telemetry::events::EventLogger;
    ///
    /// let logger = EventLogger::new("chemocare-to-mpi", "chemo_hl7_transformer");
    /// assert_eq!(logger.workflow_id(), "chemocare-to-mpi");
    /// ```
    #[must_use]
    pub fn new(workflow_id: impl AsRef<str>, microservice_id: impl AsRef<str>) -> Self {
        Self {
            workflow_id: Arc::from(workflow_id.as_ref()),
            microservice_id: Arc::from(microservice_id.as_ref()),
            redaction_enabled: true,
            clock: Arc::new(SystemClock),
        }
    }

    /// Turn redaction off.
    ///
    /// Only for local debugging. `HL7_LOG_REDACTION_ENABLED=false` is the
    /// deployment-side switch, and it must never be false in an environment
    /// holding real patient data
    /// (`spec/02-configuration.md` §2).
    #[must_use]
    pub const fn with_redaction(mut self, enabled: bool) -> Self {
        self.redaction_enabled = enabled;
        self
    }

    /// Use a different clock, so a test can assert on a timestamp.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use ihb_telemetry::clock::FixedClock;
    /// use ihb_telemetry::events::{EventLogger, EventType};
    /// use time::macros::datetime;
    ///
    /// let logger = EventLogger::new("w", "m")
    ///     .with_clock(Arc::new(FixedClock::at(datetime!(2026-08-20 09:15:00 UTC))));
    ///
    /// let event = logger.build(EventType::MessageReceived, "MSH|^~\\&|A", None, None, None);
    /// assert_eq!(event.timestamp, "2026-08-20T09:15:00+00:00");
    /// ```
    #[must_use]
    pub fn with_clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// This component's flow.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    /// This component's identifier.
    #[must_use]
    pub fn microservice_id(&self) -> &str {
        &self.microservice_id
    }

    /// Whether payloads are redacted.
    #[must_use]
    pub const fn redaction_enabled(&self) -> bool {
        self.redaction_enabled
    }

    /// Build an event without emitting it.
    ///
    /// Public so a test can assert on the shape without capturing log
    /// output. The redaction of `message_content` happens **here**, so
    /// anything built through this is already safe.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_telemetry::events::{EventLogger, EventType};
    ///
    /// let logger = EventLogger::new("phw-to-mpi", "phw_hl7_server");
    /// let event = logger.build(
    ///     EventType::MessageReceived,
    ///     "MSH|^~\\&|252|252|100|100|20260729095037||ADT^A28|MSG1|P|2.5\rPID|1||SECRET",
    ///     None,
    ///     None,
    ///     None,
    /// );
    ///
    /// assert_eq!(event.event_type, EventType::MessageReceived);
    /// assert!(!event.message_content.contains("SECRET"));
    /// assert!(event.message_content.contains("MSG1"), "routing metadata survives");
    /// ```
    #[must_use]
    pub fn build(
        &self,
        event_type: EventType,
        message_content: &str,
        validation_result: Option<&str>,
        error_details: Option<&str>,
        correlation_id: Option<&str>,
    ) -> LogEvent {
        let content = if self.redaction_enabled {
            redact_hl7(message_content)
        } else {
            message_content.to_string()
        };

        LogEvent {
            workflow_id: self.workflow_id.to_string(),
            microservice_id: self.microservice_id.to_string(),
            event_type,
            timestamp: self.clock.now_iso8601(),
            message_content: content,
            validation_result: validation_result.map(str::to_string),
            error_details: error_details.map(str::to_string),
            correlation_id: normalise_correlation_id(correlation_id),
        }
    }

    /// A message arrived.
    pub fn log_message_received(
        &self,
        message_content: &str,
        validation_result: Option<&str>,
        correlation_id: Option<&str>,
    ) {
        Self::emit(&self.build(
            EventType::MessageReceived,
            message_content,
            validation_result,
            None,
            correlation_id,
        ));
    }

    /// A message was processed successfully.
    pub fn log_message_processed(
        &self,
        message_content: &str,
        validation_result: Option<&str>,
        correlation_id: Option<&str>,
    ) {
        Self::emit(&self.build(
            EventType::MessageProcessed,
            message_content,
            validation_result,
            None,
            correlation_id,
        ));
    }

    /// A message could not be processed.
    ///
    /// `error_details` is required here, unlike the other events: an event
    /// that says something failed without saying why is the one an operator
    /// will be looking at, and it should not be empty.
    pub fn log_message_failed(
        &self,
        message_content: &str,
        error_details: &str,
        validation_result: Option<&str>,
        correlation_id: Option<&str>,
    ) {
        Self::emit(&self.build(
            EventType::MessageFailed,
            message_content,
            validation_result,
            Some(error_details),
            correlation_id,
        ));
    }

    /// A validation step passed or failed.
    pub fn log_validation_result(
        &self,
        message_content: &str,
        validation_result: &str,
        is_success: bool,
        correlation_id: Option<&str>,
    ) {
        Self::emit(&self.build(
            EventType::for_validation(is_success),
            message_content,
            Some(validation_result),
            None,
            correlation_id,
        ));
    }

    /// Write an event to the log.
    ///
    /// One `tracing` event with structured fields, whose message text is
    /// [`EVENT_MESSAGE`]. The Python's §1.2.2 requirement — that a failure to
    /// emit is logged and re-raised — has no counterpart here: emitting is
    /// infallible in `tracing`, so there is nothing to catch and nothing a
    /// caller could do about it. See `spec/04-observability.md` §1.2.2a.
    ///
    /// `otel_trace_id` and `otel_span_id` are **not** set here. They are
    /// added to every record by the subscriber layer in
    /// `spec/04-observability.md` §4.5, which is the right level: an event
    /// should not have to know whether tracing is configured.
    fn emit(event: &LogEvent) {
        tracing::info!(
            workflow_id = %event.workflow_id,
            microservice_id = %event.microservice_id,
            event_type = event.event_type.as_str(),
            timestamp = %event.timestamp,
            message_content = %event.message_content,
            validation_result = event.validation_result.as_deref(),
            error_details = event.error_details.as_deref(),
            correlation_id = event.correlation_id.as_deref(),
            "{EVENT_MESSAGE}"
        );
    }
}

/// Drop a correlation id that is absent in disguise.
///
/// `metadata_utils.py` renders a missing value as the literal `N/A` for log
/// lines, and `correlation_id_for_logger` turns that back into `None` before
/// it reaches the event. Without this, a search for `N/A` correlation ids
/// would find thousands of unrelated messages.
/// See `spec/03-message-bus.md` §4.3.
fn normalise_correlation_id(correlation_id: Option<&str>) -> Option<String> {
    correlation_id
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "N/A")
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::clock::FixedClock;

    /// A message with a marker that must not survive into an event.
    const MESSAGE: &str =
        "MSH|^~\\&|252|252|100|100|20260729095037||ADT^A28|MSG1|P|2.5\rPID|1||NEEDLE";

    /// A logger with a clock that does not move.
    fn logger() -> EventLogger {
        EventLogger::new("phw-to-mpi", "phw_hl7_server")
            .with_clock(Arc::new(FixedClock::at(datetime!(2026-08-20 09:15:00 UTC))))
    }

    #[test]
    fn the_six_event_types_keep_their_spellings() {
        // spec/04-observability.md §1.1 — frozen; alert rules match on these.
        assert_eq!(EventType::MessageReceived.as_str(), "MESSAGE_RECEIVED");
        assert_eq!(EventType::MessageProcessed.as_str(), "MESSAGE_PROCESSED");
        assert_eq!(EventType::MessageSent.as_str(), "MESSAGE_SENT");
        assert_eq!(EventType::MessageFailed.as_str(), "MESSAGE_FAILED");
        assert_eq!(EventType::ValidationSuccess.as_str(), "VALIDATION_SUCCESS");
        assert_eq!(EventType::ValidationFailed.as_str(), "VALIDATION_FAILED");
    }

    #[test]
    fn the_event_message_text_is_frozen() {
        // spec/04-observability.md §1.2.1
        assert_eq!(EVENT_MESSAGE, "Integration Hub Event");
    }

    #[test]
    fn a_payload_is_redacted_on_the_way_into_an_event() {
        // spec/04-observability.md §1.2, and index.md §0.8.1.
        let event = logger().build(EventType::MessageReceived, MESSAGE, None, None, None);
        assert!(!event.message_content.contains("NEEDLE"));
        assert!(event.message_content.contains("MSG1"));
    }

    #[test]
    fn redaction_can_be_disabled_deliberately() {
        // spec/04-observability.md §2.1 — for local debugging only.
        let event = logger().with_redaction(false).build(
            EventType::MessageReceived,
            MESSAGE,
            None,
            None,
            None,
        );
        assert!(event.message_content.contains("NEEDLE"));
    }

    #[test]
    fn the_timestamp_comes_from_the_clock() {
        // spec/index.md §0.7, spec/17 §5.
        let event = logger().build(EventType::MessageReceived, MESSAGE, None, None, None);
        assert_eq!(event.timestamp, "2026-08-20T09:15:00+00:00");
    }

    #[test]
    fn identity_is_carried_on_every_event() {
        // spec/04-observability.md §1.2
        let event = logger().build(EventType::MessageProcessed, MESSAGE, None, None, None);
        assert_eq!(event.workflow_id, "phw-to-mpi");
        assert_eq!(event.microservice_id, "phw_hl7_server");
    }

    #[test]
    fn a_validation_outcome_picks_its_event_type() {
        // spec/04-observability.md §1.3
        let logger = logger();
        let ok = logger.build(EventType::for_validation(true), MESSAGE, None, None, None);
        let bad = logger.build(EventType::for_validation(false), MESSAGE, None, None, None);
        assert_eq!(ok.event_type, EventType::ValidationSuccess);
        assert_eq!(bad.event_type, EventType::ValidationFailed);
    }

    #[test]
    fn a_not_applicable_correlation_id_is_absent_rather_than_logged() {
        // spec/03-message-bus.md §4.3 — the trap that would fill Log
        // Analytics with events correlated to the string "N/A".
        assert_eq!(normalise_correlation_id(Some("N/A")), None);
        assert_eq!(normalise_correlation_id(Some("  ")), None);
        assert_eq!(normalise_correlation_id(None), None);
        assert_eq!(
            normalise_correlation_id(Some(" 3f6d ")),
            Some("3f6d".to_string())
        );
    }

    #[test]
    fn a_failure_event_carries_its_reason() {
        // spec/04-observability.md §1.2
        let event = logger().build(
            EventType::MessageFailed,
            MESSAGE,
            Some("XML schema validation failed"),
            Some("PID.7 is required for PHW."),
            Some("3f6d"),
        );
        assert_eq!(event.event_type, EventType::MessageFailed);
        assert_eq!(
            event.validation_result.as_deref(),
            Some("XML schema validation failed")
        );
        assert_eq!(
            event.error_details.as_deref(),
            Some("PID.7 is required for PHW.")
        );
        assert_eq!(event.correlation_id.as_deref(), Some("3f6d"));
    }

    #[test]
    fn an_unparseable_payload_reaches_an_event_fully_masked() {
        // spec/04-observability.md §2.4 — a JSON body, or a partial frame,
        // must not arrive in an event just because it is not ER7.
        let event = logger().build(
            EventType::MessageFailed,
            r#"{"nhsNumber": "NEEDLE"}"#,
            None,
            Some("Invalid HL7 message format"),
            None,
        );
        assert_eq!(event.message_content, "***");
    }

    #[test]
    fn a_logger_is_cheap_to_clone_and_keeps_its_identity() {
        let original = logger();
        let clone = original.clone();
        assert_eq!(clone.workflow_id(), original.workflow_id());
        assert_eq!(clone.microservice_id(), original.microservice_id());
    }

    #[test]
    fn the_debug_rendering_does_not_dump_a_clock() {
        let rendered = format!("{:?}", logger());
        assert!(rendered.contains("phw-to-mpi"));
        assert!(rendered.contains("redaction_enabled: true"));
    }

    #[test]
    fn every_emit_path_compiles_and_runs() {
        // Emission is infallible; this asserts the four public entry points
        // exist and can be called, which the type system alone would not.
        let logger = logger();
        logger.log_message_received(MESSAGE, Some("received"), Some("3f6d"));
        logger.log_message_processed(MESSAGE, Some("processed"), Some("3f6d"));
        logger.log_message_failed(MESSAGE, "boom", Some("failed"), None);
        logger.log_validation_result(MESSAGE, "valid", true, None);
        logger.log_validation_result(MESSAGE, "invalid", false, None);
    }
}
