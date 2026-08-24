//! Against a real SQL Server.
//!
//! Implements `spec/12-message-store.md` §6.5.
//!
//! # What only a real database can show
//!
//! Three claims, and none is reachable with a mock:
//!
//! 1. **A committed transaction leaves rows.** Obvious, and the baseline
//!    the other two are measured against.
//! 2. **A failed transaction leaves none** (§5.3). The rollback is what
//!    stops an abandoned batch being redelivered and reinserted, and
//!    `monitoring.Message` holding the same message twice with no way to
//!    tell which row is real.
//! 3. **A discarded connection reopens transparently** (§5.2). A connection
//!    that has seen an error may be in an unknown transaction state, and
//!    the next call has to work anyway.
//!
//! # Running them
//!
//! ```sh
//! podman compose -f local/docker-compose.yml up -d
//! MSSQL_SA_PASSWORD=... cargo test -p ihb-sql --features sqlserver
//! ```
//!
//! Behind the `sqlserver` feature so a normal `cargo test` does not fail on
//! a machine with no database.
//!
//! # Azure SQL Edge, not SQL Server
//!
//! The local container is `azure-sql-edge`, because Microsoft ships no
//! arm64 image of `mssql/server` and it segfaults under emulation on Apple
//! Silicon. Edge speaks the same TDS and the same T-SQL for everything used
//! here. See `spec/18-deployment.md` §5.3.

#![cfg(feature = "sqlserver")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ihb_sql::{SqlConfig, SqlConnection, SqlError};

/// The schema, applied by the harness rather than by hand.
const SCHEMA: &str = include_str!("../../../local/sql/schema.sql");

/// The table these tests write to, created per run so two runs cannot see
/// each other's rows.
const TABLE: &str = "monitoring.Message";

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

/// A connection to the local container.
///
/// `SQL_ENCRYPT=No` because this build carries no TLS backend
/// (`spec/12-message-store.md` §5.8) — and against a container on the
/// loopback interface there is nothing to protect from.
fn connection() -> SqlConnection {
    let password = std::env::var("MSSQL_SA_PASSWORD")
        .expect("MSSQL_SA_PASSWORD; see this file's documentation");

    let config = SqlConfig::from_pairs([
        ("SQL_SERVER", "127.0.0.1"),
        ("SQL_DATABASE", DATABASE),
        ("SQL_USERNAME", "sa"),
        ("MSSQL_SA_PASSWORD", password.as_str()),
        ("SQL_ENCRYPT", "No"),
        ("SQL_TRUST_SERVER_CERTIFICATE", "Yes"),
    ])
    .expect("the test configuration is complete");

    SqlConnection::new(config)
}

/// Applied once per test binary.
///
/// The statements guard themselves with `IF OBJECT_ID(...) IS NULL`, which
/// is idempotent against a *sequence* of runs and not against concurrent
/// ones: `cargo test` runs these in parallel, and two of them checking the
/// guard at the same moment both find nothing and both try to create. This
/// is the lock that stops that.
static SCHEMA_APPLIED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Apply the schema, and return a connection ready to use.
async fn prepared() -> SqlConnection {
    SCHEMA_APPLIED.get_or_init(apply_schema).await;
    connection()
}

/// Run every statement in `schema.sql`.
async fn apply_schema() {
    let mut sql = bootstrap_connection();
    let mut client = sql.take().await.expect("the database is reachable");

    // `GO` is a batch separator understood by client tools, not by the
    // server, so the file is split on it here.
    for statement in SCHEMA.split("\nGO") {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        client
            .simple_query(statement)
            .await
            .unwrap_or_else(|error| panic!("applying the schema: {error}\n{statement}"))
            .into_results()
            .await
            .unwrap();
    }
}

