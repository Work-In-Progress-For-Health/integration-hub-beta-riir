//! A read-only store backed by CSV files.
//!
//! Implements `spec/20-lookup-service.md` §7.
//!
//! Each `<name>.csv` becomes a table. Without a manifest entry the first
//! column is the key and the remaining columns are values. `tables.json` can
//! override this per table, to declare composite keys, per-part
//! normalisation, TTL and preloading.
//!
//! Everything is validated and loaded once at startup into immutable maps, so
//! lookups take no lock, and any data problem stops the service starting
//! rather than surfacing as wrong substitutions later (§7.2).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::{LookupError, SeedDataError};
use crate::import::{build_definition, decode_csv_bytes, import_rows, read_csv_default};
use crate::mapping::name_list;
use crate::model::{Row, TableDefinition, TableRecord, TableStats, UploadInfo};
use crate::store::{ReplaceResult, TableStore, page_of_rows};

/// The manifest's file name.
pub const MANIFEST_NAME: &str = "tables.json";

/// The largest seed file, in bytes (§7.2).
pub const MAX_SEED_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// The same limit in megabytes, for messages (`RCA-EXC-1`: a constant).
#[expect(clippy::integer_division, reason = "RCA-EXC-1: constant expression")]
const MAX_SEED_FILE_MB: u64 = MAX_SEED_FILE_BYTES / (1024 * 1024);

/// The uploader recorded for seed data (§7.3).
pub const SEED_ACTOR: &str = "seed";

/// Fields a manifest entry may set (§7.1).
const MANIFEST_FIELDS: [&str; 7] = [
    "description",
    "key_columns",
    "value_columns",
    "default_value_column",
    "key_normalisation",
    "ttl_seconds",
    "preload",
];

/// Seed files and their manifest, wherever they were read from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedSource {
    /// The manifest's text, when there is one.
    pub manifest: Option<String>,
    /// (table name, file name, bytes), in name order.
    pub files: Vec<(String, String, Vec<u8>)>,
}

impl SeedSource {
    /// Read `*.csv` and `tables.json` from a directory.
    ///
    /// # Errors
    ///
    /// The directory is missing or a file in it cannot be read. A file over
    /// the size limit is refused here, before it is read into memory.
    pub fn from_dir(dir: &Path) -> Result<Self, SeedDataError> {
        if !dir.is_dir() {
            return Err(SeedDataError(format!(
                "Seed directory {} does not exist",
                dir.display()
            )));
        }
        let manifest_path = dir.join(MANIFEST_NAME);
        let manifest = if manifest_path.exists() {
            Some(std::fs::read_to_string(&manifest_path).map_err(|error| {
                SeedDataError(format!("{MANIFEST_NAME} could not be read: {error}"))
            })?)
        } else {
            None
        };
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .map_err(|error| SeedDataError(format!("Seed directory could not be read: {error}")))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "csv"))
            .collect();
        paths.sort();
        let mut files = Vec::new();
        for path in paths {
            let file_name = path
                .file_name()
                .map_or_default(|n| n.to_string_lossy().into_owned());
            let stem = path
                .file_stem()
                .map_or_default(|n| n.to_string_lossy().into_owned());
            // An unreadable size is an error, not a size of zero (RCA-P-3).
            let size = std::fs::metadata(&path)
                .map_err(|error| SeedDataError(format!("{file_name}: could not be read: {error}")))?
                .len();
            if size > MAX_SEED_FILE_BYTES {
                return Err(SeedDataError(format!(
                    "{file_name}: exceeds the {MAX_SEED_FILE_MB} MB seed file limit"
                )));
            }
            let bytes = std::fs::read(&path).map_err(|error| {
                SeedDataError(format!("{file_name}: could not be read: {error}"))
            })?;
            files.push((stem, file_name, bytes));
        }
        Ok(Self { manifest, files })
    }
}

