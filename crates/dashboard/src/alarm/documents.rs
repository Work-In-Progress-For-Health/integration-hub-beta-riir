//! The dashboard container's documents, as a mixed-type store.
//!
//! Implements `spec/15-dashboard.md` §10.1.
//!
//! # Routing metadata is not payload
//!
//! Cosmos wants `id` and `pk` on every document, and upstream added a
//! `type` discriminator so one container can hold alarms, pauses, contacts
//! and network-test history and still be browsed by kind (`78f2d23`). All
//! three are **stamped on write and stripped on read**, so a caller sees
//! exactly the payload it stored — and, consequently, must keep its own
//! identifier in a field of its own. Upstream lost a source's id to this and
//! shipped a delete button that did nothing (`326b3ec`); [`strip`] is where
//! that happens, and its tests are the reminder.

use serde_json::{Map, Value};

/// `type` of an alarm's configuration document.
pub const ALARM_CONFIG: &str = "alarm_config";
/// `type` of an alarm's state document.
pub const ALARM_STATE: &str = "alarm_state";
/// `type` of a pause (`§10.2`).
pub const ALARM_PAUSE: &str = "alarm_pause";
/// `type` of a contact (`§10.3`).
pub const CONTACT: &str = "contact";
/// `type` of a flow source server (`§10.4`).
pub const FLOW_SOURCE_SERVER: &str = "flow_source_server";
/// `type` of a network test's history (`§10.4`).
pub const NETWORK_TEST_HISTORY: &str = "network_test_history";

/// The keys that belong to storage and not to the caller.
const ROUTING_KEYS: [&str; 3] = ["id", "pk", "type"];

/// A document ready to write: the payload with routing metadata stamped on.
///
/// Anything in `payload` named `id`, `pk` or `type` is **overridden**, not
/// merged: the arguments are the truth. A payload that carried its own `pk`
/// would otherwise write into a partition the caller did not ask for.
///
/// # Examples
///
/// ```
/// use dashboard::alarm::documents::{stamp, CONTACT};
///
/// let document = stamp("contacts", "c-1", CONTACT, &serde_json::json!({"id": "hacked", "first_name": "A"}));
///
/// assert_eq!(document["id"], "c-1");
/// assert_eq!(document["pk"], "contacts");
/// assert_eq!(document["type"], "contact");
/// assert_eq!(document["first_name"], "A");
/// ```
#[must_use]
pub fn stamp(partition_key: &str, id: &str, doc_type: &str, payload: &Value) -> Value {
    let mut document: Map<String, Value> = payload
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(key, _)| !ROUTING_KEYS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();

    document.insert("id".to_string(), Value::String(id.to_string()));
    document.insert("pk".to_string(), Value::String(partition_key.to_string()));
    document.insert("type".to_string(), Value::String(doc_type.to_string()));

    Value::Object(document)
}

/// A stored document with the routing metadata and Cosmos' own properties
/// removed.
///
/// `_rid`, `_etag`, `_ts` and the rest of the `_`-prefixed system
/// properties go too: they are Cosmos' bookkeeping, change on every write,
/// and would otherwise surface in an API response and in equality checks.
#[must_use]
pub fn strip(document: &Value) -> Value {
    let Some(object) = document.as_object() else {
        return document.clone();
    };

    Value::Object(
        object
            .iter()
            .filter(|(key, _)| !ROUTING_KEYS.contains(&key.as_str()) && !key.starts_with('_'))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// The `type` a stored document carries, when it has one.
#[must_use]
pub fn type_of(document: &Value) -> Option<&str> {
    document.get("type").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_cannot_choose_its_own_partition() {
        // spec/15-dashboard.md §10.1.3 — `id`, `pk` and `type` in the
        // caller's payload are overridden. Merging would let a payload that
        // carried `pk` write into somebody else's partition.
        let document = stamp(
            "alarm1",
            "config",
            ALARM_CONFIG,
            &serde_json::json!({"id": "hacked", "pk": "hacked", "type": "hacked", "rules": {}}),
        );

        assert_eq!(document["id"], "config");
        assert_eq!(document["pk"], "alarm1");
        assert_eq!(document["type"], "alarm_config");
        assert!(document["rules"].is_object());
    }

    #[test]
    fn a_stored_document_comes_back_as_exactly_its_payload() {
        // §10.1.3 — routing metadata and Cosmos' `_` properties are
        // storage's, not the caller's.
        let stored = serde_json::json!({
            "id": "config", "pk": "alarm1", "type": "alarm_config",
            "_rid": "abc", "_etag": "xyz", "_ts": 123,
            "rules": {"r": {}}
        });

        assert_eq!(strip(&stored), serde_json::json!({"rules": {"r": {}}}));
    }

    #[test]
    fn a_records_own_identifier_must_not_be_called_id() {
        // §10.1.3 — the bug upstream shipped in 326b3ec: a source's `id`
        // went in and never came back, so edit and delete had nothing to
        // address. The remedy is a field of its own.
        let written = stamp(
            "flow-source-server",
            "storage-1",
            FLOW_SOURCE_SERVER,
            &serde_json::json!({"id": "public-id", "source_id": "public-id"}),
        );
        let read = strip(&written);

        assert!(
            read.get("id").is_none(),
            "`id` does not survive the round trip"
        );
        assert_eq!(read["source_id"], "public-id", "a field of its own does");
    }

    #[test]
    fn a_payload_that_is_not_an_object_stamps_as_just_the_metadata() {
        let document = stamp("p", "i", CONTACT, &serde_json::json!("text"));

        assert_eq!(type_of(&document), Some("contact"));
        assert_eq!(document.as_object().map(Map::len), Some(3));
    }

    #[test]
    fn the_type_names_are_the_pythons() {
        // They are what an existing container already holds; a different
        // spelling would be a second kind of document nobody queries.
        assert_eq!(ALARM_CONFIG, "alarm_config");
        assert_eq!(ALARM_STATE, "alarm_state");
        assert_eq!(ALARM_PAUSE, "alarm_pause");
        assert_eq!(CONTACT, "contact");
        assert_eq!(FLOW_SOURCE_SERVER, "flow_source_server");
        assert_eq!(NETWORK_TEST_HISTORY, "network_test_history");
    }
}
