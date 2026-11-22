//! Flow source servers: the managed list behind the Network Test picker.
//!
//! Implements `spec/15-dashboard.md` §10.4.5–§10.4.8 (upstream `326b3ec`).
//!
//! # Identity is `url:port`
//!
//! §10.4.6. The document id is derived from the key, and a source is
//! *created*, never upserted, so the store itself refuses a duplicate.
//! Checking first and writing second would let two people adding the same
//! server at once both succeed.

use serde_json::Value;

use crate::alarm::documents::FLOW_SOURCE_SERVER;
use crate::alarm::store::{AlarmStore, StoreError};
use crate::csv;
use crate::network_test::{validate_host, validate_port};

/// The Cosmos partition (§10.4.5).
pub const PARTITION: &str = "flow-source-server";

const MAX_DESCRIPTION_LENGTH: usize = 200;
const NOT_CONFIGURED: &str = "Source persistence is not configured.";
const UNAVAILABLE: &str = "Source persistence is currently unavailable.";

/// Why a source operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// Validation or a duplicate. Safe to show.
    Invalid(String),
    /// Nothing could be stored. Safe to show.
    Persistence(String),
}

impl SourceError {
    /// The message for a response body.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Invalid(m) | Self::Persistence(m) => m,
        }
    }
}

fn persistence(error: &StoreError) -> SourceError {
    tracing::error!(%error, "flow source persistence failed");
    SourceError::Persistence(UNAVAILABLE.to_string())
}

/// A source, as the API presents it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Source {
    /// `source_id` in storage.
    pub id: String,
    /// What an operator reads.
    pub description: String,
    /// A host.
    pub url: String,
    /// A port.
    pub port: u16,
}

/// A source plus the storage id it lives under.
#[derive(Debug, Clone)]
struct Stored {
    source: Source,
    storage_id: String,
}

fn key(url: &str, port: u16) -> (String, u16) {
    (url.to_lowercase(), port)
}

fn storage_id(url: &str, port: u16) -> String {
    format!("flow-source:{}:{port}", url.to_lowercase())
}

fn validated(
    description: &str,
    url: &str,
    port: &Value,
) -> Result<(String, String, u16), SourceError> {
    let url = validate_host(url).map_err(|e| SourceError::Invalid(e.to_string()))?;
    let port = validate_port(port).map_err(|e| SourceError::Invalid(e.to_string()))?;
    let description = description.trim();
    if description.is_empty() {
        return Err(SourceError::Invalid(
            "Description must not be empty".to_string(),
        ));
    }
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Err(SourceError::Invalid(format!(
            "Description is too long (max {MAX_DESCRIPTION_LENGTH} characters)"
        )));
    }
    Ok((description.to_string(), url, port))
}

fn require<S: AlarmStore>(store: &S) -> Result<(), SourceError> {
    if store.is_enabled() {
        Ok(())
    } else {
        Err(SourceError::Persistence(NOT_CONFIGURED.to_string()))
    }
}

async fn stored<S: AlarmStore>(store: &S) -> Result<Vec<Stored>, SourceError> {
    let mut all: Vec<Stored> = store
        .documents(PARTITION)
        .await
        .map_err(|e| persistence(&e))?
        .iter()
        .filter_map(|d| {
            let id = d.get("source_id")?.as_str()?.to_string();
            let url = d.get("url")?.as_str()?.to_string();
            let port = u16::try_from(d.get("port")?.as_u64()?).ok()?;
            let storage_id = d
                .get("storage_id")
                .and_then(Value::as_str)
                .map_or_else(|| id.clone(), ToString::to_string);
            Some(Stored {
                source: Source {
                    id,
                    description: d
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    url,
                    port,
                },
                storage_id,
            })
        })
        .collect();
    all.sort_by_key(|s| s.source.description.to_lowercase());
    Ok(all)
}