/// One table loaded from a seed file.
#[derive(Debug, Clone)]
pub struct LoadedTable {
    /// The definition.
    pub definition: TableDefinition,
    /// Rows in file order.
    pub rows: Vec<Row>,
    /// Index of each canonical key into `rows`.
    index: HashMap<Vec<String>, usize>,
    /// The file it came from.
    pub source_file: String,
    /// When it was loaded, RFC 3339 in UTC.
    pub loaded_at: String,
}

impl LoadedTable {
    /// The record a store reports for it.
    fn record(&self) -> TableRecord {
        TableRecord {
            definition: self.definition.clone(),
            stats: TableStats {
                row_count: self.rows.len(),
                last_upload_at: Some(self.loaded_at.clone()),
                last_upload_file: Some(self.source_file.clone()),
                last_upload_by: Some(SEED_ACTOR.to_owned()),
            },
        }
    }
}

/// The current time as RFC 3339 in UTC.
#[must_use]
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// A read-only store over seed files (§3.3).
#[derive(Debug, Clone)]
pub struct FileTableStore {
    /// Tables by name.
    tables: BTreeMap<String, LoadedTable>,
}

impl FileTableStore {
    /// Validate and load every table.
    ///
    /// # Errors
    ///
    /// The first problem found (§7.2), naming the file and, for a row, the
    /// line, never a value.
    pub fn load(source: &SeedSource) -> Result<Self, SeedDataError> {
        let manifest = read_manifest(source.manifest.as_deref())?;
        let stems: BTreeSet<&str> = source
            .files
            .iter()
            .map(|(stem, _, _)| stem.as_str())
            .collect();
        let orphaned: Vec<&String> = manifest
            .keys()
            .filter(|name| !stems.contains(name.as_str()))
            .collect();
        if !orphaned.is_empty() {
            return Err(SeedDataError(format!(
                "{MANIFEST_NAME} declares tables with no CSV file: {}",
                name_list(&orphaned)
            )));
        }
        let empty = Map::new();
        let mut tables = BTreeMap::new();
        for (stem, file_name, bytes) in &source.files {
            let entry = manifest.get(stem).unwrap_or(&empty);
            tables.insert(stem.clone(), load_csv_table(stem, file_name, bytes, entry)?);
        }
        if tables.is_empty() {
            tracing::warn!("No seed CSV files found; serving no tables");
        }
        for table in tables.values() {
            tracing::info!(
                table = %table.definition.name,
                rows = table.rows.len(),
                file = %table.source_file,
                "Loaded seed table"
            );
        }
        Ok(Self { tables })
    }

    /// The loaded tables in name order.
    #[must_use]
    pub fn loaded_tables(&self) -> Vec<&LoadedTable> {
        self.tables.values().collect()
    }
}

/// Read `tables.json` (§7.1, §7.2).
fn read_manifest(
    text: Option<&str>,
) -> Result<BTreeMap<String, Map<String, Value>>, SeedDataError> {
    let Some(text) = text else {
        return Ok(BTreeMap::new());
    };
    let content: Value = serde_json::from_str(text)
        .map_err(|error| SeedDataError(format!("{MANIFEST_NAME} could not be read: {error}")))?;
    let tables = content
        .as_object()
        .and_then(|object| object.get("tables"))
        .and_then(Value::as_object);
    let shape = || {
        SeedDataError(format!(
            "{MANIFEST_NAME} must be an object of the form \
             {{\"tables\": {{\"<name>\": {{...}}}}}}"
        ))
    };
    let tables = tables.ok_or_else(shape)?;
    let mut out = BTreeMap::new();
    for (name, entry) in tables {
        out.insert(name.clone(), entry.as_object().ok_or_else(shape)?.clone());
    }
    Ok(out)
}

