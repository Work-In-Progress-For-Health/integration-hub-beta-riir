//! Fetch, send and mark, against a real database.
//!
//! Implements `spec/13-message-replay.md` §8.4.
//!
//! # What only a real database can show
//!
//! The unit tests cover the statements and the classification. Three things
//! they cannot reach:
//!
//! 1. **The fetch really returns rows in `ReplayId` order** (§3.3). A
//!    clinical feed replayed out of order can apply an older update over a
//!    newer one, and the ordering lives in the SQL rather than in Rust.
//! 2. **`Loaded` really sticks**, so a second run does not replay the same
//!    messages again.
//! 3. **`Failed` rows are picked up again** (§2.2) — which is the entire
//!    recovery procedure. If that did not work, a failed replay would need
//!    someone to write SQL by hand at the worst moment.
//!
//! # Running them
//!
//! ```sh
//! podman compose -f local/docker-compose.yml up -d
//! MSSQL_SA_PASSWORD=... cargo test -p message-replay-job --features sqlserver
//! ```

#![cfg(feature = "sqlserver")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ihb_bus::error::BusError;
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_bus::message::Outgoing;
use ihb_bus::retry::Disposition;
use ihb_bus::sink::MessageSink;
use ihb_sql::{SqlConfig, SqlConnection};
use message_replay_job::job::{fetch, mark, mark_failed, send};
use message_replay_job::sql::Status;
use uuid::Uuid;

/// The schema, applied by the harness.
const SCHEMA: &str = include_str!("../../../local/sql/schema.sql");

/// The queue replayed messages go to.
const QUEUE: &str = "priority";

/// A connection to the local container.
fn connection() -> SqlConnection {
    let password = std::env::var("MSSQL_SA_PASSWORD")
        .expect("MSSQL_SA_PASSWORD; see this file's documentation");

    SqlConnection::new(
        SqlConfig::from_pairs([
            ("SQL_SERVER", "127.0.0.1"),
            ("SQL_DATABASE", "master"),
            ("SQL_USERNAME", "sa"),
            ("MSSQL_SA_PASSWORD", password.as_str()),
            ("SQL_ENCRYPT", "No"),
            ("SQL_TRUST_SERVER_CERTIFICATE", "Yes"),
        ])
        .expect("the test configuration is complete"),
    )
}