fn document(source: &Source, storage_id: &str) -> Value {
    serde_json::json!({
        "source_id": source.id,
        "storage_id": storage_id,
        "description": source.description,
        "url": source.url,
        "port": source.port,
    })
}

/// Every source, sorted by description.
///
/// # Errors
///
/// [`SourceError::Persistence`] — not an empty list.
pub async fn list<S: AlarmStore>(store: &S) -> Result<Vec<Source>, SourceError> {
    Ok(stored(store).await?.into_iter().map(|s| s.source).collect())
}

async fn create<S: AlarmStore>(store: &S, source: &Source) -> Result<String, SourceError> {
    let id = storage_id(&source.url, source.port);
    let made = store
        .create_document(PARTITION, &id, FLOW_SOURCE_SERVER, &document(source, &id))
        .await
        .map_err(|e| persistence(&e))?;
    if made {
        Ok(id)
    } else {
        Err(SourceError::Invalid(format!(
            "A source for {}:{} already exists",
            source.url, source.port
        )))
    }
}

/// Add a source (§10.4.6).
///
/// # Errors
///
/// [`SourceError`].
pub async fn add<S: AlarmStore>(
    store: &S,
    description: &str,
    url: &str,
    port: &Value,
) -> Result<Source, SourceError> {
    let (description, url, port) = validated(description, url, port)?;
    require(store)?;
    let source = Source {
        id: uuid::Uuid::new_v4().to_string(),
        description,
        url,
        port,
    };
    create(store, &source).await?;
    Ok(source)
}

/// Overwrite a source (§10.4.6).
///
/// # Errors
///
/// [`SourceError`]; "Source not found." for an unknown id.
pub async fn update<S: AlarmStore>(
    store: &S,
    id: &str,
    description: &str,
    url: &str,
    port: &Value,
) -> Result<Source, SourceError> {
    let (description, url, port) = validated(description, url, port)?;
    require(store)?;
    let existing = stored(store)
        .await?
        .into_iter()
        .find(|s| s.source.id == id)
        .ok_or_else(|| SourceError::Invalid("Source not found.".to_string()))?;

    let source = Source {
        id: id.to_string(),
        description,
        url,
        port,
    };

    if key(&existing.source.url, existing.source.port) == key(&source.url, source.port) {
        store
            .upsert_document(
                PARTITION,
                &existing.storage_id,
                FLOW_SOURCE_SERVER,
                &document(&source, &existing.storage_id),
            )
            .await
            .map_err(|e| persistence(&e))?;
        return Ok(source);
    }

    // New key: create the new document first, then remove the old, and put
    // things back if the removal fails -- never leave both.
    let new_id = create(store, &source).await?;
    if let Err(error) = store.delete_document(PARTITION, &existing.storage_id).await {
        let _ = store.delete_document(PARTITION, &new_id).await;
        return Err(persistence(&error));
    }
    Ok(source)
}

/// Delete a source. One that is not there is a no-op.
///
/// # Errors
///
/// [`SourceError::Persistence`].
pub async fn delete<S: AlarmStore>(store: &S, id: &str) -> Result<(), SourceError> {
    let Some(found) = stored(store).await?.into_iter().find(|s| s.source.id == id) else {
        return Ok(());
    };
    store
        .delete_document(PARTITION, &found.storage_id)
        .await
        .map_err(|e| persistence(&e))
}

/// What an import did (§10.4.8).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImportResult {
    /// Sources added.
    pub imported: usize,
    /// One line per problem.
    pub errors: Vec<String>,
    /// A write failed, so the import stopped.
    pub persistence_failed: bool,
}

