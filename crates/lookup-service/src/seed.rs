//! Importing seed tables into a writable store.
//!
//! Implements `spec/20-lookup-service.md` §7.3.

use std::collections::BTreeSet;

use crate::config::SeedLocation;
use crate::error::SeedDataError;
use crate::file_store::{FileTableStore, SEED_ACTOR, SeedSource};
use crate::model::UploadInfo;
use crate::store::TableStore;

/// Import seed tables that do not exist in a writable store yet.
///
/// Existing tables are never overwritten, so uploads made through the GUI
/// survive restarts and redeploys. The seed files are fully validated first;
/// a bad seed file stops startup.
///
/// # Errors
///
/// A seed problem (§7.2), or the store's own failure rendered as one.
pub async fn import_missing_seed_tables<S: TableStore>(
    store: &S,
    location: &SeedLocation,
) -> Result<Vec<String>, SeedDataError> {
    if !store.writable() {
        return Ok(Vec::new());
    }
    let seed = match location {
        // Deployed environments: nothing is written at start-up, so their
        // tables come only from uploads (upstream `2a220e3`).
        SeedLocation::None => {
            tracing::info!("LOOKUP_SEED_DIR not set; no seed tables imported");
            return Ok(Vec::new());
        }
        SeedLocation::Dir(dir) if !dir.is_dir() => {
            tracing::warn!("Seed directory not found; skipping seed import");
            return Ok(Vec::new());
        }
        SeedLocation::Dir(dir) => SeedSource::from_dir(dir)?,
    };
    let seeds = FileTableStore::load(&seed)?;
    let existing: BTreeSet<String> = store
        .list_tables()
        .await
        .map_err(|error| SeedDataError(format!("could not list tables: {error}")))?
        .into_iter()
        .map(|record| record.definition.name)
        .collect();
    let mut imported = Vec::new();
    for table in seeds.loaded_tables() {
        let name = &table.definition.name;
        if existing.contains(name) {
            continue;
        }
        store
            .replace_table(
                &table.definition,
                &table.rows,
                &UploadInfo {
                    actor: SEED_ACTOR.to_owned(),
                    file_name: Some(table.source_file.clone()),
                },
            )
            .await
            .map_err(|error| SeedDataError(format!("could not import a seed table: {error}")))?;
        tracing::info!(table = %name, rows = table.rows.len(), "Seeded table");
        imported.push(name.clone());
    }
    Ok(imported)
}
