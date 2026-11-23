//! The alarm store backed by Cosmos DB.
//!
//! Implements `spec/15-dashboard.md` §5.6, §5.6.2 and §5.7.
//!
//! # The in-memory layer is not a cache
//!
//! §5.6.2. It exists so that a **failed write still suppresses the next
//! escalation**. The cooldown of §5.1.2 step 6 is the only thing stopping a
//! firing alarm e-mailing on every evaluation cycle; if a write fails and
//! the process forgets, the next cycle reads no `last_alarm_at` and
//! escalates again, every thirty seconds, for as long as the write keeps
//! failing.
//!
//! That is §9.9 — a real failure mode rather than a theoretical one,
//! because a Cosmos write costs more RUs than a read and so throttles
//! first. "Reads fine, writes rejected" is what RU exhaustion looks like.
//!
//! # Documents
//!
//! Two per alarm, in the partition named by [`Partition`]: `config` holds
//! the rules and `state` holds the cooldowns. Both are shaped
//! `{"rules": {id: {...}}}`, which is what the Python already wrote — an
//! existing deployment's documents have to keep working.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ihb_config::Secret;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::alarm::documents::{ALARM_STATE, stamp, strip};
use crate::alarm::store::{AlarmStore, Partition, StoreError};
use crate::alarm::{RuleConfig, RuleState};
use crate::azure::cosmos::{
    COSMOS_API_VERSION, ResourceType, authorization, cosmos_date, document_url, documents_link,
    documents_url,
};
use crate::azure::http::{HttpError, HttpsClient};
use crate::azure::token::TokenSource;

/// The document holding an alarm's rules.
pub const CONFIG_DOC_ID: &str = "config";

/// The document holding an alarm's cooldowns.
pub const STATE_DOC_ID: &str = "state";

/// How long Cosmos is left alone after a connectivity failure
/// (`spec/15-dashboard.md` §5.6.4; upstream `#461`, `_UNAVAILABLE_BACKOFF_SECONDS`).
pub const UNAVAILABLE_BACKOFF: Duration = Duration::from_secs(30);

/// A pause after the account could not be reached.
///
/// §5.6.4. Without it every page and every evaluation cycle waits out the
/// full connect and request timeouts against an account that is down, so a
/// Cosmos outage makes the whole dashboard slow, not just its alarms. The
/// clock is passed in so the rule is tested without sleeping.
#[derive(Debug, Default)]
pub struct Backoff {
    /// When the pause ends, if there is one.
    until: Mutex<Option<Instant>>,
}

impl Backoff {
    /// Whether requests should be skipped at `now`.
    #[must_use]
    pub fn active_at(&self, now: Instant) -> bool {
        self.until
            .lock()
            .ok()
            .and_then(|until| *until)
            .is_some_and(|until| now < until)
    }

    /// Start a pause of `length` at `now`.
    pub fn start_at(&self, now: Instant, length: Duration) {
        // RCA-TIME-1: no `Instant + Duration`, which panics on overflow. An
        // end that cannot be represented starts no pause (`§5.6.4`: the
        // request goes ahead and fails or succeeds on its own).
        if let Ok(mut until) = self.until.lock() {
            *until = now.checked_add(length);
        }
    }

    /// Pause if `error` says the account was not reached.
    ///
    /// Only a connection failure or a timeout does. **An HTTP status means
    /// Cosmos answered** (a 404 for an unconfigured alarm, a 429 under RU
    /// exhaustion) and so is reachable; pausing on it would hide the
    /// writes §5.6.2 exists to survive.
    pub fn note(&self, error: &HttpError, now: Instant) {
        if matches!(error, HttpError::Connect(_) | HttpError::Timeout) {
            self.start_at(now, UNAVAILABLE_BACKOFF);
        }
    }
}

