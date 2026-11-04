//! Lookup orchestration: the in-memory table registry, preloaded snapshots,
//! TTL caches and the fall-back to the store.
//!
//! Implements `spec/20-lookup-service.md` §5.
//!
//! Table definitions are held in memory (loaded at startup, refreshed on this
//! replica after an upload), so a lookup never needs a definition read.
//! Preloaded tables are served from an immutable snapshot that is swapped
//! whole on refresh; other tables go cache, then store point read, then
//! cache. Cross-replica invalidation is the Python's Phase 4 (§1.1).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use crate::cache::{TableCache, Timer, monotonic_timer};
use crate::error::{Kind, LookupError};
use crate::keys::canonical_key;
use crate::model::{
    CacheMode, CacheStats, LookupResult, Row, TableDefinition, TableDetail, TableRecord,
    TableSummary,
};
use crate::store::TableStore;

/// The prefix of a named key query or form parameter (§4.3).
pub const NAMED_KEY_PREFIX: &str = "key.";

/// Collect `key.<part>=value` parameters, rejecting a part given twice
/// (§4.3).
///
/// # Errors
///
/// `invalid_key` when a part is supplied more than once.
pub fn named_key_params<'a, I>(
    table: &str,
    params: I,
) -> Result<BTreeMap<String, String>, LookupError>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut named = BTreeMap::new();
    for (name, value) in params {
        let Some(part) = name.strip_prefix(NAMED_KEY_PREFIX) else {
            continue;
        };
        if named.insert(part.to_owned(), value.to_owned()).is_some() {
            return Err(LookupError::invalid_key(
                format!("Key part '{part}' supplied more than once"),
                table,
            ));
        }
    }
    Ok(named)
}

/// What the resolver holds for one table.
struct TableState {
    /// Definition and stats.
    record: TableRecord,
    /// When this replica loaded it, RFC 3339.
    loaded_at: String,
    /// The whole table, for a preloaded one (§5.2).
    snapshot: Option<HashMap<Vec<String>, Row>>,
    /// The TTL cache, for any other.
    cache: Option<Mutex<TableCache>>,
}

/// Resolves lookups against a store (§5).
pub struct LookupResolver<S> {
    /// Where rows come from.
    store: Arc<S>,
    /// `LOOKUP_DEFAULT_TTL_SECONDS`.
    default_ttl_seconds: u32,
    /// `LOOKUP_CACHE_MAX_ENTRIES`.
    max_entries: usize,
    /// The clock the caches use.
    timer: Timer,
    /// The registry, by table name.
    tables: RwLock<HashMap<String, Arc<TableState>>>,
    /// Uploads are rare; serialising them on a replica stops two replaces of
    /// one table interleaving (§5.6).
    upload_lock: tokio::sync::Mutex<()>,
}

impl<S: TableStore> LookupResolver<S> {
    /// A resolver with the real clock.
    #[must_use]
    pub fn new(store: Arc<S>, default_ttl_seconds: u32, max_entries: usize) -> Self {
        Self::with_timer(store, default_ttl_seconds, max_entries, monotonic_timer())
    }

