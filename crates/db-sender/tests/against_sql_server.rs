//! `db-sender`'s two procedures against a real SQL Server.
//!
//! Implements `spec/10-senders.md` §7.7 and §7.8.
//!
//! Until this file the service had never called a procedure on a real
//! server. What only a database can show:
//!
//! 1. **`EXEC proc @P1, @P2` with positional parameters works** (the
//!    message procedure), including the `varchar(max)` body and the `bit`.
//! 2. **Six parameters bind, with `NULL`s in the middle** (the id update):
//!    `Option<&str>` as `NULL` is the part a mock cannot check.
//! 3. **A refused call is `Fatal` and the next call reconnects** (§7.3).
//!
//! # Running them
//!
//! ```sh
//! podman compose -f local/docker-compose.yml up -d sqledge
//! MSSQL_SA_PASSWORD=... cargo test -p db-sender --features sqlserver --test against_sql_server
//! ```
//!
//! Behind the `sqlserver` feature so a plain `cargo test` needs no database.
//! The container is Azure SQL Edge (`spec/18-deployment.md` §5.3); it speaks
//! the same TDS and T-SQL for everything used here.

#![cfg(feature = "sqlserver")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use db_sender::AppConfig;
use db_sender::db::{FailureKind, ProcedureRunner, SqlRunner};
use db_sender::idupdate::IdUpdate;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

/// The script that creates `DRS` and both procedures.
const SCRIPT: &str = include_str!("../../../local/sql/init-drs-db.sql");

fn password() -> String {
    std::env::var("MSSQL_SA_PASSWORD").expect("MSSQL_SA_PASSWORD; see this file's documentation")
}

/// A raw client, for applying the script and reading rows back.
async fn raw(database: &str) -> Client<Compat<TcpStream>> {
    let mut config = Config::new();
    config.host("127.0.0.1");
    config.port(1433);
    config.database(database);
    config.encryption(EncryptionLevel::Required);
    config.authentication(AuthMethod::sql_server("sa", password()));
    config.trust_cert();
    let stream = TcpStream::connect("127.0.0.1:1433").await.unwrap();
    stream.set_nodelay(true).unwrap();
    Client::connect(config, stream.compat_write())
        .await
        .unwrap()
}

/// Apply the script, batch by batch, once per test binary.
///
/// `GO` is a client-side separator, not T-SQL, so it is split here. A lock
/// keeps parallel tests from creating the same objects at once.
async fn apply_script() {
    static APPLIED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    APPLIED
        .get_or_init(|| async {
            let mut client = raw("master").await;
            let mut batch = String::new();
            let mut batches = Vec::new();
            for line in SCRIPT.lines() {
                if line.trim().eq_ignore_ascii_case("go") {
                    batches.push(std::mem::take(&mut batch));
                } else {
                    batch.push_str(line);
                    batch.push('\n');
                }
            }
            batches.push(batch);
            for batch in batches.iter().filter(|b| !b.trim().is_empty()) {
                client
                    .simple_query(batch.as_str())
                    .await
                    .unwrap_or_else(|e| panic!("applying the script: {e}\n{batch}"))
                    .into_results()
                    .await
                    .unwrap();
            }
        })
        .await;
}

fn runner(procedure: &str, procedure_type: &str) -> SqlRunner {
    runner_with_timeout(procedure, procedure_type, "60")
}

fn runner_with_timeout(procedure: &str, procedure_type: &str, command_timeout: &str) -> SqlRunner {
    let password = password();
    let config = AppConfig::from_pairs([
        ("INGRESS_TOPIC_NAME", "t"),
        ("INGRESS_SUBSCRIPTION_NAME", "s"),
        ("DB_SERVER", "127.0.0.1"),
        ("DB_NAME", "DRS"),
        ("DB_STORED_PROCEDURE", procedure),
        ("DB_PROCEDURE_TYPE", procedure_type),
        ("DB_USERNAME", "sa"),
        ("DB_PASSWORD", password.as_str()),
        ("DB_ENCRYPT", "Yes"),
        ("DB_TRUST_SERVER_CERTIFICATE", "Yes"),
        ("DB_COMMAND_TIMEOUT", command_timeout),
    ])
    .unwrap();
    SqlRunner::new(config.db)
}

/// A value unique to this run, short enough for `varchar(20)`.
fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{prefix}{nanos}")
}

async fn scalar(sql: &str) -> Option<String> {
    let mut client = raw("DRS").await;
    client
        .query(sql, &[])
        .await
        .unwrap()
        .into_row()
        .await
        .unwrap()
        .and_then(|row| row.get::<&str, _>(0).map(str::to_string))
}

#[tokio::test]
async fn the_message_procedure_stores_the_body_and_the_priority() {
    apply_script().await;
    let marker = unique("MSG");
    let body =
        format!("<ADT_A39><MSH><MSH.10><MSH.10.1>{marker}</MSH.10.1></MSH.10></MSH></ADT_A39>");

    runner("dbo.prQueueMessage", "queue_message")
        .execute(&body, true)
        .await
        .unwrap();

    let found = scalar(&format!(
        "SELECT CAST(IsPriority AS varchar(5)) FROM dbo.QueueMessage WHERE [Message] LIKE '%{marker}%'"
    ))
    .await;
    assert_eq!(found.as_deref(), Some("1"));
}