/// How the store authenticates (§5.6, §5.7).
pub enum Credential {
    /// `COSMOS_KEY` — HMAC-signed requests (§5.7.1).
    Key(Secret),
    /// AAD RBAC (§5.7.4).
    Rbac(TokenSource),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // §5.7.5 — a key is a full-access credential for the account.
        f.write_str(match self {
            Self::Key(_) => "Credential::Key(<redacted>)",
            Self::Rbac(_) => "Credential::Rbac",
        })
    }
}

/// An [`AlarmStore`] backed by Cosmos DB.
#[derive(Debug)]
pub struct CosmosStore {
    /// `COSMOS_ENDPOINT`.
    endpoint: String,
    /// `COSMOS_DATABASE`.
    database: String,
    /// `COSMOS_CONTAINER`.
    container: String,
    /// How to authenticate.
    credential: Credential,
    /// The HTTPS client.
    client: HttpsClient,
    /// §5.6.2's in-memory layer — see this module's documentation.
    remembered: Mutex<BTreeMap<Partition, BTreeMap<String, RuleState>>>,
    /// §5.6.4's pause after a connectivity failure.
    backoff: Backoff,
}

impl CosmosStore {
    /// A store for this account.
    #[must_use]
    pub fn new(
        endpoint: String,
        database: String,
        container: String,
        credential: Credential,
        client: HttpsClient,
    ) -> Self {
        Self {
            endpoint,
            database,
            container,
            credential,
            client,
            remembered: Mutex::new(BTreeMap::new()),
            backoff: Backoff::default(),
        }
    }

    /// Turn a failed request into the store's error, pausing first if the
    /// account was unreachable (§5.6.4).
    fn failed(&self, error: &HttpError) -> StoreError {
        self.backoff.note(error, Instant::now());
        StoreError::Unavailable(error.to_string())
    }

    /// The scope an RBAC token is taken for (§5.7.4).
    #[must_use]
    pub fn rbac_scope(endpoint: &str) -> String {
        format!("{}/.default", endpoint.trim_end_matches('/'))
    }

    /// The headers a read or write needs.
    async fn headers(
        &self,
        verb: &str,
        resource_type: ResourceType,
        resource_link: &str,
        partition_key: &str,
    ) -> Result<Vec<(String, String)>, StoreError> {
        // §5.6.4. Before any signing or token request: during the pause
        // nothing is sent. Still `Unavailable`, never an empty result (§5.6.3).
        if self.backoff.active_at(Instant::now()) {
            return Err(StoreError::Unavailable(
                "Cosmos DB was unreachable a moment ago; not retrying yet".to_string(),
            ));
        }
        let date = cosmos_date(OffsetDateTime::now_utc());

        let authorization = match &self.credential {
            Credential::Key(key) => authorization(key, verb, resource_type, resource_link, &date)
                .map_err(StoreError::Unavailable)?,
            Credential::Rbac(tokens) => {
                let scope = Self::rbac_scope(&self.endpoint);
                let token = tokens
                    .token(&scope)
                    .await
                    .map_err(|error| StoreError::Unavailable(error.to_string()))?;
                format!("type=aad&ver=1.0&sig={token}")
            }
        };

        Ok(vec![
            ("authorization".to_string(), authorization),
            ("x-ms-date".to_string(), date),
            ("x-ms-version".to_string(), COSMOS_API_VERSION.to_string()),
            // The partition key is a JSON array of one value.
            (
                "x-ms-documentdb-partitionkey".to_string(),
                format!("[\"{partition_key}\"]"),
            ),
        ])
    }

    /// Read one document, or `None` when it is not there.
    async fn read(
        &self,
        partition: Partition,
        doc_id: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let link = format!(
            "{}/docs/{doc_id}",
            documents_link(&self.database, &self.container)
        );
        let headers = self
            .headers("GET", ResourceType::Docs, &link, partition.as_str())
            .await?;
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let url = document_url(&self.endpoint, &self.database, &self.container, doc_id);

        match self.client.get_json_with(&url, &borrowed).await {
            Ok(document) => Ok(Some(document)),
            // A document that is not there is not a failure: it is an alarm
            // nobody has configured yet.
            Err(HttpError::Status { code: 404, .. }) => Ok(None),
            Err(error) => Err(self.failed(&error)),
        }
    }
}

