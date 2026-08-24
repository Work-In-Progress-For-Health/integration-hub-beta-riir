//! Inserting batches into a real database.
//!
//! Implements `spec/12-message-store.md` §6.5 and §6.6.
//!
//! # What only a real database can show
//!
//! The unit tests build records and assemble statements. What they cannot
//! check is whether the statement `insert::statement` produces, bound to
//! the parameters `insert_all` pushes, actually puts each value in the
//! column §2.1 names. A column-order mistake would put a raw payload in
//! `SessionId` and every unit test would still pass.
//!
//! # Running them
//!
//! ```sh
//! podman compose -f local/docker-compose.yml up -d
//! MSSQL_SA_PASSWORD=... cargo test -p message-store-service --features sqlserver
//! ```

#![cfg(feature = "sqlserver")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ihb_sql::{SqlConfig, SqlConnection};
use message_store_service::batch::{BatchOutcome, build_all, stored_at_now};
use message_store_service::insert::insert_all;
use message_store_service::record::Record;
use time::OffsetDateTime;

/// The schema, applied by the harness.
const SCHEMA: &str = include_str!("../../../local/sql/schema.sql");

/// The database the services use.
///
/// `spec/18-deployment.md` §5.2 keeps the Python's `.env` files unchanged,
/// and they say `SQL_DATABASE=IntegrationHub`. These tests used `master`,
/// so the schema they applied was in a different database from the one the
/// containers read — and nothing created `IntegrationHub` at all.
const DATABASE: &str = "IntegrationHub";

/// The database the schema is applied *from*.
///
/// `CREATE DATABASE` cannot run inside the database it creates.
const BOOTSTRAP_DATABASE: &str = "master";

/// A connection to `master`, for applying the schema.
///
/// The schema's first statement is `CREATE DATABASE IntegrationHub`, which
/// cannot run inside the database it creates.
fn bootstrap_connection() -> SqlConnection {
    let password = std::env::var("MSSQL_SA_PASSWORD")
        .expect("MSSQL_SA_PASSWORD; see this file's documentation");

    SqlConnection::new(
        SqlConfig::from_pairs([
            ("SQL_SERVER", "127.0.0.1"),
            ("SQL_DATABASE", BOOTSTRAP_DATABASE),
            ("SQL_USERNAME", "sa"),
            ("MSSQL_SA_PASSWORD", password.as_str()),
            ("SQL_ENCRYPT", "No"),
            ("SQL_TRUST_SERVER_CERTIFICATE", "Yes"),
        ])
        .expect("the test configuration is complete"),
    )
}

fn connection() -> SqlConnection {
    let password = std::env::var("MSSQL_SA_PASSWORD")
        .expect("MSSQL_SA_PASSWORD; see this file's documentation");

    SqlConnection::new(
        SqlConfig::from_pairs([
            ("SQL_SERVER", "127.0.0.1"),
            ("SQL_DATABASE", DATABASE),
            ("SQL_USERNAME", "sa"),
            ("MSSQL_SA_PASSWORD", password.as_str()),
            ("SQL_ENCRYPT", "No"),
            ("SQL_TRUST_SERVER_CERTIFICATE", "Yes"),
        ])
        .expect("the test configuration is complete"),
    )
}

/// Applied once per test binary; see `ihb-sql`'s equivalent for why.
static SCHEMA_APPLIED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn prepared() -> SqlConnection {
    SCHEMA_APPLIED
        .get_or_init(|| async {
            let mut sql = bootstrap_connection();
            let mut client = sql.take().await.expect("the database is reachable");
            for statement in SCHEMA.split("\nGO") {
                let statement = statement.trim();
                if statement.is_empty() {
                    continue;
                }
                client
                    .simple_query(statement)
                    .await
                    .unwrap_or_else(|error| panic!("applying the schema: {error}"))
                    .into_results()
                    .await
                    .unwrap();
            }
        })
        .await;

    connection()
}