#[tokio::test]
async fn an_a31_writes_a_row_with_nulls_in_the_old_identifier_columns() {
    apply_script().await;
    let nhs = unique("N");
    let update = IdUpdate {
        provider_identifier: Some("555".into()),
        provider_authority: Some("328".into()),
        old_identifier: None,
        old_authority: None,
        new_identifier: nhs.clone(),
        new_authority: "NHS".into(),
    };

    runner("dbo.prQueueIdUpdate", "queue_id_update")
        .execute_id_update(&update)
        .await
        .unwrap();

    let row = scalar(&format!(
        "SELECT CONCAT(ProviderPatientIdentifier, '|', ProviderAssigningAuthority, '|', \
         ISNULL(OldIdentifier, 'NULL'), '|', ISNULL(OldIdentifierAssigningAuthority, 'NULL'), '|', \
         NewIdentifier, '|', NewIdentifierAssigningAuthority) \
         FROM dbo.QueueIdUpdate WHERE NewIdentifier = '{nhs}'"
    ))
    .await;
    assert_eq!(row, Some(format!("555|328|NULL|NULL|{nhs}|NHS")));
}

#[tokio::test]
async fn an_a40_writes_a_row_with_a_null_provider_identifier() {
    apply_script().await;
    let new = unique("M");
    let update = IdUpdate {
        provider_identifier: None,
        provider_authority: Some("NHS".into()),
        old_identifier: Some("1111111111".into()),
        old_authority: Some("NHS".into()),
        new_identifier: new.clone(),
        new_authority: "NHS".into(),
    };

    runner("dbo.prQueueIdUpdate", "queue_id_update")
        .execute_id_update(&update)
        .await
        .unwrap();

    let row = scalar(&format!(
        "SELECT CONCAT(ISNULL(ProviderPatientIdentifier, 'NULL'), '|', OldIdentifier) \
         FROM dbo.QueueIdUpdate WHERE NewIdentifier = '{new}'"
    ))
    .await;
    assert_eq!(row.as_deref(), Some("NULL|1111111111"));
}

#[tokio::test]
async fn a_missing_procedure_is_fatal_and_the_next_call_reconnects() {
    apply_script().await;
    let mut missing = runner("dbo.prDoesNotExist", "queue_message");
    let error = missing.execute("x", false).await.unwrap_err();
    assert_eq!(error.kind, FailureKind::Fatal, "{error}");
    assert_eq!(error.code, 2812, "{error}");

    // The same runner, pointed at nothing new, fails the same way rather than
    // erroring on a dead connection: the old one was discarded.
    let again = missing.execute("x", false).await.unwrap_err();
    assert_eq!(again.code, 2812, "{again}");
}

#[tokio::test]
async fn the_server_truncates_a_too_long_identifier_silently() {
    // Found by this suite (spec/10-senders.md §7.8.1). The procedure's
    // parameters are varchar(20) and varchar(10), and SQL Server does not
    // refuse a longer value: it cuts it and the INSERT succeeds. For a
    // patient identifier that writes a *different* identifier, which is why
    // `idupdate::plan` refuses a value that would not fit and this runner is
    // never given one. This test pins the server behaviour that rule rests on.
    apply_script().await;
    let long = format!("{}{}", unique("T"), "ABCDEFGHIJKLMNOPQRSTUVWXYZ");
    assert!(long.len() > 20);
    let update = IdUpdate {
        provider_identifier: None,
        provider_authority: None,
        old_identifier: None,
        old_authority: None,
        new_identifier: long.clone(),
        new_authority: "NHS".into(),
    };

    runner("dbo.prQueueIdUpdate", "queue_id_update")
        .execute_id_update(&update)
        .await
        .expect("the server accepts it");

    let stored = scalar(&format!(
        "SELECT TOP 1 NewIdentifier FROM dbo.QueueIdUpdate WHERE NewIdentifier = '{}' ORDER BY Id DESC",
        &long[..20]
    ))
    .await;
    assert_eq!(
        stored.as_deref(),
        Some(&long[..20]),
        "cut to the column's width"
    );
}

/// `spec/10-senders.md` §7.3a: `RETURN 55555` with no SQL error is a
/// failure, and nothing is stored.
#[tokio::test]
async fn a_return_code_of_55555_is_fatal_and_stores_nothing() {
    apply_script().await;
    let marker = unique("REJ");
    let body = format!("<A><M>{marker}</M><S>SCENARIO-REJECT</S></A>");
    let mut runner = runner("dbo.prQueueMessage", "queue_message");
    let error = runner.execute(&body, false).await.unwrap_err();
    assert_eq!(error.kind, FailureKind::Fatal, "{error}");
    assert_eq!(error.code, 55555, "{error}");
    assert!(!error.description().contains(&marker), "{error}");
    let stored = scalar(&format!(
        "SELECT CAST(COUNT(*) AS varchar(10)) FROM dbo.QueueMessage WHERE [Message] LIKE '%{marker}%'"
    ))
    .await;
    assert_eq!(stored.as_deref(), Some("0"));

    // The runner recovers: the next good message is stored.
    runner.execute("<A><M>ok</M></A>", false).await.unwrap();
}

/// `spec/10-senders.md` §7.3: a procedure that raises is fatal.
#[tokio::test]
async fn a_raised_error_is_fatal() {
    apply_script().await;
    let error = runner("dbo.prQueueMessage", "queue_message")
        .execute("<A>SCENARIO-ERROR</A>", false)
        .await
        .unwrap_err();
    assert_eq!(error.kind, FailureKind::Fatal, "{error}");
    assert_eq!(error.code, 50001, "{error}");
}

/// `spec/10-senders.md` §7.3: a call that outlasts `DB_COMMAND_TIMEOUT` is
/// transient, so the message is retried rather than dead-lettered.
#[tokio::test]
async fn a_command_timeout_is_transient() {
    apply_script().await;
    let error = runner_with_timeout("dbo.prQueueMessage", "queue_message", "2")
        .execute("<A>SCENARIO-TIMEOUT</A>", false)
        .await
        .unwrap_err();
    assert_eq!(error.kind, FailureKind::Transient, "{error}");
}