impl AlarmStore for CosmosStore {
    async fn rules(&self, partition: Partition) -> Result<Vec<RuleConfig>, StoreError> {
        // §5.6.3 — a read failure is an error, never an empty list. The
        // second would silently disable every rule in this alarm and show
        // a clean page (§9.10).
        let Some(document) = self.read(partition, CONFIG_DOC_ID).await? else {
            return Ok(Vec::new());
        };

        Ok(parse_rules(&document))
    }

    async fn state(&self, partition: Partition) -> Result<BTreeMap<String, RuleState>, StoreError> {
        match self.read(partition, STATE_DOC_ID).await {
            Ok(Some(document)) => Ok(parse_state(&document)),
            Ok(None) => Ok(BTreeMap::new()),
            Err(error) => {
                // §5.6.2 — fall back to what this process last wrote, so a
                // read failure does not reset every cooldown to "never
                // alarmed" and re-escalate.
                if let Some(remembered) = self.remembered(partition) {
                    tracing::warn!(
                        %error,
                        alarm = partition.as_str(),
                        "the alarm state could not be read; using what this process last wrote"
                    );
                    return Ok(remembered);
                }
                Err(error)
            }
        }
    }

    async fn put_state(
        &self,
        partition: Partition,
        state: &BTreeMap<String, RuleState>,
    ) -> Result<(), StoreError> {
        // §5.6.2 — remembered **first**, so the cooldown holds even if the
        // write does not land. This ordering is the whole point.
        if let Ok(mut remembered) = self.remembered.lock() {
            remembered.insert(partition, state.clone());
        }

        let link = documents_link(&self.database, &self.container);
        let mut headers = self
            .headers("POST", ResourceType::Docs, &link, partition.as_str())
            .await?;
        headers.push(("x-ms-documentdb-is-upsert".to_string(), "true".to_string()));

        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let body = state_document(partition, state).to_string();
        let url = documents_url(&self.endpoint, &self.database, &self.container);

        match self.client.post_json_with(&url, &borrowed, body).await {
            Ok(_) => Ok(()),
            Err(error) => Err(self.failed(&error)),
        }
    }

    fn is_enabled(&self) -> bool {
        true
    }

    async fn documents(&self, partition_key: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        // A query is a POST to the collection signed with the collection's
        // link, told apart from a create by its content type (§10.1).
        let link = documents_link(&self.database, &self.container);
        let mut headers = self
            .headers("POST", ResourceType::Docs, &link, partition_key)
            .await?;
        headers.push(("x-ms-documentdb-isquery".to_string(), "true".to_string()));
        // `-1` lets the server size the page; see §10.1.5 for what is not
        // followed.
        headers.push(("x-ms-max-item-count".to_string(), "-1".to_string()));

        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let url = documents_url(&self.endpoint, &self.database, &self.container);

        match self
            .client
            .post_query_with(&url, &borrowed, query_body())
            .await
        {
            Ok(response) => Ok(parse_query_response(&response)),
            Err(error) => Err(self.failed(&error)),
        }
    }

    async fn upsert_document(
        &self,
        partition_key: &str,
        id: &str,
        doc_type: &str,
        payload: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let link = documents_link(&self.database, &self.container);
        let mut headers = self
            .headers("POST", ResourceType::Docs, &link, partition_key)
            .await?;
        headers.push(("x-ms-documentdb-is-upsert".to_string(), "true".to_string()));

        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let body = stamp(partition_key, id, doc_type, payload).to_string();
        let url = documents_url(&self.endpoint, &self.database, &self.container);

        match self.client.post_json_with(&url, &borrowed, body).await {
            Ok(_) => Ok(()),
            Err(error) => Err(self.failed(&error)),
        }
    }

