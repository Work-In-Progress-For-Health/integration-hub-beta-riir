//! Table definitions, rows and the shapes the API returns.
//!
//! Implements `spec/20-lookup-service.md` §3.

use std::collections::BTreeMap;

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};

/// The longest TTL a table may ask for: seven days (§3.1).
pub const MAX_TTL_SECONDS: u32 = 7 * 24 * 3600;

/// The longest description (§3.1).
pub const MAX_DESCRIPTION_CHARS: usize = 500;

/// What a table name must match, for messages (§3.1).
pub const TABLE_NAME_PATTERN: &str = "^[a-z0-9][a-z0-9_-]{0,63}$";

/// What a column name must match, for messages (§3.1).
pub const COLUMN_NAME_PATTERN: &str = "^[A-Za-z_][A-Za-z0-9_]{0,63}$";

/// Whether `name` matches [`TABLE_NAME_PATTERN`].
#[must_use]
pub fn is_valid_table_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Whether `name` matches [`COLUMN_NAME_PATTERN`].
#[must_use]
pub fn is_valid_column_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The case rule of a key part (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Case {
    /// Leave the case alone.
    #[default]
    Preserve,
    /// Upper-case.
    Upper,
    /// Lower-case.
    Lower,
}

impl Case {
    /// The spelling in JSON and in forms.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Upper => "upper",
            Self::Lower => "lower",
        }
    }

    /// Parse the spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "preserve" => Some(Self::Preserve),
            "upper" => Some(Self::Upper),
            "lower" => Some(Self::Lower),
            _ => None,
        }
    }
}

impl Serialize for Case {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Normalisation applied to one key part on load and on lookup, so both sides
/// compare equal (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct KeyPartRule {
    /// Trim surrounding whitespace.
    pub trim: bool,
    /// Case rule.
    pub case: Case,
}

impl Default for KeyPartRule {
    fn default() -> Self {
        Self {
            trim: true,
            case: Case::Preserve,
        }
    }
}

impl KeyPartRule {
    /// Read a rule from JSON, refusing any field but `trim` and `case`.
    ///
    /// # Errors
    ///
    /// One message per problem, naming the field and never the value.
    pub fn from_json(value: &serde_json::Value, path: &str) -> Result<Self, Vec<String>> {
        let Some(object) = value.as_object() else {
            return Err(vec![format!("{path}: must be an object")]);
        };
        let mut rule = Self::default();
        let mut problems = Vec::new();
        for (name, field) in object {
            match name.as_str() {
                "trim" => match field.as_bool() {
                    Some(trim) => rule.trim = trim,
                    None => problems.push(format!("{path}.trim: must be true or false")),
                },
                "case" => match field.as_str().and_then(Case::parse) {
                    Some(case) => rule.case = case,
                    None => problems.push(format!(
                        "{path}.case: must be one of preserve, upper, lower"
                    )),
                },
                _ => problems.push(format!("{path}.{name}: unknown field")),
            }
        }
        if problems.is_empty() {
            Ok(rule)
        } else {
            Err(problems)
        }
    }
}

/// A table's definition (§3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDefinition {
    /// The table name.
    pub name: String,
    /// A free-text description.
    pub description: String,
    /// Key columns, in key order.
    pub key_columns: Vec<String>,
    /// Value columns.
    pub value_columns: Vec<String>,
    /// The value column `value` reports.
    pub default_value_column: String,
    /// Rules per key column; a column with none uses the default rule.
    pub key_normalisation: BTreeMap<String, KeyPartRule>,
    /// `None` means the service default.
    pub ttl_seconds: Option<u32>,
    /// Hold the whole table in memory (§5.2).
    pub preload: bool,
}

impl TableDefinition {
    /// The rule for a key column.
    #[must_use]
    pub fn rule_for(&self, key_column: &str) -> KeyPartRule {
        self.key_normalisation
            .get(key_column)
            .copied()
            .unwrap_or_default()
    }

