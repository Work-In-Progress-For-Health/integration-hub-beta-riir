//! Replacing a table's data from an uploaded CSV, and changing its cache
//! settings. Shared by the API and the pages.
//!
//! Implements `spec/20-lookup-service.md` §6, §8.1–§8.3.

use std::time::Instant;

use serde_json::{Map, Value};

use crate::config::Config;
use crate::error::{Kind, LookupError};
use crate::import::{build_definition, decode_csv_bytes, import_rows, read_csv_default};
use crate::mapping::RecordMapping;
use crate::model::{
    MAX_TTL_SECONDS, TABLE_NAME_PATTERN, TableDefinition, UploadInfo, UploadResult,
    is_valid_table_name,
};
use crate::resolver::LookupResolver;
use crate::store::TableStore;

/// The longest recorded upload file name.
pub const MAX_FILE_NAME_LENGTH: usize = 200;

/// A request to replace a table from a CSV.
#[derive(Debug, Clone)]
pub struct UploadRequest {
    /// The table.
    pub table: String,
    /// The file's bytes.
    pub data: Vec<u8>,
    /// The file's name as the client sent it.
    pub file_name: Option<String>,
    /// `None` keeps the existing table's definition (§8.2).
    pub definition_settings: Option<Map<String, Value>>,
    /// `None` maps columns by name.
    pub mapping: Option<RecordMapping>,
    /// The authenticated principal's name, recorded as the uploader and in
    /// the log line (§12.6). Never a token or a claim other than the name.
    pub actor: String,
}

impl UploadRequest {
    /// A request with no definition and no mapping, made by `actor`.
    #[must_use]
    pub fn new(
        table: impl Into<String>,
        data: Vec<u8>,
        file_name: Option<String>,
        actor: impl Into<String>,
    ) -> Self {
        Self {
            table: table.into(),
            data,
            file_name,
            definition_settings: None,
            mapping: None,
            actor: actor.into(),
        }
    }
}

/// The store must accept writes (§8.1).
///
/// Whether the **caller** may write is decided before this, by the role
/// check of §12; the environment no longer matters (upstream `2a220e3`
/// removed `WRITES_ENABLED_ENVIRONMENTS`).
///
/// # Errors
///
/// `read_only` (409).
pub fn ensure_store_writable<S: TableStore>(
    resolver: &LookupResolver<S>,
) -> Result<(), LookupError> {
    if !resolver.store().writable() {
        return Err(LookupError::new(
            Kind::ReadOnly,
            "Tables are served from seed files; configure COSMOS_ENDPOINT to enable uploads",
        ));
    }
    Ok(())
}

/// Validate the whole file first, then replace the table. Nothing is written
/// if any row is invalid (§6.4).
///
/// # Errors
///
/// `read_only`, `invalid_upload`, `upload_too_large`, or a store failure.
pub async fn replace_table_from_csv<S: TableStore>(
    config: &Config,
    resolver: &LookupResolver<S>,
    request: UploadRequest,
) -> Result<UploadResult, LookupError> {
    ensure_store_writable(resolver)?;
    let table = request.table.as_str();
    if !is_valid_table_name(table) {
        return Err(LookupError::invalid_upload(format!(
            "Invalid table name; must match {TABLE_NAME_PATTERN}"
        )));
    }
    if request.data.len() > config.max_upload_bytes() {
        return Err(LookupError::new(
            Kind::UploadTooLarge,
            format!("File exceeds the {} MB upload limit", config.max_upload_mb),
        )
        .for_table(table));
    }

    let text = decode_csv_bytes(&request.data).map_err(|e| e.for_table(table))?;
    let (document, csv_errors) = read_csv_default(&text);
    if !csv_errors.is_empty() {
        return Err(
            LookupError::invalid_upload("The CSV file is invalid; nothing was changed")
                .for_table(table)
                .with_errors(csv_errors.iter().map(ToString::to_string).collect()),
        );
    }

    let created = !resolver.has_table(table);
    let definition = if let Some(settings) = &request.definition_settings {
        build_definition(table, &document.header, settings).map_err(|problems| {
            LookupError::invalid_upload("The table definition is invalid; nothing was changed")
                .for_table(table)
                .with_errors(problems)
        })?
    } else if created {
        return Err(LookupError::invalid_upload(format!(
            "Table '{table}' does not exist; supply a table definition to create it"
        ))
        .for_table(table));
    } else {
        resolver.definition(table)?
    };

    let result = import_rows(&document, &definition, request.mapping.as_ref());
    if !result.errors.is_empty() {
        return Err(LookupError::invalid_upload(format!(
            "{} problem(s) found; nothing was changed",
            result.errors.len()
        ))
        .for_table(table)
        .with_errors(result.error_messages()));
    }
    if result.rows.is_empty() {
        return Err(
            LookupError::invalid_upload("The file has no data rows; nothing was changed")
                .for_table(table),
        );
    }

    let started = Instant::now();
    let replaced = {
        let _guard = resolver.upload_lock().lock().await;
        let replaced = resolver
            .store()
            .replace_table(
                &definition,
                &result.rows,
                &UploadInfo {
                    actor: request.actor.clone(),
                    file_name: safe_file_name(request.file_name.as_deref()),
                },
            )
            .await?;
        resolver.refresh_table(table).await?;
        replaced
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        table = %table,
        rows = result.rows.len(),
        removed = replaced.removed,
        duration_ms,
        by = %request.actor,
        "Replaced table"
    );
    Ok(UploadResult {
        table: table.to_owned(),
        row_count: result.rows.len(),
        upserted: replaced.upserted,
        removed: replaced.removed,
        duration_ms,
        created,
    })
}