/// Insert one row with this correlation id, inside `sql`'s transaction.
async fn insert_one(sql: &mut SqlConnection, correlation_id: &str) -> Result<(), SqlError> {
    let id = correlation_id.to_string();

    sql.transaction(async |client| {
        let parameters: Vec<&dyn tiberius::ToSql> = vec![&id];
        client
            .query(
                "INSERT INTO monitoring.Message (
                     ReceivedAt, StoredAt, CorrelationId, SourceSystem,
                     ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId
                 ) VALUES (
                     SYSUTCDATETIME(), SYSUTCDATETIME(), @P1, '252',
                     'test', NULL, 'MSH|^~\\&|252', NULL, 'phw-to-mpi'
                 )",
                &parameters,
            )
            .await
            .map_err(|error| SqlError::Statement(error.to_string()))?
            .into_results()
            .await
            .map_err(|error| SqlError::Statement(error.to_string()))?;
        Ok(())
    })
    .await
}

/// How many rows carry this correlation id.
async fn count(sql: &mut SqlConnection, correlation_id: &str) -> i32 {
    let mut client = sql.take().await.unwrap();
    let id = correlation_id.to_string();
    let parameters: Vec<&dyn tiberius::ToSql> = vec![&id];

    let rows = client
        .query(
            &format!("SELECT COUNT(*) AS n FROM {TABLE} WHERE CorrelationId = @P1"),
            &parameters,
        )
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();

    let n: i32 = rows[0].get("n").unwrap_or_default();
    sql.restore(client);
    n
}

/// A correlation id unique to this run, so tests do not see each other's
/// rows and a container left dirty cannot make one pass.
fn unique(label: &str) -> String {
    format!(
        "ihb-sql-test-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    )
}

#[tokio::test]
async fn a_committed_transaction_leaves_its_rows() {
    // spec/12-message-store.md §5.3 and §6.5, first claim.
    let mut sql = prepared().await;
    let id = unique("commit");

    insert_one(&mut sql, &id).await.expect("the insert commits");

    assert_eq!(count(&mut sql, &id).await, 1);
}

#[tokio::test]
async fn a_failed_transaction_leaves_none() {
    // §5.3 and §6.5, second claim — THE test of this file. Without the
    // rollback, an abandoned batch redelivered by Service Bus would insert
    // its rows a second time, and `monitoring.Message` would hold the same
    // message twice with no way to tell which row is real.
    let mut sql = prepared().await;
    let id = unique("rollback");

    let result = sql
        .transaction(async |client| {
            // A real insert, so there is something to roll back.
            let correlation = id.clone();
            let parameters: Vec<&dyn tiberius::ToSql> = vec![&correlation];
            client
                .query(
                    "INSERT INTO monitoring.Message (
                         ReceivedAt, StoredAt, CorrelationId, SourceSystem,
                         ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId
                     ) VALUES (
                         SYSUTCDATETIME(), SYSUTCDATETIME(), @P1, '252',
                         'test', NULL, 'MSH|^~\\&|252', NULL, 'phw-to-mpi'
                     )",
                    &parameters,
                )
                .await
                .map_err(|error| SqlError::Statement(error.to_string()))?
                .into_results()
                .await
                .map_err(|error| SqlError::Statement(error.to_string()))?;

            // Then a failure, as a malformed document or a constraint
            // violation would produce mid-batch.
            Err(SqlError::Statement("the batch failed".to_string()))
        })
        .await;

    assert!(result.is_err(), "the transaction reports the failure");
    assert_eq!(
        count(&mut sql, &id).await,
        0,
        "and the inserted row was rolled back"
    );
}

#[tokio::test]
async fn a_discarded_connection_reopens_on_the_next_call() {
    // §5.2 and §6.5, third claim. A connection that has seen an error may
    // be in an unknown transaction state, so it is dropped — and the next
    // call has to work anyway, without the caller knowing.
    let mut sql = prepared().await;
    let id = unique("reopen");

    insert_one(&mut sql, &id).await.unwrap();
    assert!(sql.is_open(), "a healthy connection is kept for reuse");

    sql.discard();
    assert!(!sql.is_open());

    // No reconnect call anywhere: the next use just works.
    let second = unique("reopen-second");
    insert_one(&mut sql, &second).await.expect("it reconnected");

    assert_eq!(count(&mut sql, &second).await, 1);
    assert!(sql.is_open());
}

