//! Where alarm configuration and state live.
//!
//! Implements `spec/15-dashboard.md` §5.6.
//!
//! # Three modes, and the first one is not a degraded mode
//!
//! §5.6's table has `COSMOS_ENDPOINT` blank meaning **persistence
//! disabled**: reads return empty and writes are no-ops. §1.2 makes that
//! the normal state on a laptop and in CI, not a failure to apologise for.
//!
//! So [`Disabled`] is a real implementation rather than an error path, and
//! it is the one every test in this crate runs against.
//!
//! # A write that fails must not change what the page says
//!
//! Alarm *state* is a cooldown (§5.1.3). If a write fails and the
//! evaluation carried on as though it had succeeded, the next cycle would
//! re-read the old state and escalate again — turning a Cosmos outage into
//! a mail storm. Every write here returns whether it happened.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::alarm::{RuleConfig, RuleState};

/// Which alarm a document belongs to.
///
/// The Cosmos partition key (§5.6): `alarm1`, `alarm2`, `alarm3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Partition {
    /// Incoming inactivity (§5.3).
    Alarm1,
    /// Outgoing inactivity (§5.4).
    Alarm2,
    /// Processing failures (§5.5).
    Alarm3,
}

impl Partition {
    /// The partition key as it is stored.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alarm1 => "alarm1",
            Self::Alarm2 => "alarm2",
            Self::Alarm3 => "alarm3",
        }
    }

    /// Every partition, for a page that shows all three.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Alarm1, Self::Alarm2, Self::Alarm3]
    }
}

/// Why a store operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The store could not be reached.
    Unavailable(String),
    /// The document was there and could not be read.
    Malformed(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "the alarm store is unavailable: {reason}"),
            Self::Malformed(reason) => write!(f, "the alarm document could not be read: {reason}"),
        }
    }
}

