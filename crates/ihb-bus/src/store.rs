//! The message-store protocol: the JSON every component sends so that every
//! message that crosses the Hub is kept.
//!
//! Implements `spec/03-message-bus.md` §6.
//!
//! The message-store queue carries JSON, not HL7, and the document below is
//! a contract between every producer and `message-store-service`. The keys
//! are `PascalCase` and are frozen; the consumer rejects a document missing
//! a required one, and the SQL columns are named after them.
//!
//! This is also the one place in the workspace where a full, unredacted
//! payload is deliberately stored — replay needs it, and the store is
//! access-controlled. See `spec/index.md` §0.8.2.

use serde::{Deserialize, Serialize};

/// One message, on its way to be persisted.
///
/// Serialised with `PascalCase` keys exactly as
/// `spec/03-message-bus.md` §6 lists them.
///
/// # Examples
///
/// ```
/// use ihb_bus::store::StoreEvent;
///
/// let event = StoreEvent {
///     message_received_at: "2026-08-20T09:15:00+00:00".into(),
///     correlation_id: "3f6d".into(),
///     source_system: "252".into(),
///     processing_component: "phw_hl7_server".into(),
///     target_system: Some("MPI".into()),
///     raw_payload: "MSH|^~\\&|252".into(),
///     xml_payload: None,
///     session_id: "phw-to-mpi".into(),
/// };
///
/// let json = event.to_json()?;
/// assert!(json.contains(r#""CorrelationId":"3f6d""#));
/// assert!(json.contains(r#""XmlPayload":null"#), "nullable, not omitted");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct StoreEvent {
    /// When the message arrived at the Hub.
    pub message_received_at: String,
    /// The Hub correlation id.
    pub correlation_id: String,
    /// `MSH-3` of the original message.
    pub source_system: String,
    /// The producing component's `MICROSERVICE_ID`.
    pub processing_component: String,
    /// Where it is going. Defaults to the producer's `PEER_SERVICE`.
    pub target_system: Option<String>,
    /// The message, in full and unredacted.
    pub raw_payload: String,
    /// The v2.xml rendering, when one could be produced.
    pub xml_payload: Option<String>,
    /// The producing component's Service Bus session, so replay can restore
    /// the message onto the right one.
    pub session_id: String,
}

impl StoreEvent {
    /// Render as the JSON the store service reads.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] only if serialisation fails, which for this
    /// shape means an allocation failure.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Read one back.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the document is not valid JSON or is
    /// missing a required key.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> StoreEvent {
        StoreEvent {
            message_received_at: "2026-08-20T09:15:00.123456+00:00".into(),
            correlation_id: "3f6d".into(),
            source_system: "252".into(),
            processing_component: "phw_hl7_server".into(),
            target_system: Some("MPI".into()),
            raw_payload: "MSH|^~\\&|252\rPID|1||X".into(),
            xml_payload: Some("<ADT_A05/>".into()),
            session_id: "phw-to-mpi".into(),
        }
    }

    #[test]
    fn the_keys_are_pascal_case_exactly_as_specified() {
        // spec/03-message-bus.md §6.1 — a contract with every producer and
        // with the SQL columns.
        let json = event().to_json().unwrap();
        for key in [
            "MessageReceivedAt",
            "CorrelationId",
            "SourceSystem",
            "ProcessingComponent",
            "TargetSystem",
            "RawPayload",
            "XmlPayload",
            "SessionId",
        ] {
            assert!(
                json.contains(&format!("\"{key}\"")),
                "missing {key}: {json}"
            );
        }
    }

    #[test]
    fn a_document_round_trips() {
        let original = event();
        let json = original.to_json().unwrap();
        assert_eq!(StoreEvent::from_json(&json).unwrap(), original);
    }

    #[test]
    fn the_nullable_keys_are_written_as_null_not_omitted() {
        // spec/03-message-bus.md §6.1 — the consumer distinguishes "absent"
        // from "null", and a SQL NULL is what it writes for both. Emitting
        // the key keeps the document shape constant.
        let mut event = event();
        event.target_system = None;
        event.xml_payload = None;

        let json = event.to_json().unwrap();
        assert!(json.contains(r#""TargetSystem":null"#), "{json}");
        assert!(json.contains(r#""XmlPayload":null"#), "{json}");
    }

    #[test]
    fn a_document_missing_a_required_key_is_rejected() {
        // spec/12-message-store.md §3.3
        let json = r#"{"MessageReceivedAt":"t","CorrelationId":"c"}"#;
        assert!(StoreEvent::from_json(json).is_err());
    }

    #[test]
    fn a_document_missing_only_a_nullable_key_is_accepted() {
        let json = r#"{
            "MessageReceivedAt":"t","CorrelationId":"c","SourceSystem":"s",
            "ProcessingComponent":"p","RawPayload":"r","SessionId":"x"
        }"#;
        let event = StoreEvent::from_json(json).unwrap();
        assert_eq!(event.target_system, None);
        assert_eq!(event.xml_payload, None);
    }

    #[test]
    fn a_payload_with_hl7_separators_survives_the_round_trip() {
        // The payload is JSON-encoded, and HL7 is full of backslashes.
        let mut event = event();
        event.raw_payload = "MSH|^~\\&|252\rPID|1||A\\R\\B\rNTE|\"quoted\"".into();

        let json = event.to_json().unwrap();
        let back = StoreEvent::from_json(&json).unwrap();
        assert_eq!(back.raw_payload, event.raw_payload);
    }

    #[test]
    fn the_payload_is_stored_unredacted() {
        // spec/index.md §0.8.2 — deliberate, and the one place it is true.
        // Replay cannot work from a redacted message.
        let event = event();
        let json = event.to_json().unwrap();
        assert!(json.contains("PID|1||X"), "the payload was altered");
    }
}