/// The schema is applied once per test binary; see `ihb-sql`'s equivalent
/// for why a `IF OBJECT_ID` guard is not enough under parallel tests.
static SCHEMA_APPLIED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Apply the schema and seed `count` messages into a fresh replay batch.
///
/// Returns the batch id. Each test gets its own, so tests cannot see each
/// other's rows and a container left dirty by an earlier run cannot make
/// one pass.
async fn seeded(count: usize) -> (SqlConnection, Uuid) {
    SCHEMA_APPLIED
        .get_or_init(|| async {
            let mut sql = connection();
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

    let batch_id = Uuid::new_v4();
    let mut sql = connection();
    let mut client = sql.take().await.unwrap();

    for index in 0..count {
        let payload = format!("MSH|^~\\&|252|252|100|100|20260821||ADT^A31|REPLAY{index}|P|2.5");
        let correlation = format!("correlation-{index}");
        let batch = batch_id.to_string();

        let parameters: Vec<&dyn tiberius::ToSql> = vec![&payload, &correlation, &batch];
        client
            .query(
                "INSERT INTO monitoring.Message (
                     ReceivedAt, StoredAt, CorrelationId, SourceSystem,
                     ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId
                 ) VALUES (
                     SYSUTCDATETIME(), SYSUTCDATETIME(), @P2, '252',
                     'phw_hl7_server', 'MPI', @P1, NULL, 'phw-to-mpi'
                 );
                 INSERT INTO monitoring.MessageReplayQueue
                     (MessageId, ReplayBatchId, Status)
                 VALUES (SCOPE_IDENTITY(), @P3, 'Pending');",
                &parameters,
            )
            .await
            .unwrap()
            .into_results()
            .await
            .unwrap();
    }

    sql.restore(client);
    (sql, batch_id)
}

/// How many rows of a batch hold this status.
async fn count_with(sql: &mut SqlConnection, batch_id: Uuid, status: Status) -> i32 {
    let mut client = sql.take().await.unwrap();
    let batch = batch_id.to_string();
    let label = status.as_str();
    let parameters: Vec<&dyn tiberius::ToSql> = vec![&batch, &label];

    let rows = client
        .query(
            "SELECT COUNT(*) AS n FROM monitoring.MessageReplayQueue
             WHERE ReplayBatchId = @P1 AND Status = @P2",
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

/// A sink that fails every send.
struct Refusing;

impl MessageSink for Refusing {
    fn send(
        &self,
        _message: Outgoing,
    ) -> impl std::future::Future<Output = Result<(), BusError>> + Send {
        std::future::ready(Err(BusError::Send {
            destination: QUEUE.to_string(),
            attempts: 1,
            reason: "refusing, for the test".to_string(),
            disposition: Disposition::Retry,
        }))
    }

    fn entity(&self) -> &str {
        QUEUE
    }
}

#[tokio::test]
async fn a_batch_is_fetched_in_order_sent_and_marked_loaded() {
    // spec/13-message-replay.md §8.4, the happy path.
    let (mut sql, batch_id) = seeded(5).await;
    let bus = FakeBus::new();
    let sink: FakeSender = bus.sender(QUEUE);

    let page = fetch(&mut sql, batch_id, 100)
        .await
        .expect("the fetch works");
    assert_eq!(page.len(), 5);

    // §3.3 — ascending, which is the order they were stored in.
    let ids: Vec<i64> = page.iter().map(|row| row.replay_id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "the fetch orders by ReplayId");

    // And the payloads came back in that order too.
    assert!(
        page[0].raw_payload.contains("REPLAY0"),
        "{}",
        page[0].raw_payload
    );
    assert!(
        page[4].raw_payload.contains("REPLAY4"),
        "{}",
        page[4].raw_payload
    );

    send(&sink, &page).await.expect("the send works");
    assert_eq!(bus.all_sent(QUEUE).len(), 5);

    // §4.1.1 — the original correlation id crossed intact.
    assert_eq!(
        bus.all_sent(QUEUE)[0].properties["CorrelationId"],
        "correlation-0"
    );

    let marked = mark(&mut sql, &page, Status::Loaded)
        .await
        .expect("the mark works");
    assert_eq!(marked, 5);
    assert_eq!(count_with(&mut sql, batch_id, Status::Loaded).await, 5);

    // §1.3 — and a second fetch finds nothing, so the job would finish.
    let second = fetch(&mut sql, batch_id, 100).await.unwrap();
    assert!(second.is_empty(), "Loaded rows are not fetched again");
}

#[tokio::test]
async fn a_send_failure_leaves_the_rows_failed() {
    // §5.2.1 and §8.4.
    let (mut sql, batch_id) = seeded(3).await;

    let page = fetch(&mut sql, batch_id, 100).await.unwrap();
    assert_eq!(page.len(), 3);

    let error = send(&Refusing, &page).await.expect_err("the sink refuses");
    assert!(error.to_string().contains("refusing"), "{error}");

    mark_failed(&mut sql, &page).await;

    assert_eq!(count_with(&mut sql, batch_id, Status::Failed).await, 3);
    assert_eq!(count_with(&mut sql, batch_id, Status::Loaded).await, 0);
}

#[tokio::test]
async fn a_rerun_picks_the_failed_rows_up_again() {
    // §2.2 and §8.4 — the entire recovery procedure. If this did not work,
    // a failed replay would need someone to write SQL by hand at the worst
    // possible moment.
    let (mut sql, batch_id) = seeded(4).await;

    // First run: everything fails.
    let page = fetch(&mut sql, batch_id, 100).await.unwrap();
    let _ = send(&Refusing, &page).await;
    mark_failed(&mut sql, &page).await;
    assert_eq!(count_with(&mut sql, batch_id, Status::Failed).await, 4);

    // Second run: the same rows come back, because §3's fetch selects
    // `Failed` as well as `Pending`.
    let bus = FakeBus::new();
    let sink: FakeSender = bus.sender(QUEUE);

    let again = fetch(&mut sql, batch_id, 100).await.unwrap();
    assert_eq!(again.len(), 4, "the Failed rows are fetched again");
    assert_eq!(
        again.iter().map(|row| row.replay_id).collect::<Vec<_>>(),
        page.iter().map(|row| row.replay_id).collect::<Vec<_>>(),
        "and they are the same rows, in the same order"
    );

    send(&sink, &again).await.expect("this time it works");
    mark(&mut sql, &again, Status::Loaded).await.unwrap();

    assert_eq!(count_with(&mut sql, batch_id, Status::Loaded).await, 4);
    assert_eq!(count_with(&mut sql, batch_id, Status::Failed).await, 0);
}

#[tokio::test]
async fn a_page_is_limited_by_the_batch_size() {
    // §3.2 — `TOP (?)` is `REPLAY_BATCH_SIZE`, parameterised. A job with a
    // small page size loops rather than pulling everything into memory.
    let (mut sql, batch_id) = seeded(7).await;

    let first = fetch(&mut sql, batch_id, 3).await.unwrap();
    assert_eq!(first.len(), 3);

    mark(&mut sql, &first, Status::Loaded).await.unwrap();

    let second = fetch(&mut sql, batch_id, 3).await.unwrap();
    assert_eq!(second.len(), 3);
    assert!(
        second[0].replay_id > first[2].replay_id,
        "the second page continues where the first stopped"
    );

    mark(&mut sql, &second, Status::Loaded).await.unwrap();

    let third = fetch(&mut sql, batch_id, 3).await.unwrap();
    assert_eq!(third.len(), 1, "the remainder");
}

#[tokio::test]
async fn a_batch_only_sees_its_own_rows() {
    // §1.2 — one `REPLAY_BATCH_ID` per execution. A job that pulled another
    // batch's rows would replay messages nobody asked for, onto a live
    // clinical feed.
    let (mut sql, mine) = seeded(3).await;
    let (_, theirs) = seeded(4).await;

    assert_eq!(fetch(&mut sql, mine, 100).await.unwrap().len(), 3);
    assert_eq!(fetch(&mut sql, theirs, 100).await.unwrap().len(), 4);
    assert!(
        fetch(&mut sql, Uuid::new_v4(), 100)
            .await
            .unwrap()
            .is_empty(),
        "a batch nobody seeded is empty rather than everything"
    );
}

#[tokio::test]
async fn an_empty_mark_is_a_no_op() {
    // §6.3 — rather than a statement with an empty `IN ()`, which is a
    // syntax error the job would abort on.
    let (mut sql, _) = seeded(0).await;

    assert_eq!(mark(&mut sql, &[], Status::Loaded).await.unwrap(), 0);
}