/// Configuration and state for one alarm.
pub trait AlarmStore: Send + Sync + 'static {
    /// Every rule configured for this alarm.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the store could not be read. **Not** an empty
    /// list: a store that is down and a store with no rules configured
    /// must not look the same, or a Cosmos outage would silently disable
    /// every alarm in the estate.
    fn rules(
        &self,
        partition: Partition,
    ) -> impl Future<Output = Result<Vec<RuleConfig>, StoreError>> + Send;

    /// The per-rule state (§5.1.3).
    ///
    /// # Errors
    ///
    /// As [`AlarmStore::rules`].
    fn state(
        &self,
        partition: Partition,
    ) -> impl Future<Output = Result<BTreeMap<String, RuleState>, StoreError>> + Send;

    /// Replace the per-rule state.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write did not happen. The caller must not
    /// treat a failure as success — see this module's documentation.
    fn put_state(
        &self,
        partition: Partition,
        state: &BTreeMap<String, RuleState>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Whether anything is persisted at all (§5.6's first row).
    fn is_enabled(&self) -> bool;

    /// Every document in a partition of the dashboard container, routing
    /// metadata stripped (`spec/15-dashboard.md` §10.1.3).
    ///
    /// The default is an empty partition, which is right for a store that
    /// keeps nothing ([`Disabled`]).
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the partition could not be read — **not** an
    /// empty list (§10.1.4).
    fn documents(
        &self,
        _partition_key: &str,
    ) -> impl Future<Output = Result<Vec<serde_json::Value>, StoreError>> + Send {
        async { Ok(Vec::new()) }
    }

    /// Create or replace one document, stamping `id`, `pk` and `type`
    /// (§10.1.2, §10.1.3).
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write did not happen.
    fn upsert_document(
        &self,
        _partition_key: &str,
        _id: &str,
        _doc_type: &str,
        _payload: &serde_json::Value,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        async { Ok(()) }
    }

    /// Create a document **only if none has this id** (§10.4.6).
    ///
    /// `Ok(true)` when created; `Ok(false)` when one already exists, which
    /// is the answer an upsert cannot give and why two people adding the
    /// same flow source server at once cannot both succeed.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write did not happen.
    fn create_document(
        &self,
        _partition_key: &str,
        _id: &str,
        _doc_type: &str,
        _payload: &serde_json::Value,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send {
        async { Ok(true) }
    }

    /// Delete one document. Deleting one that is not there succeeds.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the delete did not happen.
    fn delete_document(
        &self,
        _partition_key: &str,
        _id: &str,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        async { Ok(()) }
    }
}

/// The store when `COSMOS_ENDPOINT` is blank (§5.6, first row).
///
/// Reads return empty and writes are no-ops. Not an error: this is what
/// §1.2 makes normal on a laptop and in CI.
///
/// # Examples
///
/// ```
/// # tokio_test::block_on(async {
/// use dashboard::alarm::store::{AlarmStore, Disabled, Partition};
///
/// let store = Disabled;
///
/// assert!(!store.is_enabled());
/// assert!(store.rules(Partition::Alarm1).await.unwrap().is_empty());
/// # })
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct Disabled;

impl AlarmStore for Disabled {
    async fn rules(&self, _partition: Partition) -> Result<Vec<RuleConfig>, StoreError> {
        // Empty rather than an error: nothing is configured, which is a
        // valid state and not a fault (§5.6).
        Ok(Vec::new())
    }

    async fn state(
        &self,
        _partition: Partition,
    ) -> Result<BTreeMap<String, RuleState>, StoreError> {
        Ok(BTreeMap::new())
    }

    async fn put_state(
        &self,
        _partition: Partition,
        _state: &BTreeMap<String, RuleState>,
    ) -> Result<(), StoreError> {
        // A no-op that reports success, because nothing was asked to be
        // kept. Reporting failure would make every evaluation log a write
        // error on a correctly-configured laptop.
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        false
    }
}

/// A store held in memory, for tests.
///
/// Not a production mode: it exists so §5's lifecycle can be exercised
/// across evaluation cycles — a cooldown surviving one cycle to the next is
/// the whole of §5.1.3, and [`Disabled`] cannot show it.
#[derive(Debug, Default)]
pub struct InMemory {
    /// Rules, by partition.
    rules: Mutex<BTreeMap<Partition, Vec<RuleConfig>>>,
    /// State, by partition then rule id.
    state: Mutex<BTreeMap<Partition, BTreeMap<String, RuleState>>>,
    /// A reason every operation fails, for the outage case.
    failing: Mutex<Option<String>>,
    /// A reason only writes fail — RU exhaustion (§9.9).
    write_failing: Mutex<Option<String>>,
    /// Documents of the mixed-type container, as stored (stamped), keyed by
    /// partition and id (§10.1).
    docs: Mutex<BTreeMap<(String, String), serde_json::Value>>,
}

impl InMemory {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure a rule.
    pub fn add(&self, partition: Partition, rule: RuleConfig) {
        if let Ok(mut rules) = self.rules.lock() {
            rules.entry(partition).or_default().push(rule);
        }
    }

    /// Make every operation fail, for the outage case.
    pub fn fail(&self, reason: &str) {
        if let Ok(mut failing) = self.failing.lock() {
            *failing = Some(reason.to_string());
        }
    }

    /// Make only **writes** fail — what Cosmos RU exhaustion looks like.
    ///
    /// A write costs more RUs than a read and so throttles first, which is
    /// why §5.6.2 and §9.9 are about this case specifically rather than
    /// about a total outage.
    pub fn fail_writes(&self, reason: &str) {
        if let Ok(mut failing) = self.write_failing.lock() {
            *failing = Some(reason.to_string());
        }
    }

    /// The stored form of one document, `id`/`pk`/`type` included — what a
    /// test needs to assert that the discriminator was written (§10.1.2).
    #[must_use]
    pub fn raw_document(&self, partition_key: &str, id: &str) -> Option<serde_json::Value> {
        self.docs
            .lock()
            .ok()?
            .get(&(partition_key.to_string(), id.to_string()))
            .cloned()
    }

    /// The reason operations are failing, if any.
    fn failure(&self) -> Option<StoreError> {
        self.failing
            .lock()
            .ok()?
            .clone()
            .map(StoreError::Unavailable)
    }
}

impl AlarmStore for InMemory {
    async fn rules(&self, partition: Partition) -> Result<Vec<RuleConfig>, StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }

        Ok(self
            .rules
            .lock()
            .map_err(|_| StoreError::Unavailable("poisoned".to_string()))?
            .get(&partition)
            .cloned()
            .unwrap_or_default())
    }

    async fn state(&self, partition: Partition) -> Result<BTreeMap<String, RuleState>, StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }

        Ok(self
            .state
            .lock()
            .map_err(|_| StoreError::Unavailable("poisoned".to_string()))?
            .get(&partition)
            .cloned()
            .unwrap_or_default())
    }

    async fn put_state(
        &self,
        partition: Partition,
        state: &BTreeMap<String, RuleState>,
    ) -> Result<(), StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        if let Some(reason) = self.write_failing.lock().ok().and_then(|r| r.clone()) {
            return Err(StoreError::Unavailable(reason));
        }

        self.state
            .lock()
            .map_err(|_| StoreError::Unavailable("poisoned".to_string()))?
            .insert(partition, state.clone());

        Ok(())
    }

    fn is_enabled(&self) -> bool {
        true
    }

    async fn documents(&self, partition_key: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }

        Ok(self
            .docs
            .lock()
            .map_err(|_| StoreError::Unavailable("poisoned".to_string()))?
            .iter()
            .filter(|((pk, _), _)| pk == partition_key)
            .map(|(_, document)| crate::alarm::documents::strip(document))
            .collect())
    }

    async fn upsert_document(
        &self,
        partition_key: &str,
        id: &str,
        doc_type: &str,
        payload: &serde_json::Value,
    ) -> Result<(), StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        if let Some(reason) = self.write_failing.lock().ok().and_then(|r| r.clone()) {
            return Err(StoreError::Unavailable(reason));
        }

        self.docs
            .lock()
            .map_err(|_| StoreError::Unavailable("poisoned".to_string()))?
            .insert(
                (partition_key.to_string(), id.to_string()),
                crate::alarm::documents::stamp(partition_key, id, doc_type, payload),
            );

        Ok(())
    }

    async fn create_document(
        &self,
        partition_key: &str,
        id: &str,
        doc_type: &str,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        if let Some(reason) = self.write_failing.lock().ok().and_then(|r| r.clone()) {
            return Err(StoreError::Unavailable(reason));
        }

        let mut docs = self
            .docs
            .lock()
            .map_err(|_| StoreError::Unavailable("poisoned".to_string()))?;
        let key = (partition_key.to_string(), id.to_string());
        if docs.contains_key(&key) {
            return Ok(false);
        }
        docs.insert(
            key,
            crate::alarm::documents::stamp(partition_key, id, doc_type, payload),
        );

        Ok(true)
    }

    async fn delete_document(&self, partition_key: &str, id: &str) -> Result<(), StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        if let Some(reason) = self.write_failing.lock().ok().and_then(|r| r.clone()) {
            return Err(StoreError::Unavailable(reason));
        }

        self.docs
            .lock()
            .map_err(|_| StoreError::Unavailable("poisoned".to_string()))?
            .remove(&(partition_key.to_string(), id.to_string()));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_partition_keys_are_the_pythons() {
        // spec/15-dashboard.md §5.6 — these are the Cosmos partition keys
        // an existing deployment already has documents under. A different
        // spelling would read as an empty database.
        assert_eq!(Partition::Alarm1.as_str(), "alarm1");
        assert_eq!(Partition::Alarm2.as_str(), "alarm2");
        assert_eq!(Partition::Alarm3.as_str(), "alarm3");
        assert_eq!(Partition::all().len(), 3);
    }

    #[tokio::test]
    async fn a_disabled_store_reads_empty_and_writes_nothing() {
        // §5.6's first row and §1.2. This is the normal state on a laptop,
        // not a degraded one.
        let store = Disabled;

        assert!(!store.is_enabled());
        assert!(store.rules(Partition::Alarm1).await.unwrap().is_empty());
        assert!(store.state(Partition::Alarm1).await.unwrap().is_empty());
        assert!(
            store
                .put_state(Partition::Alarm1, &BTreeMap::new())
                .await
                .is_ok(),
            "a no-op reports success, or every evaluation logs a write error"
        );
    }

    #[tokio::test]
    async fn an_in_memory_store_keeps_state_across_cycles() {
        // §5.1.3 — the cooldown surviving one evaluation to the next is the
        // whole point of persisting anything.
        let store = InMemory::new();

        let mut state = BTreeMap::new();
        state.insert(
            "phw".to_string(),
            RuleState {
                last_alarm_at: Some(time::macros::datetime!(2026-08-22 09:00 UTC)),
                ..RuleState::default()
            },
        );

        store.put_state(Partition::Alarm1, &state).await.unwrap();

        let read = store.state(Partition::Alarm1).await.unwrap();
        assert_eq!(read["phw"].last_alarm_at, state["phw"].last_alarm_at);
    }

    #[tokio::test]
    async fn the_partitions_do_not_see_each_other() {
        // Three alarms, three partitions. Alarm 1's cooldown must not
        // suppress Alarm 3.
        let store = InMemory::new();

        let mut state = BTreeMap::new();
        state.insert("shared-id".to_string(), RuleState::default());
        store.put_state(Partition::Alarm1, &state).await.unwrap();

        assert_eq!(store.state(Partition::Alarm1).await.unwrap().len(), 1);
        assert!(store.state(Partition::Alarm2).await.unwrap().is_empty());
        assert!(store.state(Partition::Alarm3).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_store_that_is_down_is_an_error_not_an_empty_list() {
        // THE distinction in this file. A store that is down and a store
        // with no rules configured must not look the same: the first is an
        // outage somebody must fix, the second is a Tuesday. Returning
        // empty for both would silently disable every alarm in the estate
        // and report everything healthy.
        let store = InMemory::new();
        store.add(Partition::Alarm1, RuleConfig::for_test("phw-to-mpi"));
        store.fail("Cosmos returned 503");

        assert!(store.rules(Partition::Alarm1).await.is_err());
        assert!(store.state(Partition::Alarm1).await.is_err());
        assert!(
            store
                .put_state(Partition::Alarm1, &BTreeMap::new())
                .await
                .is_err(),
            "a failed write must not report success, or the next cycle \
             re-reads the old state and escalates again"
        );
    }

    #[tokio::test]
    async fn a_document_is_stamped_on_write_and_stripped_on_read() {
        // spec/15-dashboard.md §10.1.2, §10.1.3.
        let store = InMemory::new();

        store
            .upsert_document(
                "contacts",
                "c-1",
                crate::alarm::documents::CONTACT,
                &serde_json::json!({"contact_id": "c-1", "first_name": "A"}),
            )
            .await
            .unwrap();

        let stored = store.raw_document("contacts", "c-1").unwrap();
        assert_eq!(stored["type"], "contact");
        assert_eq!(stored["pk"], "contacts");

        let read = store.documents("contacts").await.unwrap();
        assert_eq!(
            read,
            vec![serde_json::json!({"contact_id": "c-1", "first_name": "A"})]
        );
    }

    #[tokio::test]
    async fn create_refuses_an_id_that_exists_and_upsert_does_not() {
        // spec/15-dashboard.md §10.4.6 -- the answer an upsert cannot give.
        let store = InMemory::new();
        let payload = serde_json::json!({"n": 1});

        assert!(
            store
                .create_document("p", "i", "t", &payload)
                .await
                .unwrap()
        );
        assert!(
            !store
                .create_document("p", "i", "t", &payload)
                .await
                .unwrap()
        );
        assert!(
            store
                .create_document("p", "other", "t", &payload)
                .await
                .unwrap()
        );
        assert!(store.upsert_document("p", "i", "t", &payload).await.is_ok());
    }

    #[tokio::test]
    async fn documents_are_kept_per_partition_and_can_be_deleted() {
        let store = InMemory::new();
        for (pk, id) in [("a", "1"), ("a", "2"), ("b", "1")] {
            store
                .upsert_document(pk, id, "contact", &serde_json::json!({"n": id}))
                .await
                .unwrap();
        }

        assert_eq!(store.documents("a").await.unwrap().len(), 2);
        assert_eq!(store.documents("b").await.unwrap().len(), 1);

        store.delete_document("a", "1").await.unwrap();
        store.delete_document("a", "absent").await.unwrap();
        assert_eq!(store.documents("a").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_document_read_from_a_store_that_is_down_is_an_error() {
        // §10.1.4 — an outage is not an empty partition.
        let store = InMemory::new();
        store.fail("Cosmos returned 503");

        assert!(store.documents("contacts").await.is_err());
        assert!(
            store
                .upsert_document("contacts", "c", "contact", &serde_json::json!({}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_disabled_store_holds_no_documents() {
        let store = Disabled;

        assert!(store.documents("contacts").await.unwrap().is_empty());
        assert!(!store.is_enabled());
    }

    #[test]
    fn every_error_says_what_happened() {
        for error in [
            StoreError::Unavailable("503".to_string()),
            StoreError::Malformed("not an object".to_string()),
        ] {
            assert!(error.to_string().len() > 20, "{error:?}");
        }
    }
}