#[tokio::test]
async fn a_failed_transaction_discards_the_connection() {
    // §5.2 — the discard is what makes the reopen above necessary, and it
    // is not visible from the transaction's return value alone.
    let mut sql = prepared().await;
    // Opened by the first statement, not by `prepared` — §5.1 is lazy.
    insert_one(&mut sql, &unique("discard-warmup"))
        .await
        .unwrap();
    assert!(sql.is_open());

    let result = sql
        .transaction(async |_client| Err(SqlError::Statement("no".to_string())))
        .await;

    assert!(result.is_err());
    assert!(
        !sql.is_open(),
        "a connection that has seen an error is not put back"
    );
}

#[tokio::test]
async fn a_statement_error_from_the_server_rolls_back_too() {
    // The rollback path driven by a *real* database error rather than one
    // this test invented: the previous test proves the plumbing, this one
    // proves it fires when SQL Server is the thing that says no.
    let mut sql = prepared().await;
    let id = unique("server-error");

    let result = sql
        .transaction(async |client| {
            let correlation = id.clone();
            let parameters: Vec<&dyn tiberius::ToSql> = vec![&correlation];
            client
                .query(
                    "INSERT INTO monitoring.Message (
                         ReceivedAt, StoredAt, CorrelationId, SourceSystem,
                         ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId
                     ) VALUES (
                         SYSUTCDATETIME(), SYSUTCDATETIME(), @P1, '252',
                         'test', NULL, 'MSH|^~\\&|252', NULL, 'phw-to-mpi'
                     )",
                    &parameters,
                )
                .await
                .map_err(|error| SqlError::Statement(error.to_string()))?
                .into_results()
                .await
                .map_err(|error| SqlError::Statement(error.to_string()))?;

            // `ReceivedAt` is NOT NULL, so this is the server's refusal
            // rather than ours.
            client
                .simple_query(
                    "INSERT INTO monitoring.Message (
                         ReceivedAt, StoredAt, CorrelationId, SourceSystem,
                         ProcessingComponent, RawPayload, SessionId
                     ) VALUES (NULL, SYSUTCDATETIME(), 'x', 'x', 'x', 'x', 'x')",
                )
                .await
                .map_err(|error| SqlError::Statement(error.to_string()))?
                .into_results()
                .await
                .map_err(|error| SqlError::Statement(error.to_string()))?;

            Ok(())
        })
        .await;

    assert!(result.is_err(), "the server refused the second insert");
    assert_eq!(
        count(&mut sql, &id).await,
        0,
        "so the first insert was rolled back with it"
    );
}

#[tokio::test]
async fn a_credential_never_reaches_an_error() {
    // §5.7 and §6.7, against a real failure rather than a constructed one:
    // the wrong password, and the resulting error is checked for it.
    let config = SqlConfig::from_pairs([
        ("SQL_SERVER", "127.0.0.1"),
        ("SQL_DATABASE", DATABASE),
        ("SQL_USERNAME", "sa"),
        ("MSSQL_SA_PASSWORD", "a-deliberately-wrong-password-9f3c"),
        ("SQL_ENCRYPT", "No"),
        ("SQL_TRUST_SERVER_CERTIFICATE", "Yes"),
    ])
    .unwrap();

    let mut sql = SqlConnection::new(config);
    let error = sql.take().await.expect_err("the password is wrong");

    let rendered = format!("{error}\n{error:?}");
    assert!(
        !rendered.contains("a-deliberately-wrong-password-9f3c"),
        "the password reached an error: {rendered}"
    );
    assert!(
        rendered.contains("127.0.0.1"),
        "the server may appear: {rendered}"
    );
}
