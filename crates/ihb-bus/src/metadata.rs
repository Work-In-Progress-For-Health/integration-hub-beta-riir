//! The properties that ride along with a message, hop after hop.
//!
//! Implements `spec/03-message-bus.md` §4.
//!
//! These keys are a contract between every component in the estate, and
//! their exact spelling matters — including `WorkflowID`, which capitalises
//! `ID` where everything else would write `Id`. That is not a typo to fix:
//! Log Analytics queries and the dashboard read these names.
//!
//! The one that earns its keep is `CorrelationId`. It is minted once, at
//! ingress, and carried unchanged to the far end, so "what happened to the
//! message the ward is asking about?" has an answer that survives retries,
//! transformers, and a queue in between.

use std::collections::BTreeMap;

/// The Hub correlation id: minted at ingress, never rewritten.
pub const CORRELATION_ID: &str = "CorrelationId";

/// When the message arrived at the Hub, RFC 3339, UTC.
pub const MESSAGE_RECEIVED_AT: &str = "MessageReceivedAt";

/// `MSH-3` of the received message.
pub const SOURCE_SYSTEM: &str = "SourceSystem";

/// The flow identifier.
///
/// Note the capitalisation: `WorkflowID`, not `WorkflowId`. Every other key
/// here uses `Id`; this one does not, and changing it would break queries.
pub const WORKFLOW_ID: &str = "WorkflowID";

/// What a missing value renders as in a log line.
///
/// From `metadata_utils.py :: NA`. It is a **log** convention only: it must
/// never reach an event's `correlation_id` field, or a search for it would
/// return every message that lacked one. See [`correlation_id_for_event`].
pub const NOT_APPLICABLE: &str = "N/A";

/// The application properties on a Service Bus message.
///
/// Ordered, so a rendering is stable and a test can assert on one.
pub type Properties = BTreeMap<String, String>;

/// The tracking metadata an ingress server mints for a message.
///
/// Built once, at the point a message enters the Hub, and forwarded
/// unchanged by every component after that
/// (`spec/01-architecture.md` §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracking {
    /// A fresh UUID v4.
    pub correlation_id: String,
    /// When the message arrived, per `spec/index.md` §0.7.
    pub received_at: String,
    /// `MSH-3`, or empty.
    pub source_system: String,
    /// `WORKFLOW_ID`.
    pub workflow_id: String,
}

impl Tracking {
    /// Render as application properties.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::metadata::Tracking;
    ///
    /// let tracking = Tracking {
    ///     correlation_id: "3f6d".into(),
    ///     received_at: "2026-08-20T09:15:00+00:00".into(),
    ///     source_system: "252".into(),
    ///     workflow_id: "phw-to-mpi".into(),
    /// };
    ///
    /// let properties = tracking.to_properties();
    /// assert_eq!(properties["CorrelationId"], "3f6d");
    /// assert_eq!(properties["WorkflowID"], "phw-to-mpi", "note the capital ID");
    /// ```
    #[must_use]
    pub fn to_properties(&self) -> Properties {
        let mut properties = Properties::new();
        properties.insert(CORRELATION_ID.to_string(), self.correlation_id.clone());
        properties.insert(MESSAGE_RECEIVED_AT.to_string(), self.received_at.clone());
        properties.insert(SOURCE_SYSTEM.to_string(), self.source_system.clone());
        properties.insert(WORKFLOW_ID.to_string(), self.workflow_id.clone());
        properties
    }
}

/// What a log line says about a message's metadata.
///
/// Every absent value renders as [`NOT_APPLICABLE`], which is what makes the
/// line readable at a glance — the fields stay in the same places whether or
/// not they are populated (`metadata_utils.py :: get_metadata_log_values`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogValues {
    /// `CorrelationId`, or `N/A`.
    pub correlation_id: String,
    /// `WorkflowID`, or `N/A`.
    pub workflow_id: String,
    /// `SourceSystem`, or `N/A`.
    pub source_system: String,
    /// `MessageReceivedAt`, or `N/A`.
    pub received_at: String,
}

