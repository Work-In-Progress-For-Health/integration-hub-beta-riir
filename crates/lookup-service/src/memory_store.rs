//! A writable store held in memory.
//!
//! Implements `spec/20-lookup-service.md` §3.3.
//!
//! It exists so the write paths (upload, settings, seed import) can run
//! end to end without Cosmos, which is not ported (§1.4). It is not
//! selectable by configuration: a store that loses its data on restart is a
//! test double, not a deployment option.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::error::LookupError;
use crate::file_store::now_rfc3339;
use crate::model::{Row, TableDefinition, TableRecord, TableStats, UploadInfo};
use crate::store::{ReplaceResult, TableStore, page_of_rows};

/// What one table holds.
#[derive(Debug, Clone)]
struct Held {
    /// Definition and stats.
    record: TableRecord,
    /// The rows.
    rows: Vec<Row>,
}

/// An in-memory writable store that counts point reads and can be made to
/// fail them.
#[derive(Debug)]
pub struct MemoryTableStore {
    /// Whether writes are accepted.
    writable: bool,
    /// Tables by name.
    tables: Mutex<BTreeMap<String, Held>>,
    /// Point reads made.
    get_row_calls: AtomicUsize,
    /// Whole-table loads made.
    load_rows_calls: AtomicUsize,
    /// Whether point reads fail.
    fail_reads: AtomicBool,
    /// Whether `open` was called.
    opened: AtomicBool,
    /// Whether `close` was called.
    closed: AtomicBool,
}

impl Default for MemoryTableStore {
    fn default() -> Self {
        Self::new(true)
    }
}

impl MemoryTableStore {
    /// An empty store.
    #[must_use]
    pub const fn new(writable: bool) -> Self {
        Self {
            writable,
            tables: Mutex::new(BTreeMap::new()),
            get_row_calls: AtomicUsize::new(0),
            load_rows_calls: AtomicUsize::new(0),
            fail_reads: AtomicBool::new(false),
            opened: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    /// Put a table in directly, as a store holding data already would.
    pub fn put_table(&self, definition: TableDefinition, rows: Vec<Row>) {
        let record = TableRecord {
            stats: TableStats {
                row_count: rows.len(),
                ..TableStats::default()
            },
            definition,
        };
        self.lock()
            .insert(record.definition.name.clone(), Held { record, rows });
    }

    /// A table's record, if present.
    #[must_use]
    pub fn record(&self, table: &str) -> Option<TableRecord> {
        self.lock().get(table).map(|held| held.record.clone())
    }

    /// A table's row by canonical key, if present.
    #[must_use]
    pub fn row(&self, table: &str, key: &[&str]) -> Option<Row> {
        self.lock()
            .get(table)
            .and_then(|held| held.rows.iter().find(|row| row.key == key).cloned())
    }

    /// Names of the tables held.
    #[must_use]
    pub fn table_names(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// Remove a table, as another replica might have.
    pub fn remove_table(&self, table: &str) {
        self.lock().remove(table);
    }

    /// Point reads made so far.
    #[must_use]
    pub fn get_row_calls(&self) -> usize {
        self.get_row_calls.load(Ordering::SeqCst)
    }

    /// Whole-table loads made so far.
    #[must_use]
    pub fn load_rows_calls(&self) -> usize {
        self.load_rows_calls.load(Ordering::SeqCst)
    }

    /// Make every point read fail with `backend_unavailable`.
    pub fn set_fail_reads(&self, fail: bool) {
        self.fail_reads.store(fail, Ordering::SeqCst);
    }

    /// Whether `open` was called.
    #[must_use]
    pub fn was_opened(&self) -> bool {
        self.opened.load(Ordering::SeqCst)
    }

    /// Whether `close` was called.
    #[must_use]
    pub fn was_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The lock; a poisoned one still holds consistent data because no
    /// critical section here can panic part-way.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Held>> {
        self.tables
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

// Nothing here awaits: the data is in memory. The trait is async for the
// stores that are not.
#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
impl TableStore for MemoryTableStore {
    fn source(&self) -> &'static str {
        "memory"
    }

    fn writable(&self) -> bool {
        self.writable
    }

    async fn open(&self) -> Result<(), LookupError> {
        self.opened.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    async fn list_tables(&self) -> Result<Vec<TableRecord>, LookupError> {
        Ok(self.lock().values().map(|h| h.record.clone()).collect())
    }

    async fn get_table(&self, table: &str) -> Result<Option<TableRecord>, LookupError> {
        Ok(self.record(table))
    }

    async fn get_row(&self, table: &str, key: &[String]) -> Result<Option<Row>, LookupError> {
        self.get_row_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(LookupError::backend_unavailable(
                "simulated outage",
                Some(table),
            ));
        }
        Ok(self
            .lock()
            .get(table)
            .and_then(|held| held.rows.iter().find(|row| row.key == key).cloned()))
    }

    async fn load_rows(&self, table: &str) -> Result<Vec<Row>, LookupError> {
        self.load_rows_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .lock()
            .get(table)
            .map(|held| held.rows.clone())
            .unwrap_or_default())
    }

    async fn query_rows(
        &self,
        table: &str,
        search: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<Row>, usize), LookupError> {
        Ok(self.lock().get(table).map_or((Vec::new(), 0), |held| {
            page_of_rows(&held.rows, search, offset, limit)
        }))
    }

    async fn replace_table(
        &self,
        definition: &TableDefinition,
        rows: &[Row],
        upload: &UploadInfo,
    ) -> Result<ReplaceResult, LookupError> {
        if !self.writable {
            return Err(LookupError::read_only("read-only", &definition.name));
        }
        let mut tables = self.lock();
        let previous: HashMap<&Vec<String>, ()> = tables
            .get(&definition.name)
            .map(|held| held.rows.iter().map(|row| (&row.key, ())).collect())
            .unwrap_or_default();
        let new_keys: std::collections::HashSet<&Vec<String>> =
            rows.iter().map(|row| &row.key).collect();
        let removed = previous
            .keys()
            .filter(|key| !new_keys.contains(**key))
            .count();
        let record = TableRecord {
            definition: definition.clone(),
            stats: TableStats {
                row_count: rows.len(),
                last_upload_at: Some(now_rfc3339()),
                last_upload_file: upload.file_name.clone(),
                last_upload_by: Some(upload.actor.clone()),
            },
        };
        tables.insert(
            definition.name.clone(),
            Held {
                record,
                rows: rows.to_vec(),
            },
        );
        Ok(ReplaceResult {
            upserted: rows.len(),
            removed,
        })
    }

    async fn update_definition(&self, definition: &TableDefinition) -> Result<(), LookupError> {
        if !self.writable {
            return Err(LookupError::read_only("read-only", &definition.name));
        }
        let mut tables = self.lock();
        let held = tables
            .get_mut(&definition.name)
            .ok_or_else(|| LookupError::table_not_found(&definition.name))?;
        held.record.definition = definition.clone();
        Ok(())
    }
}