/// Parse a CSV into validated rows and the errors of the rest.
#[must_use]
pub fn parse_csv(content: &str) -> (Vec<(String, String, u16)>, Vec<String>) {
    let Ok(table) = csv::parse(content) else {
        return (Vec::new(), vec!["CSV file is empty.".to_string()]);
    };
    let missing: Vec<&str> = ["description", "url", "port"]
        .into_iter()
        .filter(|c| !table.headers.iter().any(|h| h == c))
        .collect();
    if !missing.is_empty() {
        return (
            Vec::new(),
            vec![format!(
                "CSV header is missing required column(s): {}",
                missing.join(", ")
            )],
        );
    }

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let mut seen: std::collections::BTreeMap<(String, u16), usize> =
        std::collections::BTreeMap::new();
    for (index, row) in table.rows.iter().enumerate() {
        let number = crate::counting::row_number(index);
        let cell = |c: &str| row.get(c).cloned().unwrap_or_default();
        match validated(
            &cell("description"),
            &cell("url"),
            &Value::String(cell("port")),
        ) {
            Err(e) => errors.push(format!("Row {number}: {}", e.message())),
            Ok((description, url, port)) => {
                let k = key(&url, port);
                if let Some(first) = seen.get(&k) {
                    errors.push(format!(
                        "Row {number}: duplicate of row {first} ({url}:{port})"
                    ));
                    continue;
                }
                seen.insert(k, number);
                rows.push((description, url, port));
            }
        }
    }
    (rows, errors)
}