    async fn create_document(
        &self,
        partition_key: &str,
        id: &str,
        doc_type: &str,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        // No `is-upsert` header: Cosmos then answers `409 Conflict` for an
        // id that exists, which is the whole point (§10.4.6).
        let link = documents_link(&self.database, &self.container);
        let headers = self
            .headers("POST", ResourceType::Docs, &link, partition_key)
            .await?;
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let body = stamp(partition_key, id, doc_type, payload).to_string();
        let url = documents_url(&self.endpoint, &self.database, &self.container);

        match self.client.post_json_with(&url, &borrowed, body).await {
            Ok(_) => Ok(true),
            Err(HttpError::Status { code: 409, .. }) => Ok(false),
            Err(error) => Err(self.failed(&error)),
        }
    }

    async fn delete_document(&self, partition_key: &str, id: &str) -> Result<(), StoreError> {
        let link = format!(
            "{}/docs/{id}",
            documents_link(&self.database, &self.container)
        );
        let headers = self
            .headers("DELETE", ResourceType::Docs, &link, partition_key)
            .await?;
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let url = document_url(&self.endpoint, &self.database, &self.container, id);

        match self.client.delete_with(&url, &borrowed).await {
            // Already gone (404) is the outcome that was asked for.
            Ok(_) | Err(HttpError::Status { code: 404, .. }) => Ok(()),
            Err(error) => Err(self.failed(&error)),
        }
    }
}

/// The query for every document in the partition named by the request's
/// partition-key header.
///
/// No `WHERE`: the partition header already scopes it, and a `type` filter
/// would hide a document written before the discriminator existed.
#[must_use]
pub fn query_body() -> String {
    serde_json::json!({"query": "SELECT * FROM c", "parameters": []}).to_string()
}

/// The documents in a Cosmos query response, routing metadata stripped.
///
/// `{"Documents": [...], "_count": n}`. A response without `Documents` is
/// no documents rather than a panic.
#[must_use]
pub fn parse_query_response(response: &serde_json::Value) -> Vec<serde_json::Value> {
    response
        .get("Documents")
        .and_then(serde_json::Value::as_array)
        .map_or_default(|documents| documents.iter().map(strip).collect())
}

impl CosmosStore {
    /// What this process last wrote for a partition (§5.6.2).
    fn remembered(&self, partition: Partition) -> Option<BTreeMap<String, RuleState>> {
        self.remembered.lock().ok()?.get(&partition).cloned()
    }
}

/// The document a state write sends.
///
/// `id` and `pk` are the routing fields the Python writes, so an existing
/// deployment's documents keep their shape.
#[must_use]
pub fn state_document(
    partition: Partition,
    state: &BTreeMap<String, RuleState>,
) -> serde_json::Value {
    let mut rules = serde_json::Map::new();

    for (id, held) in state {
        let mut entry = serde_json::Map::new();
        if let Some(stamp) = held.last_alarm_at.and_then(rfc3339) {
            entry.insert("last_alarm_at".to_string(), stamp);
        }
        if let Some(stamp) = held.paused_until.and_then(rfc3339) {
            entry.insert("paused_until".to_string(), stamp);
        }
        if let Some(reason) = &held.pause_reason {
            entry.insert(
                "pause_reason".to_string(),
                serde_json::Value::String(reason.clone()),
            );
        }
        rules.insert(id.clone(), serde_json::Value::Object(entry));
    }

    serde_json::json!({
        "id": STATE_DOC_ID,
        "pk": partition.as_str(),
        // §10.1.2 — storage-managed, so the mixed container can be browsed
        // by kind.
        "type": ALARM_STATE,
        "rules": rules,
    })
}

/// A timestamp as the Python writes it, or `None` when it cannot be written
/// (a year outside 0..=9999). An unwritable moment is left out of the
/// document, never stored as an empty string that reads back as garbage
/// (RCA-IN-6).
fn rfc3339(at: OffsetDateTime) -> Option<serde_json::Value> {
    at.format(&Rfc3339).ok().map(serde_json::Value::String)
}