/// Settings a table may change without re-uploading (§8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsChange {
    /// `Some(None)` resets to the service default.
    pub ttl_seconds: Option<Option<u32>>,
    /// A new preload flag.
    pub preload: Option<bool>,
}

impl SettingsChange {
    /// Read a PATCH body: only `ttl_seconds` and `preload` are accepted, and
    /// only the fields present change.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a body that is not an object, an unknown field
    /// or a value of the wrong type or range.
    pub fn from_json(body: &Value) -> Result<Self, LookupError> {
        let Some(object) = body.as_object() else {
            return Err(LookupError::invalid_request(
                "The request body must be a JSON object",
            ));
        };
        let mut change = Self::default();
        let mut problems = Vec::new();
        for (name, value) in object {
            match name.as_str() {
                "ttl_seconds" => {
                    if value.is_null() {
                        change.ttl_seconds = Some(None);
                    } else if let Some(ttl) = value
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| (1..=MAX_TTL_SECONDS).contains(n))
                    {
                        change.ttl_seconds = Some(Some(ttl));
                    } else {
                        problems.push(format!(
                            "ttl_seconds: must be null or a whole number from 1 to {MAX_TTL_SECONDS}"
                        ));
                    }
                }
                "preload" => match value.as_bool() {
                    Some(flag) => change.preload = Some(flag),
                    None => problems.push("preload: must be true or false".to_owned()),
                },
                _ => problems.push(format!("{name}: unknown field")),
            }
        }
        if problems.is_empty() {
            Ok(change)
        } else {
            Err(LookupError::invalid_request("The request body is invalid").with_errors(problems))
        }
    }

    /// Apply to a definition.
    #[must_use]
    pub fn apply(&self, current: &TableDefinition) -> TableDefinition {
        let mut definition = current.clone();
        if let Some(ttl) = self.ttl_seconds {
            definition.ttl_seconds = ttl;
        }
        if let Some(preload) = self.preload {
            definition.preload = preload;
        }
        definition
    }
}

/// Change `ttl_seconds` and/or `preload` without touching rows;
/// `ttl_seconds = None` resets to the default (§8.3).
///
/// # Errors
///
/// `read_only`, `table_not_found`, `invalid_settings`, or a store failure.
pub async fn update_table_settings<S: TableStore>(
    resolver: &LookupResolver<S>,
    table: &str,
    change: SettingsChange,
    actor: &str,
) -> Result<(), LookupError> {
    ensure_store_writable(resolver)?;
    let _guard = resolver.upload_lock().lock().await;
    let current = resolver.definition(table)?;
    let definition = change.apply(&current);
    if let Err(problems) = definition.validate() {
        return Err(LookupError::invalid_settings(
            "The settings are invalid; nothing was changed",
            Some(table),
        )
        .with_errors(problems));
    }
    resolver.store().update_definition(&definition).await?;
    resolver.refresh_table(table).await?;
    tracing::info!(table = %table, by = %actor, "Updated table settings");
    Ok(())
}

/// The last path component of a client-supplied file name, bounded.
fn safe_file_name(name: Option<&str>) -> Option<String> {
    let name = name?.replace('\\', "/");
    let base = name.rsplit('/').next().unwrap_or_default();
    let bounded: String = base.chars().take(MAX_FILE_NAME_LENGTH).collect();
    if bounded.is_empty() {
        None
    } else {
        Some(bounded)
    }
}