impl LogValues {
    /// Read the four tracked values out of a message's properties.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::metadata::{LogValues, Properties};
    ///
    /// let mut properties = Properties::new();
    /// properties.insert("CorrelationId".into(), "3f6d".into());
    ///
    /// let values = LogValues::from_properties(&properties);
    /// assert_eq!(values.correlation_id, "3f6d");
    /// assert_eq!(values.workflow_id, "N/A", "an absent value is visible as N/A");
    /// ```
    #[must_use]
    pub fn from_properties(properties: &Properties) -> Self {
        let read = |key: &str| {
            properties
                .get(key)
                .map(String::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(NOT_APPLICABLE)
                .to_string()
        };

        Self {
            correlation_id: read(CORRELATION_ID),
            workflow_id: read(WORKFLOW_ID),
            source_system: read(SOURCE_SYSTEM),
            received_at: read(MESSAGE_RECEIVED_AT),
        }
    }
}

/// The correlation id to put on an audit event, or `None`.
///
/// `N/A` is a **log** rendering, not a value: an event whose `correlation_id`
/// is the literal string `N/A` would make every message that lacked one look
/// related. `correlation_id_for_logger` in the Python exists for exactly this
/// and so does this (`spec/03-message-bus.md` §4.3).
///
/// # Examples
///
/// ```
/// use ihb_bus::metadata::correlation_id_for_event;
///
/// assert_eq!(correlation_id_for_event("3f6d"), Some("3f6d"));
/// assert_eq!(correlation_id_for_event("N/A"), None);
/// assert_eq!(correlation_id_for_event("  "), None);
/// ```
#[must_use]
pub fn correlation_id_for_event(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == NOT_APPLICABLE {
        None
    } else {
        Some(trimmed)
    }
}

// `pipe_wrap` used to live here as well as in `ihb-fields`, byte for byte
// identical. Two copies of a rule whose specification says "MUST be
// reproduced exactly" (`spec/03-message-bus.md` §4.1) is two things to keep
// in step, and this copy had no callers outside its own tests. It is
// `ihb_fields::pipe_wrap` now, next to `authority_codes`, which is what
// produces the lists it wraps.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_property_keys_are_frozen() {
        // spec/03-message-bus.md §4 — a cross-service contract.
        assert_eq!(CORRELATION_ID, "CorrelationId");
        assert_eq!(MESSAGE_RECEIVED_AT, "MessageReceivedAt");
        assert_eq!(SOURCE_SYSTEM, "SourceSystem");
        assert_eq!(
            WORKFLOW_ID, "WorkflowID",
            "capital ID, unlike every other key; queries depend on it"
        );
    }

    #[test]
    fn tracking_renders_all_four_properties() {
        let tracking = Tracking {
            correlation_id: "3f6d".into(),
            received_at: "2026-08-20T09:15:00+00:00".into(),
            source_system: "252".into(),
            workflow_id: "phw-to-mpi".into(),
        };

        let properties = tracking.to_properties();
        assert_eq!(properties.len(), 4);
        assert_eq!(properties[CORRELATION_ID], "3f6d");
        assert_eq!(properties[MESSAGE_RECEIVED_AT], "2026-08-20T09:15:00+00:00");
        assert_eq!(properties[SOURCE_SYSTEM], "252");
        assert_eq!(properties[WORKFLOW_ID], "phw-to-mpi");
    }

    #[test]
    fn an_absent_source_system_is_empty_not_missing() {
        // spec/03-message-bus.md §4 — SourceSystem is "" when MSH-3 is
        // absent, so the property is present and the shape is constant.
        let tracking = Tracking {
            correlation_id: "3f6d".into(),
            received_at: "2026-08-20T09:15:00+00:00".into(),
            source_system: String::new(),
            workflow_id: "phw-to-mpi".into(),
        };
        assert_eq!(tracking.to_properties()[SOURCE_SYSTEM], "");
    }

    #[test]
    fn a_missing_value_reads_as_not_applicable_in_a_log() {
        // spec/03-message-bus.md §4.3
        let values = LogValues::from_properties(&Properties::new());
        assert_eq!(values.correlation_id, "N/A");
        assert_eq!(values.workflow_id, "N/A");
        assert_eq!(values.source_system, "N/A");
        assert_eq!(values.received_at, "N/A");
    }

    #[test]
    fn a_blank_value_reads_as_not_applicable_too() {
        let mut properties = Properties::new();
        properties.insert(CORRELATION_ID.to_string(), "   ".to_string());
        assert_eq!(
            LogValues::from_properties(&properties).correlation_id,
            "N/A"
        );
    }

    #[test]
    fn not_applicable_never_becomes_an_events_correlation_id() {
        // spec/03-message-bus.md §4.3 — the trap. Thousands of unrelated
        // events sharing the correlation id "N/A" would be worse than none.
        assert_eq!(correlation_id_for_event(NOT_APPLICABLE), None);
        assert_eq!(correlation_id_for_event(""), None);
        assert_eq!(correlation_id_for_event(" N/A "), None);
        assert_eq!(correlation_id_for_event(" 3f6d "), Some("3f6d"));
    }
}