/// Read the rules out of a `config` document.
#[must_use]
pub fn parse_rules(document: &serde_json::Value) -> Vec<RuleConfig> {
    let Some(rules) = document.get("rules").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };

    rules
        .iter()
        .map(|(id, entry)| {
            let string = |name: &str| {
                entry
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let flag = |name: &str| {
                entry
                    .get(name)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            };

            let workflow_id = string("workflow_id");
            let display_name = {
                let given = string("display_name");
                if given.is_empty() {
                    // The Python falls back to the workflow id, then the
                    // rule id. An empty label is a blank cell on the page.
                    if workflow_id.is_empty() {
                        id.clone()
                    } else {
                        workflow_id.clone()
                    }
                } else {
                    given
                }
            };

            RuleConfig {
                id: id.clone(),
                display_name,
                workflow_id,
                alarm_enabled: flag("alarm_enabled"),
                deleted: flag("deleted"),
                alerting_gap_minutes: entry
                    .get("alerting_gap_minutes")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(60),
                email_alerts_enabled: flag("email_alerts_enabled"),
                // §5.2 — forced false when alerts are off, on read as well
                // as on write, so a stale document cannot re-enable it.
                email_ooh_enabled: flag("email_ooh_enabled") && flag("email_alerts_enabled"),
            }
        })
        .collect()
}