    /// Check every rule of §3.1.
    ///
    /// # Errors
    ///
    /// Every violation, as `<field>: <reason>`, never with the offending
    /// value.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        if !is_valid_table_name(&self.name) {
            problems.push(format!("name: must match {TABLE_NAME_PATTERN}"));
        }
        if self.description.chars().count() > MAX_DESCRIPTION_CHARS {
            problems.push(format!(
                "description: must be at most {MAX_DESCRIPTION_CHARS} characters"
            ));
        }
        if self.key_columns.is_empty() {
            problems.push("key_columns: must have at least one column".to_owned());
        }
        if self.value_columns.is_empty() {
            problems.push("value_columns: must have at least one column".to_owned());
        }
        let all: Vec<&String> = self.key_columns.iter().chain(&self.value_columns).collect();
        for column in &all {
            if !is_valid_column_name(column) {
                problems.push(format!(
                    "invalid column name '{column}': must match {COLUMN_NAME_PATTERN}"
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        if !all.iter().all(|column| seen.insert(*column)) {
            problems.push(
                "column names must be unique across key_columns and value_columns".to_owned(),
            );
        }
        if !self.value_columns.contains(&self.default_value_column) {
            problems.push(format!(
                "default_value_column '{}' is not a value column",
                self.default_value_column
            ));
        }
        let unknown: Vec<&String> = self
            .key_normalisation
            .keys()
            .filter(|column| !self.key_columns.contains(column))
            .collect();
        if !unknown.is_empty() {
            problems.push(format!(
                "key_normalisation refers to non-key columns: {}",
                crate::mapping::name_list(&unknown)
            ));
        }
        if let Some(ttl) = self.ttl_seconds
            && !(1..=MAX_TTL_SECONDS).contains(&ttl)
        {
            problems.push(format!("ttl_seconds: must be from 1 to {MAX_TTL_SECONDS}"));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

/// Housekeeping a store keeps beside a definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableStats {
    /// Rows in the table.
    pub row_count: usize,
    /// When the table was last replaced, RFC 3339 in UTC.
    pub last_upload_at: Option<String>,
    /// The file name of the last upload.
    pub last_upload_file: Option<String>,
    /// Who made it.
    pub last_upload_by: Option<String>,
}

/// A definition plus its stats, as a store holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRecord {
    /// The definition.
    pub definition: TableDefinition,
    /// The stats.
    pub stats: TableStats,
}

/// A row's values, in value-column order (§3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Values(Vec<(String, String)>);

impl Values {
    /// Build from pairs, keeping their order.
    #[must_use]
    pub const fn new(pairs: Vec<(String, String)>) -> Self {
        Self(pairs)
    }

    /// The value of a column.
    #[must_use]
    pub fn get(&self, column: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name == column)
            .map(|(_, value)| value.as_str())
    }

    /// Every pair, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl Serialize for Values {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

/// One row: a canonical key and its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The canonical key (§4.1).
    pub key: Vec<String>,
    /// The values.
    pub values: Values,
}

/// Who uploaded what, recorded in a table's stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadInfo {
    /// The actor; `unauthenticated` until Phase 2.
    pub actor: String,
    /// The uploaded file's name.
    pub file_name: Option<String>,
}

/// The `GET /api/v1/lookup/{table}` body (§8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookupResult {
    /// The table.
    pub table: String,
    /// The canonical key.
    pub key: Vec<String>,
    /// The default value column's value.
    pub value: String,
    /// Every value.
    pub values: Values,
    /// The store's `source`.
    pub source: String,
    /// Whether it came from memory (§5.3).
    pub cached: bool,
}

/// How a table is cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// The whole table is in memory.
    Preloaded,
    /// A TTL cache.
    Ttl,
}

impl Serialize for CacheMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Preloaded => "preloaded",
            Self::Ttl => "ttl",
        })
    }
}

/// Cache statistics of one table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheStats {
    /// Preloaded or TTL.
    pub mode: CacheMode,
    /// The TTL, for a TTL cache.
    pub ttl_seconds: Option<u32>,
    /// Entries held.
    pub entries: usize,
    /// Hits since the cache was built.
    pub hits: u64,
    /// Misses since the cache was built.
    pub misses: u64,
}

/// One table in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableSummary {
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Key columns.
    pub key_columns: Vec<String>,
    /// Value columns.
    pub value_columns: Vec<String>,
    /// Default value column.
    pub default_value_column: String,
    /// Rows.
    pub row_count: usize,
    /// The store's `source`.
    pub source: String,
    /// Preloaded?
    pub preload: bool,
    /// The effective TTL.
    pub ttl_seconds: u32,
    /// When last replaced.
    pub last_upload_at: Option<String>,
    /// Last uploaded file.
    pub last_upload_file: Option<String>,
    /// Last uploader.
    pub last_upload_by: Option<String>,
    /// When this replica loaded the table.
    pub loaded_at: Option<String>,
    /// Cache statistics.
    pub cache: CacheStats,
}

/// One table with its normalisation rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableDetail {
    /// The summary, flattened.
    #[serde(flatten)]
    pub summary: TableSummary,
    /// Rules per key column.
    pub key_normalisation: BTreeMap<String, KeyPartRule>,
}

/// One row as the API returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RowOut {
    /// Canonical key.
    pub key: Vec<String>,
    /// Values.
    pub values: Values,
}

/// A page of rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RowsPage {
    /// The table.
    pub table: String,
    /// Rows matching, over all pages.
    pub total: usize,
    /// Rows skipped.
    pub offset: usize,
    /// Page size.
    pub limit: usize,
    /// The filter, when there was one.
    pub search: Option<String>,
    /// The rows.
    pub rows: Vec<RowOut>,
}

/// The `201` body of an upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UploadResult {
    /// The table.
    pub table: String,
    /// Rows in the new table.
    pub row_count: usize,
    /// Rows written.
    pub upserted: usize,
    /// Stale rows deleted.
    pub removed: usize,
    /// How long the replace took.
    pub duration_ms: u64,
    /// Whether the table is new.
    pub created: bool,
}
