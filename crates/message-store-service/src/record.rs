//! Turning a queue message into a row.
//!
//! Implements `spec/12-message-store.md` §3.
//!
//! # What may appear in an error here
//!
//! Almost nothing. `spec/index.md` §0.8 clause 3 says an error carries a
//! structural location and a reason, never a value — and every value in
//! this document is patient data or points at it.
//!
//! §3.5 makes **one** exception: an unparseable `MessageReceivedAt` is
//! reported with the offending value, because a timestamp is not patient
//! data and an operator cannot fix the sender without seeing what it sent.
//! That exception is written into the specification rather than left to
//! judgement, and nothing else in this module follows it.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// One row of `monitoring.Message`.
///
/// The column order is §2.1's, and the insert depends on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// `ReceivedAt`, from the event's `MessageReceivedAt`.
    pub received_at: OffsetDateTime,
    /// `CorrelationId`.
    pub correlation_id: String,
    /// `SourceSystem`.
    pub source_system: String,
    /// `ProcessingComponent`.
    pub processing_component: String,
    /// `TargetSystem`, nullable.
    pub target_system: Option<String>,
    /// `RawPayload`, in full and unredacted (§2.3).
    pub raw_payload: String,
    /// `XmlPayload`, nullable.
    pub xml_payload: Option<String>,
    /// `SessionId`.
    pub session_id: String,
}

/// A document that could not be turned into a row.
///
/// Every variant fails the **whole batch** (§3.6), which is why the
/// correlation id travels with the error: it is the only handle an operator
/// has on which message in a batch of a hundred was the problem.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// §3.2.
    #[error("Invalid JSON in message body")]
    InvalidJson,

    /// §3.3.
    #[error("Missing required field: {key}")]
    MissingField {
        /// Which key.
        key: &'static str,
        /// The document's correlation id, or `UNKNOWN`.
        correlation_id: String,
    },

    /// §3.5 — and the one place a value from the document appears.
    #[error("Invalid received_at timestamp: {value:?}")]
    InvalidTimestamp {
        /// The offending timestamp. A timestamp, not patient data.
        value: String,
    },
}

/// When a document carries no correlation id to report (§3.3).
pub const UNKNOWN_CORRELATION_ID: &str = "UNKNOWN";

/// The keys §3.3 requires.
pub const REQUIRED: [&str; 6] = [
    "MessageReceivedAt",
    "CorrelationId",
    "SourceSystem",
    "ProcessingComponent",
    "RawPayload",
    "SessionId",
];

/// Build a row from a queue message body.
///
/// # Errors
///
/// [`RecordError`], per §3.2, §3.3 and §3.5.
///
/// # Examples
///
/// ```
/// use message_store_service::record::build;
///
/// let document = r#"{
///     "MessageReceivedAt": "2026-08-21T09:15:00+00:00",
///     "CorrelationId": "3f6d",
///     "SourceSystem": "252",
///     "ProcessingComponent": "phw_hl7_server",
///     "TargetSystem": "MPI",
///     "RawPayload": "MSH|^~\\&|252",
///     "XmlPayload": "<ADT_A05/>",
///     "SessionId": "phw-to-mpi"
/// }"#;
///
/// let record = build(document)?;
///
/// assert_eq!(record.correlation_id, "3f6d");
/// assert_eq!(record.target_system.as_deref(), Some("MPI"));
/// # Ok::<(), message_store_service::record::RecordError>(())
/// ```
///
/// The optional columns become nulls when absent (§3.4):
///
/// ```
/// use message_store_service::record::build;
///
/// let document = r#"{
///     "MessageReceivedAt": "2026-08-21T09:15:00+00:00",
///     "CorrelationId": "3f6d",
///     "SourceSystem": "252",
///     "ProcessingComponent": "phw_hl7_server",
///     "RawPayload": "MSH|^~\\&|252",
///     "SessionId": "phw-to-mpi"
/// }"#;
///
/// let record = build(document)?;
///
/// assert_eq!(record.target_system, None);
/// assert_eq!(record.xml_payload, None);
/// # Ok::<(), message_store_service::record::RecordError>(())
/// ```
pub fn build(body: &str) -> Result<Record, RecordError> {
    let document: serde_json::Value =
        serde_json::from_str(body).map_err(|_| RecordError::InvalidJson)?;

    // Read first, so a missing-key error can name the correlation id even
    // when the correlation id itself is what is missing.
    let reported =
        string(&document, "CorrelationId").unwrap_or_else(|| UNKNOWN_CORRELATION_ID.to_string());

    let require = |key: &'static str| -> Result<String, RecordError> {
        string(&document, key).ok_or_else(|| RecordError::MissingField {
            key,
            correlation_id: reported.clone(),
        })
    };

    let received_at_text = require("MessageReceivedAt")?;
    let correlation_id = require("CorrelationId")?;
    let source_system = require("SourceSystem")?;
    let processing_component = require("ProcessingComponent")?;
    let raw_payload = require("RawPayload")?;
    let session_id = require("SessionId")?;

    // §3.5 — ISO-8601. `spec/index.md` §0.7 pins the exact shape the
    // producers write (`+00:00`, six-digit fraction or none), and RFC 3339
    // is the profile of ISO-8601 that describes it.
    let received_at = OffsetDateTime::parse(&received_at_text, &Rfc3339).map_err(|_| {
        RecordError::InvalidTimestamp {
            value: received_at_text.clone(),
        }
    })?;

    Ok(Record {
        received_at,
        correlation_id,
        source_system,
        processing_component,
        // §3.4 — absent becomes a SQL null. A JSON `null` is treated the
        // same way, because a producer that wrote one meant "no value".
        target_system: string(&document, "TargetSystem"),
        raw_payload,
        xml_payload: string(&document, "XmlPayload"),
        session_id,
    })
}

