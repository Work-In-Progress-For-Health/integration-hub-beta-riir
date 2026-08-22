//! One stored row, and the message it becomes.
//!
//! Implements `spec/13-message-replay.md` §4.1.

use ihb_bus::message::Outgoing;

/// One row of the fetch in §3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `ReplayId` — the ordering key, and half of how a replayed message
    /// is told apart from the original.
    pub replay_id: i64,
    /// `MessageId` — the row in `monitoring.Message` this came from.
    pub message_id: i64,
    /// `RawPayload` — the message, exactly as it first arrived.
    pub raw_payload: String,
    /// `CorrelationId` — the id the message carried the first time.
    pub correlation_id: String,
    /// `SessionId` — the session it was originally sent under.
    pub session_id: String,
}

/// The `application_properties` key carrying the original correlation id.
pub const CORRELATION_ID: &str = "CorrelationId";

/// The key carrying the replay row's identifier.
pub const REPLAY_ID: &str = "ReplayId";

/// The key carrying the stored message's identifier.
pub const MESSAGE_ID: &str = "MessageId";

/// Build the Service Bus message for a stored row.
///
/// # Two decisions worth understanding
///
/// **The original correlation id is preserved** (§4.1.1). A replayed
/// message is traceable to its first passage through the Hub, which is the
/// whole point of replaying it — an operator replaying a batch is
/// investigating what happened the first time. `ReplayId` and `MessageId`
/// are what distinguish the replay from the original, rather than a fresh
/// correlation id that would sever the link.
///
/// **No `message_id` is set** (§4.1.2). Service Bus duplicate detection
/// keys on it, and a replay is a *deliberate* duplicate: setting the
/// original `MSH-10` would have the broker silently suppress exactly the
/// message the operator asked for.
///
/// # Examples
///
/// ```
/// use message_replay_job::page::{Row, message_for};
///
/// let row = Row {
///     replay_id: 42,
///     message_id: 1007,
///     raw_payload: "MSH|^~\\&|252".to_string(),
///     correlation_id: "minted-at-ingress".to_string(),
///     session_id: "phw-to-mpi".to_string(),
/// };
///
/// let message = message_for(&row);
///
/// assert_eq!(message.session_id.as_deref(), Some("phw-to-mpi"));
/// assert_eq!(message.properties["CorrelationId"], "minted-at-ingress");
/// assert_eq!(message.properties["ReplayId"], "42");
/// assert_eq!(message.properties["MessageId"], "1007");
/// assert_eq!(message.message_id, None, "§4.1.2 — never set");
/// ```
#[must_use]
pub fn message_for(row: &Row) -> Outgoing {
    let mut properties = ihb_bus::metadata::Properties::new();
    properties.insert(CORRELATION_ID.to_string(), row.correlation_id.clone());
    properties.insert(REPLAY_ID.to_string(), row.replay_id.to_string());
    properties.insert(MESSAGE_ID.to_string(), row.message_id.to_string());

    Outgoing::new(row.raw_payload.clone())
        .with_session(row.session_id.clone())
        .with_properties(properties)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row {
            replay_id: 42,
            message_id: 1007,
            raw_payload: "MSH|^~\\&|252|252|100|100|20260821||ADT^A31|M1|P|2.5".to_string(),
            correlation_id: "minted-at-ingress".to_string(),
            session_id: "phw-to-mpi".to_string(),
        }
    }

    #[test]
    fn every_field_of_section_four_one() {
        // spec/13-message-replay.md §4.1 and §8.1.
        let message = message_for(&row());

        assert_eq!(
            String::from_utf8(message.body.clone()).unwrap(),
            row().raw_payload
        );
        assert_eq!(message.session_id.as_deref(), Some("phw-to-mpi"));
        assert_eq!(message.properties[CORRELATION_ID], "minted-at-ingress");
        assert_eq!(message.properties[REPLAY_ID], "42");
        assert_eq!(message.properties[MESSAGE_ID], "1007");
    }

    #[test]
    fn the_original_correlation_id_is_preserved() {
        // §4.1.1 — a replayed message is traceable to its first passage
        // through the Hub, which is why an operator is replaying it.
        let message = message_for(&row());

        assert_eq!(
            message.properties[CORRELATION_ID], "minted-at-ingress",
            "not a fresh id, which would sever the link to the original"
        );
    }

    #[test]
    fn no_message_id_is_set() {
        // §4.1.2 — duplicate detection keys on it, and a replay is a
        // deliberate duplicate. Setting it would have the broker silently
        // suppress exactly the message the operator asked for.
        assert_eq!(message_for(&row()).message_id, None);
    }

    #[test]
    fn the_payload_crosses_unchanged() {
        // A replay that altered the message would not be a replay.
        let awkward = Row {
            raw_payload: "MSH|^~\\&|252\rPID|||1~2||O'BRIEN^\"J\"&x".to_string(),
            ..row()
        };

        let message = message_for(&awkward);

        assert_eq!(
            String::from_utf8(message.body).unwrap(),
            awkward.raw_payload,
            "separators, quotes and escapes included"
        );
    }
}