/// A correlation id unique to this run.
fn unique(label: &str) -> String {
    format!(
        "store-test-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    )
}

/// A store document with this correlation id.
fn document(correlation_id: &str, target: Option<&str>, xml: Option<&str>) -> String {
    let mut value = serde_json::json!({
        "MessageReceivedAt": "2026-08-21T09:15:00+00:00",
        "CorrelationId": correlation_id,
        "SourceSystem": "252",
        "ProcessingComponent": "phw_hl7_server",
        "RawPayload": "MSH|^~\\&|252|252|100|100|20260821||ADT^A31|M1|P|2.5",
        "SessionId": "phw-to-mpi"
    });
    if let Some(target) = target {
        value["TargetSystem"] = serde_json::json!(target);
    }
    if let Some(xml) = xml {
        value["XmlPayload"] = serde_json::json!(xml);
    }
    value.to_string()
}

/// Read back the one row with this correlation id.
async fn row_for(sql: &mut SqlConnection, correlation_id: &str) -> Option<StoredRow> {
    let mut client = sql.take().await.unwrap();
    let id = correlation_id.to_string();
    let parameters: Vec<&dyn tiberius::ToSql> = vec![&id];

    let rows = client
        .query(
            "SELECT ReceivedAt, StoredAt, CorrelationId, SourceSystem,
                    ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId
             FROM monitoring.Message WHERE CorrelationId = @P1",
            &parameters,
        )
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();

    let found = rows.first().map(|row| StoredRow {
        correlation_id: row
            .get::<&str, _>("CorrelationId")
            .unwrap_or_default()
            .to_string(),
        source_system: row
            .get::<&str, _>("SourceSystem")
            .unwrap_or_default()
            .to_string(),
        processing_component: row
            .get::<&str, _>("ProcessingComponent")
            .unwrap_or_default()
            .to_string(),
        target_system: row.get::<&str, _>("TargetSystem").map(ToString::to_string),
        raw_payload: row
            .get::<&str, _>("RawPayload")
            .unwrap_or_default()
            .to_string(),
        xml_payload: row.get::<&str, _>("XmlPayload").map(ToString::to_string),
        session_id: row
            .get::<&str, _>("SessionId")
            .unwrap_or_default()
            .to_string(),
    });

    sql.restore(client);
    found
}

/// What came back out of the table.
#[derive(Debug)]
struct StoredRow {
    correlation_id: String,
    source_system: String,
    processing_component: String,
    target_system: Option<String>,
    raw_payload: String,
    xml_payload: Option<String>,
    session_id: String,
}

/// Insert records in one transaction, as the service does.
async fn store(sql: &mut SqlConnection, records: &[Record], at: OffsetDateTime) -> BatchOutcome {
    match sql
        .transaction(async |client| insert_all(client, records, at).await)
        .await
    {
        Ok(()) => BatchOutcome::Committed {
            count: records.len(),
        },
        Err(error) => BatchOutcome::Abandoned {
            reason: error.to_string(),
        },
    }
}

#[tokio::test]
async fn every_value_lands_in_the_column_section_two_one_names() {
    // spec/12-message-store.md §2.1 and §6.5 — THE test of this file. A
    // column-order mistake would put a raw payload in `SessionId`, and
    // every unit test in this crate would still pass.
    let mut sql = prepared().await;
    let id = unique("columns");

    let records = build_all_from(&[document(&id, Some("MPI"), Some("<ADT_A05/>"))]);
    let outcome = store(&mut sql, &records, stored_at_now()).await;
    assert!(outcome.is_committed(), "{outcome:?}");

    let row = row_for(&mut sql, &id).await.expect("the row is there");

    assert_eq!(row.correlation_id, id);
    assert_eq!(row.source_system, "252");
    assert_eq!(row.processing_component, "phw_hl7_server");
    assert_eq!(row.target_system.as_deref(), Some("MPI"));
    assert!(row.raw_payload.starts_with("MSH|^~\\&|252"));
    assert_eq!(row.xml_payload.as_deref(), Some("<ADT_A05/>"));
    assert_eq!(row.session_id, "phw-to-mpi");
}

#[tokio::test]
async fn the_optional_columns_are_really_null() {
    // §3.4 and §6.2, read back from the database rather than from the
    // record — a `None` that became an empty string would look the same in
    // Rust and different to every query an operator writes.
    let mut sql = prepared().await;
    let id = unique("nulls");

    let records = build_all_from(&[document(&id, None, None)]);
    assert!(
        store(&mut sql, &records, stored_at_now())
            .await
            .is_committed()
    );

    let row = row_for(&mut sql, &id).await.unwrap();

    assert_eq!(row.target_system, None);
    assert_eq!(row.xml_payload, None);
}

#[tokio::test]
async fn a_whole_batch_commits_together() {
    // §1.1 and §4.1.
    let mut sql = prepared().await;
    let ids: Vec<String> = (0..10).map(|n| unique(&format!("batch-{n}"))).collect();

    let documents: Vec<String> = ids
        .iter()
        .map(|id| document(id, Some("MPI"), None))
        .collect();
    let records = build_all_from(&documents);

    let outcome = store(&mut sql, &records, stored_at_now()).await;
    assert_eq!(outcome, BatchOutcome::Committed { count: 10 });

    for id in &ids {
        assert!(row_for(&mut sql, id).await.is_some(), "{id}");
    }
}

#[tokio::test]
async fn a_batch_shares_one_stored_at() {
    // §2.2 — computed once per batch, which is what lets an operator group
    // a batch in the table afterwards. Read back and compared.
    //
    // The three ids are matched **exactly**, not by a shared prefix. They
    // do not have one: `unique` stamps each with its own nanosecond
    // timestamp, so `store-test-stamp-0-…` and `store-test-stamp-1-…`
    // differ from the fourth segment on.
    //
    // An earlier version took the prefix of `ids[0]` up to its last `-`,
    // which matched exactly one row of the batch — so it asserted "one
    // StoredAt across the batch" while looking at one row, and was
    // vacuously true. It also matched every *previous* run's `stamp-0`
    // row, so it failed on the second run against a persistent database
    // for a reason unrelated to what it was testing. See
    // spec/17-testing-and-quality.md §9.2.1b.
    let mut sql = prepared().await;
    let ids: Vec<String> = (0..3).map(|n| unique(&format!("stamp-{n}"))).collect();

    let documents: Vec<String> = ids.iter().map(|id| document(id, None, None)).collect();
    let records = build_all_from(&documents);

    assert!(
        store(&mut sql, &records, stored_at_now())
            .await
            .is_committed()
    );

    let mut client = sql.take().await.unwrap();
    let parameters: Vec<&dyn tiberius::ToSql> = vec![&ids[0], &ids[1], &ids[2]];
    let rows = client
        .query(
            "SELECT COUNT(*) AS rows_found, COUNT(DISTINCT StoredAt) AS stamps
             FROM monitoring.Message
             WHERE CorrelationId IN (@P1, @P2, @P3)",
            &parameters,
        )
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    let rows_found: i32 = rows[0].get("rows_found").unwrap_or_default();
    let stamps: i32 = rows[0].get("stamps").unwrap_or_default();
    sql.restore(client);

    // Without this the next assertion passes on nothing at all, which is
    // how the previous version came to be vacuous.
    assert_eq!(rows_found, 3, "all three rows of the batch were found");
    assert_eq!(stamps, 1, "one StoredAt across the batch");
}

#[tokio::test]
async fn a_failing_batch_leaves_no_rows_at_all() {
    // §5.3 and §6.5 — the rollback, at the service's own level. Without it
    // an abandoned batch redelivered by Service Bus would insert its good
    // rows twice.
    let mut sql = prepared().await;
    let good = unique("partial-good");

    // A record whose `RawPayload` exceeds nothing but whose *session* is
    // absurdly long: `SessionId` is NVARCHAR(200), so the server refuses
    // the second row and the first must go with it.
    let mut records = build_all_from(&[document(&good, None, None)]);
    let mut poison = records[0].clone();
    poison.correlation_id = unique("partial-bad");
    poison.session_id = "x".repeat(500);
    records.push(poison);

    let outcome = store(&mut sql, &records, stored_at_now()).await;

    assert!(!outcome.is_committed(), "the server refused the batch");
    assert!(
        row_for(&mut sql, &good).await.is_none(),
        "and the good row went with it"
    );
}

#[tokio::test]
async fn a_poison_document_never_reaches_the_database() {
    // §3.6 and §6.6 — a batch with one malformed document fails at the
    // building stage, so nothing is attempted at all.
    let good = unique("poison-good");
    let documents = vec![document(&good, None, None), "not json".to_string()];

    let error = build_all(&received(&documents).await).unwrap_err();

    assert_eq!(error, "Invalid JSON in message body");

    // And nothing was inserted, because `build_all` failed before any
    // statement ran.
    let mut sql = prepared().await;
    assert!(row_for(&mut sql, &good).await.is_none());
}

/// Build records from documents, panicking on a failure.
fn build_all_from(documents: &[String]) -> Vec<Record> {
    documents
        .iter()
        .map(|body| message_store_service::record::build(body).expect("a valid document"))
        .collect()
}

/// Received messages carrying `bodies`, via the fake bus.
async fn received(bodies: &[String]) -> Vec<ihb_bus::message::Incoming> {
    use ihb_bus::sink::MessageSink;

    let bus = ihb_bus::fake::FakeBus::new();
    let sender = bus.sender("messagestore");
    for body in bodies {
        sender
            .send(ihb_bus::message::Outgoing::new(body.clone()))
            .await
            .unwrap();
    }
    bus.receive("messagestore", bodies.len())
}