/// Read the cooldowns out of a `state` document.
#[must_use]
pub fn parse_state(document: &serde_json::Value) -> BTreeMap<String, RuleState> {
    let Some(rules) = document.get("rules").and_then(serde_json::Value::as_object) else {
        return BTreeMap::new();
    };

    rules
        .iter()
        .map(|(id, entry)| {
            let at = |name: &str| {
                entry
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
            };

            (
                id.clone(),
                RuleState {
                    last_alarm_at: at("last_alarm_at"),
                    paused_until: at("paused_until"),
                    pause_reason: entry
                        .get("pause_reason")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn a_config_document_is_read_the_way_the_python_wrote_it() {
        // spec/15-dashboard.md §5.6 — an existing deployment's documents
        // have to keep working.
        let document = serde_json::json!({
            "id": "config",
            "pk": "alarm1",
            "rules": {
                "phw-to-mpi": {
                    "display_name": "PHW → MPI",
                    "workflow_id": "phw-to-mpi",
                    "alarm_enabled": true,
                    "alerting_gap_minutes": 30,
                    "email_alerts_enabled": true,
                    "email_ooh_enabled": true
                }
            }
        });

        let rules = parse_rules(&document);

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "phw-to-mpi");
        assert_eq!(rules[0].display_name, "PHW → MPI");
        assert!(rules[0].alarm_enabled);
        assert_eq!(rules[0].alerting_gap_minutes, 30);
        assert!(rules[0].email_ooh_enabled);
    }

    #[test]
    fn a_stale_out_of_hours_flag_cannot_re_enable_itself() {
        // §5.2 — forced false when alerts are off, on read as well as on
        // write. A document written before that guard existed would
        // otherwise turn e-mail back on.
        let document = serde_json::json!({
            "rules": {
                "r": { "email_alerts_enabled": false, "email_ooh_enabled": true }
            }
        });

        let rules = parse_rules(&document);

        assert!(!rules[0].email_alerts_enabled);
        assert!(!rules[0].email_ooh_enabled);
    }

    #[test]
    fn a_rule_with_no_display_name_still_has_a_label() {
        // A blank cell on an alarm page is worse than a technical name.
        let document = serde_json::json!({
            "rules": {
                "rule-1": { "workflow_id": "phw-to-mpi" },
                "rule-2": {}
            }
        });

        let rules = parse_rules(&document);
        let by_id: BTreeMap<_, _> = rules.iter().map(|r| (r.id.clone(), r)).collect();

        assert_eq!(by_id["rule-1"].display_name, "phw-to-mpi");
        assert_eq!(by_id["rule-2"].display_name, "rule-2");
    }

    #[test]
    fn a_missing_alerting_gap_defaults_rather_than_zeroing() {
        // A gap of 0 re-fires every cycle. Defaulting to 0 on a document
        // that simply predates the field would be a mail storm.
        let document = serde_json::json!({"rules": {"r": {"alarm_enabled": true}}});

        assert_eq!(parse_rules(&document)[0].alerting_gap_minutes, 60);
    }

    #[test]
    fn a_state_document_round_trips() {
        // What is written must read back the same, or a cooldown is lost
        // on the next cycle and the alarm re-escalates.
        let mut state = BTreeMap::new();
        state.insert(
            "phw".to_string(),
            RuleState {
                last_alarm_at: Some(datetime!(2026-08-22 09:15:00 UTC)),
                paused_until: Some(datetime!(2026-08-22 10:00:00 UTC)),
                pause_reason: Some("maintenance".to_string()),
            },
        );

        let document = state_document(Partition::Alarm1, &state);
        let back = parse_state(&document);

        assert_eq!(back["phw"].last_alarm_at, state["phw"].last_alarm_at);
        assert_eq!(back["phw"].paused_until, state["phw"].paused_until);
        assert_eq!(back["phw"].pause_reason.as_deref(), Some("maintenance"));
    }

    #[test]
    fn a_state_document_carries_its_routing_fields() {
        // §5.6 — `id` and `pk`, which is how the Python writes it and how
        // Cosmos routes it.
        let document = state_document(Partition::Alarm2, &BTreeMap::new());

        assert_eq!(document["id"], "state");
        assert_eq!(document["pk"], "alarm2");
        assert!(document["rules"].is_object());
    }

    #[test]
    fn a_state_document_is_typed() {
        // spec/15-dashboard.md §10.1.2.
        let document = state_document(Partition::Alarm3, &BTreeMap::new());

        assert_eq!(document["type"], "alarm_state");
    }

    #[test]
    fn a_query_response_yields_its_documents_without_routing_metadata() {
        // §10.1.3 — `id`, `pk`, `type` and the `_` properties are storage's.
        let response = serde_json::json!({
            "Documents": [
                {"id": "a", "pk": "contacts", "type": "contact", "_rid": "x", "contact_id": "a"},
                {"id": "b", "pk": "contacts", "type": "contact", "contact_id": "b"}
            ],
            "_count": 2
        });

        let documents = parse_query_response(&response);

        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0], serde_json::json!({"contact_id": "a"}));
    }

    #[test]
    fn a_query_response_without_documents_is_none_not_a_panic() {
        assert!(parse_query_response(&serde_json::json!({})).is_empty());
        assert!(parse_query_response(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn the_query_has_no_type_filter() {
        // §10.1 — a document written before the discriminator existed has
        // no `type`, and filtering on it would hide the document.
        let body: serde_json::Value = serde_json::from_str(&query_body()).unwrap();

        assert_eq!(body["query"], "SELECT * FROM c");
    }

    #[test]
    fn an_empty_rule_state_writes_an_empty_entry_not_nulls() {
        // A rule whose state was cleared (§5.1.2 step 4) should not come
        // back with `last_alarm_at: null`, which parses to the same thing
        // but makes the document harder to read by hand.
        let mut state = BTreeMap::new();
        state.insert("r".to_string(), RuleState::default());

        let document = state_document(Partition::Alarm1, &state);

        assert_eq!(document["rules"]["r"], serde_json::json!({}));
    }

    #[test]
    fn an_unreadable_document_is_no_rules_rather_than_a_panic() {
        for document in [
            serde_json::json!({}),
            serde_json::json!({"rules": "not an object"}),
            serde_json::json!([]),
            serde_json::Value::Null,
        ] {
            assert!(parse_rules(&document).is_empty(), "{document}");
            assert!(parse_state(&document).is_empty(), "{document}");
        }
    }

    #[test]
    fn an_unparseable_timestamp_is_absent_rather_than_now() {
        // A `last_alarm_at` that failed to parse must not read as "just
        // alarmed" — that would suppress a real alarm for a whole gap.
        let document = serde_json::json!({
            "rules": {"r": {"last_alarm_at": "not a timestamp"}}
        });

        assert_eq!(parse_state(&document)["r"].last_alarm_at, None);
    }

    #[test]
    fn the_rbac_scope_is_the_account() {
        // §5.7.4.
        assert_eq!(
            CosmosStore::rbac_scope("https://x.documents.azure.com"),
            "https://x.documents.azure.com/.default"
        );
        assert_eq!(
            CosmosStore::rbac_scope("https://x.documents.azure.com/"),
            "https://x.documents.azure.com/.default",
            "a trailing slash must not double"
        );
    }

    #[test]
    fn a_connection_failure_pauses_requests_for_thirty_seconds() {
        // §5.6.4, upstream #461 (test_outage_after_container_cached_starts_backoff).
        let backoff = Backoff::default();
        let now = Instant::now();
        assert!(!backoff.active_at(now), "no pause before a failure");

        backoff.note(&HttpError::Connect("refused".into()), now);
        assert!(backoff.active_at(now));
        assert!(backoff.active_at(now + Duration::from_secs(29)));
        assert!(!backoff.active_at(now + Duration::from_secs(30)), "it ends");
    }

    #[test]
    fn a_timeout_pauses_too() {
        let backoff = Backoff::default();
        let now = Instant::now();
        backoff.note(&HttpError::Timeout, now);
        assert!(backoff.active_at(now));
    }

    #[test]
    fn an_http_status_does_not_pause() {
        // §5.6.4, upstream test_http_error_response_does_not_start_backoff:
        // Cosmos answered, so it is reachable. A throttled write (429) is
        // the case §5.6.2 exists for.
        let backoff = Backoff::default();
        let now = Instant::now();
        for code in [401, 404, 429, 500] {
            backoff.note(
                &HttpError::Status {
                    code,
                    body: String::new(),
                },
                now,
            );
        }
        backoff.note(&HttpError::Body("not json".into()), now);
        assert!(!backoff.active_at(now));
    }

    #[test]
    fn a_key_never_reaches_a_debug_line() {
        // §5.7.5 — a full-access credential for the account.
        let credential = Credential::Key(Secret::new("c29tZS1rZXk="));

        let rendered = format!("{credential:?}");
        assert!(!rendered.contains("c29tZS1rZXk"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }
}

#[cfg(test)]
mod write_failure_tests {
    //! §5.6.2's guarantee, which is the reason the in-memory layer exists.
    //!
    //! Exercised against a store that behaves the way Cosmos does under RU
    //! exhaustion: reads succeed, writes are rejected (§9.9).

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use time::OffsetDateTime;
    use time::macros::datetime;

    use crate::alarm::rules::evaluate_all;
    use crate::alarm::store::{AlarmStore, Partition, StoreError};
    use crate::alarm::{RuleConfig, RuleState, Status, Trip};

    /// A store whose reads work and whose writes are rejected, holding
    /// what it was asked to write the way [`super::CosmosStore`] does.
    struct RuThrottled {
        rules: Vec<RuleConfig>,
        remembered: Mutex<BTreeMap<String, RuleState>>,
    }

    impl AlarmStore for RuThrottled {
        async fn rules(&self, _partition: Partition) -> Result<Vec<RuleConfig>, StoreError> {
            Ok(self.rules.clone())
        }

        async fn state(
            &self,
            _partition: Partition,
        ) -> Result<BTreeMap<String, RuleState>, StoreError> {
            // Cosmos has nothing stored, because every write was rejected.
            // §5.6.2's fallback is what makes this the remembered value
            // rather than an empty map.
            Ok(self.remembered.lock().map_or_else(
                |poisoned| poisoned.into_inner().clone(),
                |held| held.clone(),
            ))
        }

        async fn put_state(
            &self,
            _partition: Partition,
            state: &BTreeMap<String, RuleState>,
        ) -> Result<(), StoreError> {
            // Remembered first, then the write fails — the ordering
            // `CosmosStore::put_state` uses, and the whole point of it.
            if let Ok(mut held) = self.remembered.lock() {
                *held = state.clone();
            }
            Err(StoreError::Unavailable("429 TooManyRequests".to_string()))
        }

        fn is_enabled(&self) -> bool {
            true
        }
    }

    /// A Wednesday at 10:00 UTC — in hours, so the guard is not what is
    /// being measured.
    const NOW: OffsetDateTime = datetime!(2026-08-19 10:00 UTC);

    #[tokio::test]
    async fn a_rejected_write_still_suppresses_the_next_escalation() {
        // spec/15-dashboard.md §5.6.2 and §9.9 — THE test of this module.
        //
        // The Python holds nothing, so during RU throttling every cycle
        // reads no `last_alarm_at`, reports `critical` rather than
        // `suppressed`, and e-mails again: up to two a minute per firing
        // rule, against an ACS quota its own code carries retry logic for.
        let store = RuThrottled {
            rules: vec![RuleConfig {
                email_alerts_enabled: true,
                ..RuleConfig::for_test("phw-to-mpi")
            }],
            remembered: Mutex::new(BTreeMap::new()),
        };

        // First cycle: critical, escalates, and the write is rejected.
        let first = evaluate_all(&store, Partition::Alarm1, NOW, |_| Trip::Tripped).await;
        assert!(first.is_err(), "the write failure surfaces");

        // The cooldown was remembered even though the write was rejected.
        let held = store.state(Partition::Alarm1).await.unwrap();
        assert!(
            held.get("phw-to-mpi")
                .and_then(|s| s.last_alarm_at)
                .is_some(),
            "the cooldown was not remembered, so the next cycle re-escalates"
        );

        // Second cycle, ten minutes later and still inside the 60-minute
        // gap. It **succeeds**: the rule is now Suppressed, so its stored
        // state is unchanged, so no write is attempted at all — the
        // throttled write is not even retried.
        let later = NOW + time::Duration::minutes(10);
        let second = evaluate_all(&store, Partition::Alarm1, later, |_| Trip::Tripped)
            .await
            .expect("no write is needed, so nothing can fail");

        assert_eq!(second[0].outcome.status, Status::Suppressed);
        assert!(
            !second[0].outcome.escalate,
            "a rejected write must not turn into an e-mail every cycle"
        );

        // And it stays suppressed for the rest of the gap.
        let nearly = NOW + time::Duration::minutes(59);
        let third = evaluate_all(&store, Partition::Alarm1, nearly, |_| Trip::Tripped)
            .await
            .unwrap();
        assert_eq!(third[0].outcome.status, Status::Suppressed);
        assert!(!third[0].outcome.escalate);
    }

    #[tokio::test]
    async fn without_the_in_memory_layer_it_would_escalate_every_cycle() {
        // The Python's behaviour, written out so the difference is visible
        // rather than asserted about code that is not here. A store that
        // forgets what it tried to write reports `critical` — and so
        // `escalate` — on every single cycle.
        struct Forgetful;

        impl AlarmStore for Forgetful {
            async fn rules(&self, _p: Partition) -> Result<Vec<RuleConfig>, StoreError> {
                Ok(vec![RuleConfig {
                    email_alerts_enabled: true,
                    ..RuleConfig::for_test("phw-to-mpi")
                }])
            }
            async fn state(
                &self,
                _p: Partition,
            ) -> Result<BTreeMap<String, RuleState>, StoreError> {
                Ok(BTreeMap::new())
            }
            async fn put_state(
                &self,
                _p: Partition,
                _s: &BTreeMap<String, RuleState>,
            ) -> Result<(), StoreError> {
                Ok(())
            }
            fn is_enabled(&self) -> bool {
                true
            }
        }

        for minutes in [0, 10, 20, 30] {
            let at = NOW + time::Duration::minutes(minutes);
            let rows = evaluate_all(&Forgetful, Partition::Alarm1, at, |_| Trip::Tripped)
                .await
                .unwrap();

            assert_eq!(
                rows[0].outcome.status,
                Status::Critical,
                "every cycle is a first trip when nothing is remembered"
            );
            assert!(
                rows[0].outcome.escalate,
                "at +{minutes} minutes this would send another e-mail"
            );
        }
    }
}
