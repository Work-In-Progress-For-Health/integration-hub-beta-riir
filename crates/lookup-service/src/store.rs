//! The storage contract for table definitions and rows.
//!
//! Implements `spec/20-lookup-service.md` §3.3.

use std::future::Future;

use crate::error::LookupError;
use crate::model::{Row, TableDefinition, TableRecord, UploadInfo};

/// What a replace did, for the upload response (`spec/20-lookup-service.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaceResult {
    /// Rows written (inserted or overwritten); each key is written once.
    pub upserted: usize,
    /// Rows that were in the table before and not in the upload, now deleted.
    pub removed: usize,
}

/// Storage for table definitions and rows (§3.3).
///
/// The methods return `impl Future + Send` rather than being `async fn` so
/// that the futures are `Send` and a handler holding a store can be spawned.
pub trait TableStore: Send + Sync + 'static {
    /// A short name reported in responses: `file`, `memory`, `cosmos`.
    ///
    /// A constant per implementation; it is how an operator tells from a
    /// response which backend answered.
    fn source(&self) -> &'static str;

    /// Whether the store accepts writes.
    ///
    /// When `false`, [`Self::replace_table`] and [`Self::update_definition`]
    /// must return `read_only` (`spec/20-lookup-service.md` §3.3) rather
    /// than silently drop the write; the pages use this flag to hide the
    /// forms that would lead there.
    fn writable(&self) -> bool;

    /// Connect, before the service reports ready (`spec/20-lookup-service.md`
    /// §5.1).
    ///
    /// # Errors
    ///
    /// A [`LookupError`] when the backend cannot be reached or read; the
    /// service then exits instead of serving from nothing.
    fn open(&self) -> impl Future<Output = Result<(), LookupError>> + Send;

    /// Release resources at shutdown. Infallible: there is nothing a caller
    /// could do about a failed close, so an implementation logs it instead.
    fn close(&self) -> impl Future<Output = ()> + Send;

    /// Every table's definition and stats.
    ///
    /// # Errors
    ///
    /// A [`LookupError`] when the backend fails.
    fn list_tables(&self) -> impl Future<Output = Result<Vec<TableRecord>, LookupError>> + Send;

    /// One table's record, or `None` when no such table exists (not an
    /// error: the caller chooses between `table_not_found` and carrying on).
    ///
    /// # Errors
    ///
    /// A [`LookupError`] when the backend fails.
    fn get_table(
        &self,
        table: &str,
    ) -> impl Future<Output = Result<Option<TableRecord>, LookupError>> + Send;

    /// Point read by canonical key. `None` for a missing row or table.
    ///
    /// `key` is already canonical (normalised per `spec/20-lookup-service.md`
    /// §4); the store compares it exactly.
    ///
    /// # Errors
    ///
    /// A [`LookupError`] when the backend fails; the resolver reports that
    /// as `backend_unavailable` without the error's text (§5.5).
    fn get_row(
        &self,
        table: &str,
        key: &[String],
    ) -> impl Future<Output = Result<Option<Row>, LookupError>> + Send;

    /// Every row of a table, for preloading (`spec/20-lookup-service.md`
    /// §5.2). Unordered; the caller indexes them.
    ///
    /// # Errors
    ///
    /// A [`LookupError`] when the backend fails.
    fn load_rows(&self, table: &str) -> impl Future<Output = Result<Vec<Row>, LookupError>> + Send;

    /// A page of rows ordered by key text, optionally filtered by a
    /// case-insensitive key substring, plus the total matching.
    ///
    /// The total ignores `offset` and `limit`, so a pager can show the
    /// number of pages. An unknown table is an empty page, not an error.
    ///
    /// # Errors
    ///
    /// A [`LookupError`] when the backend fails.
    fn query_rows(
        &self,
        table: &str,
        search: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> impl Future<Output = Result<(Vec<Row>, usize), LookupError>> + Send;

    /// Replace a table's definition and rows together (`spec/20-lookup-service.md`
    /// §3.3), so no reader sees the new definition over old rows.
    ///
    /// # Errors
    ///
    /// `read_only` from a store that is not [`Self::writable`]; otherwise a
    /// [`LookupError`] when the backend fails.
    fn replace_table(
        &self,
        definition: &TableDefinition,
        rows: &[Row],
        upload: &UploadInfo,
    ) -> impl Future<Output = Result<ReplaceResult, LookupError>> + Send;

    /// Replace a table's definition only, keeping rows and stats.
    ///
    /// # Errors
    ///
    /// `read_only` from a store that is not [`Self::writable`]; otherwise a
    /// [`LookupError`] when the backend fails.
    fn update_definition(
        &self,
        definition: &TableDefinition,
    ) -> impl Future<Output = Result<(), LookupError>> + Send;
}

/// The page of rows a query returns: ordered by key text, filtered, sliced.
///
/// Shared by the stores that hold rows in memory, so they cannot disagree
/// about what "ordered" and "contains" mean.
#[must_use]
pub fn page_of_rows<'a, I>(
    rows: I,
    search: Option<&str>,
    offset: usize,
    limit: usize,
) -> (Vec<Row>, usize)
where
    I: IntoIterator<Item = &'a Row>,
{
    let mut keyed: Vec<(String, &Row)> = rows
        .into_iter()
        .map(|row| (crate::keys::key_text(&row.key), row))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(search) = search.filter(|s| !s.is_empty()) {
        let needle = search.to_lowercase();
        keyed.retain(|(text, _)| text.to_lowercase().contains(&needle));
    }
    let total = keyed.len();
    let page = keyed
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(_, row)| row.clone())
        .collect();
    (page, total)
}