/// Import a CSV (§10.4.8).
pub async fn import<S: AlarmStore>(store: &S, content: &str) -> ImportResult {
    let (rows, mut errors) = parse_csv(content);
    if let Err(e) = require(store) {
        errors.push(e.message().to_string());
        return ImportResult {
            imported: 0,
            errors,
            persistence_failed: true,
        };
    }

    let mut result = ImportResult::default();
    for (description, url, port) in rows {
        let label = format!("{description} ({url}:{port})");
        match add(store, &description, &url, &Value::from(port)).await {
            Ok(_) => result.imported = crate::counting::next(result.imported),
            Err(SourceError::Invalid(m)) => errors.push(format!("{label}: {m}")),
            Err(SourceError::Persistence(m)) => {
                errors.push(format!("{label}: {m}"));
                result.persistence_failed = true;
                break;
            }
        }
    }
    result.errors = errors;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::store::{Disabled, InMemory};

    fn port(n: u16) -> Value {
        Value::from(n)
    }

    #[tokio::test]
    async fn a_source_is_stored_typed_and_comes_back_with_its_id() {
        // spec/15-dashboard.md §10.4.5, §10.1.3.
        let store = InMemory::new();
        let added = add(&store, " PHW ", "Phw.Example.org", &port(2575))
            .await
            .unwrap();

        let stored = store
            .raw_document(PARTITION, "flow-source:phw.example.org:2575")
            .unwrap();
        assert_eq!(stored["type"], "flow_source_server");
        assert_eq!(list(&store).await.unwrap(), vec![added]);
    }

    #[tokio::test]
    async fn a_duplicate_url_and_port_is_refused_case_insensitively() {
        // §10.4.6.
        let store = InMemory::new();
        add(&store, "A", "Host.example", &port(1)).await.unwrap();

        let error = add(&store, "B", "HOST.example", &port(1))
            .await
            .unwrap_err();
        assert!(error.message().contains("already exists"), "{error:?}");
        assert!(
            add(&store, "B", "host.example", &port(2)).await.is_ok(),
            "another port is another source"
        );
    }

    #[tokio::test]
    async fn validation_and_persistence_are_refused_with_their_messages() {
        let store = InMemory::new();
        assert!(
            add(&store, "", "h.example", &port(1))
                .await
                .unwrap_err()
                .message()
                .contains("Description")
        );
        assert!(
            add(&store, &"x".repeat(201), "h.example", &port(1))
                .await
                .unwrap_err()
                .message()
                .contains("too long")
        );
        assert_eq!(
            add(&store, "A", "bad host", &port(1))
                .await
                .unwrap_err()
                .message(),
            "Invalid host or port."
        );
        assert_eq!(
            add(&Disabled, "A", "h.example", &port(1))
                .await
                .unwrap_err(),
            SourceError::Persistence(NOT_CONFIGURED.to_string())
        );
    }

    #[tokio::test]
    async fn an_edit_in_place_and_an_edit_to_a_new_key() {
        // §10.4.6.
        let store = InMemory::new();
        let a = add(&store, "A", "a.example", &port(1)).await.unwrap();
        let b = add(&store, "B", "b.example", &port(1)).await.unwrap();

        let renamed = update(&store, &a.id, "A2", "A.example", &port(1))
            .await
            .unwrap();
        assert_eq!(renamed.description, "A2");
        assert_eq!(list(&store).await.unwrap().len(), 2);

        let moved = update(&store, &a.id, "A2", "c.example", &port(9))
            .await
            .unwrap();
        assert_eq!(moved.id, a.id, "the public id is stable");
        assert!(
            store
                .raw_document(PARTITION, "flow-source:a.example:1")
                .is_none()
        );
        assert!(
            store
                .raw_document(PARTITION, "flow-source:c.example:9")
                .is_some()
        );

        let clash = update(&store, &a.id, "A2", "b.example", &port(1))
            .await
            .unwrap_err();
        assert!(clash.message().contains("already exists"));
        assert_eq!(
            list(&store).await.unwrap().len(),
            2,
            "a refused edit changes nothing"
        );
        assert!(list(&store).await.unwrap().iter().any(|s| s.id == b.id));

        assert_eq!(
            update(&store, "ghost", "x", "h.example", &port(1))
                .await
                .unwrap_err()
                .message(),
            "Source not found."
        );
    }

    #[tokio::test]
    async fn delete_removes_and_is_a_no_op_when_gone() {
        let store = InMemory::new();
        let a = add(&store, "A", "a.example", &port(1)).await.unwrap();

        delete(&store, &a.id).await.unwrap();
        delete(&store, &a.id).await.unwrap();

        assert!(list(&store).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_list_is_sorted_by_description() {
        let store = InMemory::new();
        add(&store, "zebra", "z.example", &port(1)).await.unwrap();
        add(&store, "Apple", "a.example", &port(1)).await.unwrap();

        let names: Vec<String> = list(&store)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.description)
            .collect();
        assert_eq!(names, ["Apple", "zebra"]);
    }

    #[tokio::test]
    async fn an_unreadable_store_is_an_error_not_an_empty_list() {
        let store = InMemory::new();
        store.fail("503");

        assert!(list(&store).await.is_err());
    }

    #[test]
    fn csv_rows_and_in_file_duplicates_are_reported() {
        // §10.4.8.
        let (rows, errors) = parse_csv(
            "Description,URL,Port\nA,a.example,1\nB,A.EXAMPLE,1\nC,bad host,2\nD,d.example,99999\n",
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert_eq!(errors[0], "Row 3: duplicate of row 2 (A.EXAMPLE:1)");
        assert!(errors[1].starts_with("Row 4: "));
        assert_eq!(
            parse_csv("description,url\n").1[0],
            "CSV header is missing required column(s): port"
        );
    }

    #[tokio::test]
    async fn an_import_skips_stored_duplicates_and_stops_on_a_failed_write() {
        let store = InMemory::new();
        add(&store, "Existing", "a.example", &port(1))
            .await
            .unwrap();

        let result = import(
            &store,
            "description,url,port\nDup,a.example,1\nNew,n.example,2\n",
        )
        .await;
        assert_eq!(result.imported, 1);
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0].starts_with("Dup (a.example:1): "),
            "{result:?}"
        );

        let failing = InMemory::new();
        failing.fail_writes("throttled");
        let result = import(
            &failing,
            "description,url,port\nA,a.example,1\nB,b.example,2\n",
        )
        .await;
        assert!(result.persistence_failed);
        assert_eq!(result.errors.len(), 1, "it stopped");

        let result = import(&Disabled, "description,url,port\nA,a.example,1\n").await;
        assert!(result.persistence_failed);
    }
}