/// A string field, or `None` when absent or JSON `null`.
fn string(document: &serde_json::Value, key: &str) -> Option<String> {
    document.get(key)?.as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete document, which each test then breaks in one way.
    fn document() -> serde_json::Value {
        serde_json::json!({
            "MessageReceivedAt": "2026-08-21T09:15:00+00:00",
            "CorrelationId": "3f6d",
            "SourceSystem": "252",
            "ProcessingComponent": "phw_hl7_server",
            "TargetSystem": "MPI",
            "RawPayload": "MSH|^~\\&|252|252|100|100|20260821||ADT^A31|M1|P|2.5",
            "XmlPayload": "<ADT_A05 xmlns=\"urn:hl7-org:v2xml\"/>",
            "SessionId": "phw-to-mpi"
        })
    }

    #[test]
    fn a_complete_document_becomes_a_row() {
        // spec/12-message-store.md §3 and §6.1.
        let record = build(&document().to_string()).unwrap();

        assert_eq!(record.correlation_id, "3f6d");
        assert_eq!(record.source_system, "252");
        assert_eq!(record.processing_component, "phw_hl7_server");
        assert_eq!(record.target_system.as_deref(), Some("MPI"));
        assert_eq!(record.session_id, "phw-to-mpi");
        assert!(record.raw_payload.starts_with("MSH|^~\\&|252"));
        assert!(record.xml_payload.is_some());
        assert_eq!(record.received_at.year(), 2026);
    }

    #[test]
    fn every_required_key_is_required() {
        // §3.3 and §6.1 — each one, by name.
        for key in REQUIRED {
            let mut document = document();
            document.as_object_mut().unwrap().remove(key);

            let error = build(&document.to_string()).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("Missing required field: {key}"),
                "removing {key}"
            );
        }
    }

    #[test]
    fn a_missing_key_reports_the_correlation_id() {
        // §3.3 — the only handle an operator has on which message in a
        // batch of a hundred was the problem.
        let mut document = document();
        document.as_object_mut().unwrap().remove("SourceSystem");

        let error = build(&document.to_string()).unwrap_err();
        let RecordError::MissingField { correlation_id, .. } = &error else {
            panic!("expected a missing field")
        };
        assert_eq!(correlation_id, "3f6d");
    }

    #[test]
    fn a_document_with_no_correlation_id_reports_unknown() {
        // §3.3 — "logged with the correlation id or `UNKNOWN`". The case
        // where the missing key *is* the correlation id.
        let mut document = document();
        document.as_object_mut().unwrap().remove("CorrelationId");

        let error = build(&document.to_string()).unwrap_err();
        let RecordError::MissingField { correlation_id, .. } = &error else {
            panic!("expected a missing field")
        };
        assert_eq!(correlation_id, UNKNOWN_CORRELATION_ID);
    }

    #[test]
    fn invalid_json_is_its_own_error() {
        // §3.2 and §6.1.
        for body in ["", "not json", "{", "{\"a\":", "}{"] {
            assert_eq!(
                build(body).unwrap_err(),
                RecordError::InvalidJson,
                "{body:?}"
            );
        }
    }

    #[test]
    fn valid_json_that_is_not_an_object_fails_on_the_missing_field() {
        // `[]` and `null` parse — they are valid JSON — and then have no
        // keys, so §3.3 catches them rather than §3.2. That is also what
        // the Python does: `json.loads` succeeds and the subscript raises
        // `KeyError`. Worth a test, because "not valid JSON" reads as
        // though it should cover them and does not.
        for body in ["[]", "null", "42", "\"a string\""] {
            let error = build(body).unwrap_err();
            assert!(
                matches!(error, RecordError::MissingField { .. }),
                "{body:?} gave {error:?}"
            );
        }
    }

    #[test]
    fn an_unparseable_timestamp_names_the_value() {
        // §3.5 and §6.1 — the one place a value from the document appears
        // in an error, and it is permitted because a timestamp is not
        // patient data.
        let mut document = document();
        document.as_object_mut().unwrap()["MessageReceivedAt"] =
            serde_json::json!("not a timestamp");

        let error = build(&document.to_string()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid received_at timestamp: \"not a timestamp\""
        );
    }

    #[test]
    fn the_exact_timestamp_shapes_the_producers_write() {
        // `spec/index.md` §0.7 — the fraction is present with six digits or
        // omitted entirely, and the offset is `+00:00` rather than `Z`.
        // Both forms cross the queue, so both must parse.
        for value in [
            "2026-08-21T09:15:00+00:00",
            "2026-08-21T16:09:01.991442+00:00",
            "2026-08-21T09:15:00Z",
        ] {
            let mut document = document();
            document.as_object_mut().unwrap()["MessageReceivedAt"] = serde_json::json!(value);

            assert!(build(&document.to_string()).is_ok(), "{value}");
        }
    }

    #[test]
    fn the_optional_columns_become_nulls() {
        // §3.4 and §6.2.
        let mut document = document();
        document.as_object_mut().unwrap().remove("TargetSystem");
        document.as_object_mut().unwrap().remove("XmlPayload");

        let record = build(&document.to_string()).unwrap();

        assert_eq!(record.target_system, None);
        assert_eq!(record.xml_payload, None);
    }

    #[test]
    fn an_explicit_json_null_is_also_a_sql_null() {
        // A producer that wrote `"XmlPayload": null` meant "no value", and
        // `hl7-server` does exactly that when §3.4 of spec/07 could not
        // render one.
        let mut document = document();
        document.as_object_mut().unwrap()["XmlPayload"] = serde_json::Value::Null;
        document.as_object_mut().unwrap()["TargetSystem"] = serde_json::Value::Null;

        let record = build(&document.to_string()).unwrap();

        assert_eq!(record.xml_payload, None);
        assert_eq!(record.target_system, None);
    }

    #[test]
    fn no_error_carries_a_payload() {
        // `spec/index.md` §0.8 clause 4 — a known marker through every
        // failure path, asserting it does not surface.
        const MARKER: &str = "8888888-A-PATIENT-IDENTIFIER";

        let mut missing = document();
        missing.as_object_mut().unwrap()["RawPayload"] = serde_json::json!(MARKER);
        missing.as_object_mut().unwrap()["XmlPayload"] = serde_json::json!(MARKER);
        missing.as_object_mut().unwrap().remove("SessionId");

        let rendered = build(&missing.to_string()).unwrap_err().to_string();
        assert!(!rendered.contains(MARKER), "{rendered}");

        // And through the timestamp path, which *is* allowed to quote a
        // value — but only the timestamp.
        let mut bad_stamp = document();
        bad_stamp.as_object_mut().unwrap()["RawPayload"] = serde_json::json!(MARKER);
        bad_stamp.as_object_mut().unwrap()["MessageReceivedAt"] = serde_json::json!("nope");

        let rendered = build(&bad_stamp.to_string()).unwrap_err().to_string();
        assert!(!rendered.contains(MARKER), "{rendered}");
        assert!(rendered.contains("nope"));
    }
}