/// Load one CSV file as a table (§7).
fn load_csv_table(
    stem: &str,
    file_name: &str,
    bytes: &[u8],
    entry: &Map<String, Value>,
) -> Result<LoadedTable, SeedDataError> {
    if !u64::try_from(bytes.len()).is_ok_and(|length| length <= MAX_SEED_FILE_BYTES) {
        return Err(SeedDataError(format!(
            "{file_name}: exceeds the {MAX_SEED_FILE_MB} MB seed file limit"
        )));
    }
    validate_manifest_entry(stem, entry)?;
    let text = decode_csv_bytes(bytes)
        .map_err(|_| SeedDataError(format!("{file_name}: not valid UTF-8")))?;
    let (document, errors) = read_csv_default(&text);
    if let Some(first) = errors.first() {
        return Err(SeedDataError(format!("{file_name} {first}")));
    }
    let definition = build_definition(stem, &document.header, entry).map_err(|problems| {
        SeedDataError(format!(
            "{file_name}: invalid table definition: {}",
            problems.join("; ")
        ))
    })?;
    let result = import_rows(&document, &definition, None);
    if let Some(first) = result.errors.first() {
        return Err(SeedDataError(format!("{file_name} {first}")));
    }
    let index = result
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| (row.key.clone(), i))
        .collect();
    Ok(LoadedTable {
        definition,
        rows: result.rows,
        index,
        source_file: file_name.to_owned(),
        loaded_at: now_rfc3339(),
    })
}

/// Refuse an unknown field or a column list of the wrong shape (§7.2).
fn validate_manifest_entry(table: &str, entry: &Map<String, Value>) -> Result<(), SeedDataError> {
    let unknown: Vec<&String> = entry
        .keys()
        .filter(|name| !MANIFEST_FIELDS.contains(&name.as_str()))
        .collect();
    if !unknown.is_empty() {
        return Err(SeedDataError(format!(
            "{MANIFEST_NAME} entry for '{table}' has unknown fields {}",
            name_list(&unknown)
        )));
    }
    for field in ["key_columns", "value_columns"] {
        if let Some(configured) = entry.get(field)
            && !configured.is_null()
            && !configured
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        {
            return Err(SeedDataError(format!(
                "{MANIFEST_NAME} entry for '{table}': {field} must be a list of strings"
            )));
        }
    }
    Ok(())
}

// Nothing here awaits: the data is in memory. The trait is async for the
// stores that are not.
#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
impl TableStore for FileTableStore {
    fn source(&self) -> &'static str {
        "file"
    }

    fn writable(&self) -> bool {
        false
    }

    async fn open(&self) -> Result<(), LookupError> {
        Ok(())
    }

    async fn close(&self) {}

    async fn list_tables(&self) -> Result<Vec<TableRecord>, LookupError> {
        Ok(self.tables.values().map(LoadedTable::record).collect())
    }

    async fn get_table(&self, table: &str) -> Result<Option<TableRecord>, LookupError> {
        Ok(self.tables.get(table).map(LoadedTable::record))
    }

    async fn get_row(&self, table: &str, key: &[String]) -> Result<Option<Row>, LookupError> {
        let Some(loaded) = self.tables.get(table) else {
            return Ok(None);
        };
        let Some(position) = loaded.index.get(key) else {
            return Ok(None);
        };
        // An index entry with no row is a corrupt seed table (RCA-IN-6): an
        // error, not a miss.
        loaded
            .rows
            .get(*position)
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                LookupError::backend_unavailable(
                    "seed table index points past its rows",
                    Some(table),
                )
            })
    }

    async fn load_rows(&self, table: &str) -> Result<Vec<Row>, LookupError> {
        Ok(self.tables.get(table).map_or_default(|t| t.rows.clone()))
    }

    async fn query_rows(
        &self,
        table: &str,
        search: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<Row>, usize), LookupError> {
        Ok(self.tables.get(table).map_or((Vec::new(), 0), |t| {
            page_of_rows(&t.rows, search, offset, limit)
        }))
    }

    async fn replace_table(
        &self,
        definition: &TableDefinition,
        _rows: &[Row],
        _upload: &UploadInfo,
    ) -> Result<ReplaceResult, LookupError> {
        Err(LookupError::read_only(
            "Tables are served from seed files; configure COSMOS_ENDPOINT to enable uploads",
            &definition.name,
        ))
    }

    async fn update_definition(&self, definition: &TableDefinition) -> Result<(), LookupError> {
        Err(LookupError::read_only(
            "Tables are served from seed files; configure COSMOS_ENDPOINT to enable edits",
            &definition.name,
        ))
    }
}