    /// A resolver with an injected clock, for tests that move time.
    #[must_use]
    pub fn with_timer(
        store: Arc<S>,
        default_ttl_seconds: u32,
        max_entries: usize,
        timer: Timer,
    ) -> Self {
        Self {
            store,
            default_ttl_seconds,
            max_entries,
            timer,
            tables: RwLock::new(HashMap::new()),
            upload_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The store.
    #[must_use]
    pub const fn store(&self) -> &Arc<S> {
        &self.store
    }

    /// Serialises uploads and settings changes (§5.6).
    pub const fn upload_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.upload_lock
    }

    /// Load every table definition and preload snapshots (§5.1).
    ///
    /// # Errors
    ///
    /// A store failure.
    pub async fn start(&self) -> Result<(), LookupError> {
        let mut tables = HashMap::new();
        for record in self.store.list_tables().await? {
            let name = record.definition.name.clone();
            tables.insert(name, Arc::new(self.build_state(record).await?));
        }
        *self.tables_mut() = tables;
        Ok(())
    }

    /// Re-read one table after it changed: new definition, fresh cache or
    /// snapshot (§5.4).
    ///
    /// # Errors
    ///
    /// A store failure.
    pub async fn refresh_table(&self, table: &str) -> Result<(), LookupError> {
        match self.store.get_table(table).await? {
            None => {
                self.tables_mut().remove(table);
            }
            Some(record) => {
                let state = Arc::new(self.build_state(record).await?);
                self.tables_mut().insert(table.to_owned(), state);
            }
        }
        Ok(())
    }

    /// Whether the table is known.
    #[must_use]
    pub fn has_table(&self, table: &str) -> bool {
        self.tables_ref().contains_key(table)
    }

    /// A table's definition.
    ///
    /// # Errors
    ///
    /// `table_not_found`.
    pub fn definition(&self, table: &str) -> Result<TableDefinition, LookupError> {
        Ok(self.state(table)?.record.definition.clone())
    }

    /// Resolve a key (§5.2).
    ///
    /// # Errors
    ///
    /// `table_not_found`, `invalid_key`, `key_not_found` (which does not
    /// name the key, §4.4) or a store failure.
    pub async fn lookup(
        &self,
        table: &str,
        positional: &[String],
        named: &BTreeMap<String, String>,
    ) -> Result<LookupResult, LookupError> {
        let state = self.state(table)?;
        let definition = &state.record.definition;
        let key = canonical_key(definition, positional, named)?;

        let mut cached = true;
        let row = if let Some(snapshot) = &state.snapshot {
            snapshot.get(&key).cloned()
        } else if let Some(cache) = &state.cache {
            let hit = lock(cache).get(&key);
            if hit.is_some() {
                hit
            } else {
                cached = false;
                let row = self.store.get_row(table, &key).await?;
                if let Some(row) = &row {
                    lock(cache).put(key.clone(), row.clone());
                }
                row
            }
        } else {
            return Err(LookupError::new(
                Kind::BackendUnavailable,
                "table has neither a snapshot nor a cache",
            ));
        };

        // Deliberately no key values in the message: it can end up in
        // caller logs (§4.4).
        let row = row.ok_or_else(|| {
            LookupError::new(
                Kind::KeyNotFound,
                format!("No row for the supplied key in table '{table}'"),
            )
            .for_table(table)
        })?;
        let value = row
            .values
            .get(&definition.default_value_column)
            .unwrap_or_default()
            .to_owned();
        Ok(LookupResult {
            table: table.to_owned(),
            key,
            value,
            values: row.values,
            source: self.store.source().to_owned(),
            cached,
        })
    }

    /// Every table, by name.
    #[must_use]
    pub fn summaries(&self) -> Vec<TableSummary> {
        let tables = self.tables_ref();
        let mut states: Vec<&Arc<TableState>> = tables.values().collect();
        states.sort_by(|a, b| a.record.definition.name.cmp(&b.record.definition.name));
        states.into_iter().map(|s| self.summary(s)).collect()
    }

    /// One table with its normalisation rules.
    ///
    /// # Errors
    ///
    /// `table_not_found`.
    pub fn detail(&self, table: &str) -> Result<TableDetail, LookupError> {
        let state = self.state(table)?;
        Ok(TableDetail {
            summary: self.summary(&state),
            key_normalisation: state.record.definition.key_normalisation.clone(),
        })
    }

    /// A table's state, or `table_not_found`.
    fn state(&self, table: &str) -> Result<Arc<TableState>, LookupError> {
        self.tables_ref()
            .get(table)
            .cloned()
            .ok_or_else(|| LookupError::table_not_found(table))
    }

    /// The effective TTL of a table (§5.3).
    fn ttl_for(&self, definition: &TableDefinition) -> u32 {
        definition.ttl_seconds.unwrap_or(self.default_ttl_seconds)
    }

    /// Build the in-memory state for a table.
    async fn build_state(&self, record: TableRecord) -> Result<TableState, LookupError> {
        let loaded_at = crate::file_store::now_rfc3339();
        if record.definition.preload {
            let rows = self.store.load_rows(&record.definition.name).await?;
            let snapshot = rows.into_iter().map(|row| (row.key.clone(), row)).collect();
            return Ok(TableState {
                record,
                loaded_at,
                snapshot: Some(snapshot),
                cache: None,
            });
        }
        let cache = TableCache::new(
            self.ttl_for(&record.definition),
            self.max_entries,
            Arc::clone(&self.timer),
        );
        Ok(TableState {
            record,
            loaded_at,
            snapshot: None,
            cache: Some(Mutex::new(cache)),
        })
    }

    /// The summary of one state.
    fn summary(&self, state: &TableState) -> TableSummary {
        let definition = &state.record.definition;
        let upload = &state.record.stats;
        let cache = if let Some(snapshot) = &state.snapshot {
            CacheStats {
                mode: CacheMode::Preloaded,
                ttl_seconds: None,
                entries: snapshot.len(),
                hits: 0,
                misses: 0,
            }
        } else if let Some(cache) = &state.cache {
            let mut cache = lock(cache);
            CacheStats {
                mode: CacheMode::Ttl,
                ttl_seconds: Some(cache.ttl_seconds()),
                entries: cache.len(),
                hits: cache.hits(),
                misses: cache.misses(),
            }
        } else {
            CacheStats {
                mode: CacheMode::Ttl,
                ttl_seconds: None,
                entries: 0,
                hits: 0,
                misses: 0,
            }
        };
        TableSummary {
            name: definition.name.clone(),
            description: definition.description.clone(),
            key_columns: definition.key_columns.clone(),
            value_columns: definition.value_columns.clone(),
            default_value_column: definition.default_value_column.clone(),
            row_count: upload.row_count,
            source: self.store.source().to_owned(),
            preload: definition.preload,
            ttl_seconds: self.ttl_for(definition),
            last_upload_at: upload.last_upload_at.clone(),
            last_upload_file: upload.last_upload_file.clone(),
            last_upload_by: upload.last_upload_by.clone(),
            loaded_at: state.loaded_at.clone(),
            cache,
        }
    }

    /// The registry for reading. A poisoned lock still holds a consistent
    /// map: every write is a single insert or remove.
    fn tables_ref(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Arc<TableState>>> {
        self.tables
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The registry for writing.
    fn tables_mut(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Arc<TableState>>> {
        self.tables
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Lock a cache; a poisoned one is still a valid cache.
fn lock(cache: &Mutex<TableCache>) -> std::sync::MutexGuard<'_, TableCache> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
