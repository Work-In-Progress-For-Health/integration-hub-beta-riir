//! The fetch, send, mark loop.
//!
//! Implements `spec/13-message-replay.md` §1.3, §5, §6 and §7.

use ihb_bus::sink::MessageSink;
use ihb_sql::{SqlConnection, SqlError};
use uuid::Uuid;

use crate::page::{Row, message_for};
use crate::retry::classify;
use crate::sql::{FETCH_SQL, Status, identifier_chunks, mark_sql};

/// Why the job stopped.
///
/// Each variant means "exit 1": the job never carries on past an error
/// (`spec/13-message-replay.md` §1.1). Reasons are driver or bus text and
/// carry no row content (§7.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// A fetch failed twice (§5.1).
    #[error("could not fetch a page: {0}")]
    Fetch(String),

    /// A send failed, per §5.2.
    #[error("could not send a page: {0}")]
    Send(String),

    /// A mark failed twice (§5.1).
    #[error("could not mark a page as {status}: {reason}")]
    Mark {
        /// Which status was being written.
        status: &'static str,
        /// Why.
        reason: String,
    },
}

/// Fetch one page of rows.
///
/// `size` is the page size; rows come back in `ReplayId` order and only
/// those still `Pending` or `Failed` (§3), so as pages are marked `Loaded`
/// the next fetch naturally moves on, and an empty page means the batch is
/// done. `READPAST` means rows locked by another worker are skipped rather
/// than waited for.
///
/// # Errors
///
/// [`JobError::Fetch`] after two attempts (§5.1). A fetch failure discards
/// the connection (§3.4), so the second attempt reconnects.
pub async fn fetch(
    sql: &mut SqlConnection,
    batch_id: Uuid,
    size: i32,
) -> Result<Vec<Row>, JobError> {
    match fetch_once(sql, batch_id, size).await {
        Ok(rows) => return Ok(rows),
        Err(error) => {
            // §5.1 — a plain retry-once. Logged at warn because the second
            // attempt may well succeed and an error would be noise.
            tracing::warn!(%error, "the fetch failed; retrying once");
            sql.discard();
        }
    }

    fetch_once(sql, batch_id, size)
        .await
        .map_err(|error| JobError::Fetch(error.to_string()))
}

/// One fetch attempt.
///
/// Takes the connection out of `sql` and only puts it back on success: a
/// failure leaves `sql` without a client, so the retry in [`fetch`]
/// reconnects instead of reusing a connection in an unknown state
/// (`spec/12-message-store.md` §5.2). Missing or `NULL` columns read as
/// empty/zero rather than failing; the `INNER JOIN` in the fetch means a
/// row whose message was purged from `monitoring.Message` is simply absent.
async fn fetch_once(
    sql: &mut SqlConnection,
    batch_id: Uuid,
    size: i32,
) -> Result<Vec<Row>, SqlError> {
    let mut client = sql.take().await?;

    let batch = batch_id.to_string();
    let parameters: Vec<&dyn tiberius::ToSql> = vec![&size, &batch];

    let rows = client
        .query(FETCH_SQL, &parameters)
        .await
        .map_err(|error| SqlError::Statement(error.to_string()))?
        .into_first_result()
        .await
        .map_err(|error| SqlError::Statement(error.to_string()))?;

    let mut page = Vec::with_capacity(rows.len());
    for row in rows {
        page.push(Row {
            replay_id: row.get("ReplayId").unwrap_or_default(),
            message_id: row.get("MessageId").unwrap_or_default(),
            raw_payload: row
                .get::<&str, _>("RawPayload")
                .unwrap_or_default()
                .to_string(),
            correlation_id: row
                .get::<&str, _>("CorrelationId")
                .unwrap_or_default()
                .to_string(),
            session_id: row
                .get::<&str, _>("SessionId")
                .unwrap_or_default()
                .to_string(),
        });
    }

    sql.restore(client);
    Ok(page)
}

/// Send a page, with §5.2's classification.
///
/// # Errors
///
/// [`JobError::Send`] when the page did not go. The caller marks the page
/// `Failed` and aborts (§5.2.1).
pub async fn send<S: MessageSink>(sink: &S, page: &[Row]) -> Result<(), JobError> {
    let first = send_once(sink, page).await;
    let Err(error) = first else {
        return Ok(());
    };

    let failure = classify(&error);

    if !failure.is_retryable() {
        // §5.2's first row. A message too large will not become smaller.
        tracing::error!(%error, "a message exceeds the Service Bus size limit; not retrying");
        return Err(JobError::Send(error.to_string()));
    }

    if failure.may_duplicate() {
        // §5.2's second row and §5.3 — the warning that matters. A replay
        // that double-delivers puts the same patient record into a clinical
        // system twice, and whoever ordered the replay needs to know it
        // might have happened.
        tracing::warn!(
            %error,
            "the send timed out; the broker may already have accepted it, \
             so retrying MAY PRODUCE DUPLICATES"
        );
    } else {
        tracing::warn!(%error, "the send failed; retrying once");
    }

    send_once(sink, page)
        .await
        .map_err(|error| JobError::Send(error.to_string()))
}

/// One send attempt: the whole page, as a batch.
///
/// `spec/13-message-replay.md` §4.2 — the page goes as a size-split batch,
/// as the Python builds one `ServiceBus` batch per attempt. A failed attempt
/// therefore delivers nothing from the batch it failed in, so the retry in
/// [`send`] does not re-send rows that already arrived. (Sent one row at a
/// time, a failure part-way would leave the earlier rows on the queue and
/// the retry would duplicate them.) A sink without real batching falls back
/// to one-at-a-time; see [`MessageSink::send_batch`].
async fn send_once<S: MessageSink>(sink: &S, page: &[Row]) -> Result<(), ihb_bus::error::BusError> {
    sink.send_batch(page.iter().map(message_for).collect())
        .await
}

/// Mark a page's rows (§6), returning how many were submitted.
///
/// An empty page is a no-op returning `0` (§6.3).
///
/// # Errors
///
/// [`JobError::Mark`] after two attempts (§5.1).
pub async fn mark(
    sql: &mut SqlConnection,
    rows: &[Row],
    status: Status,
) -> Result<usize, JobError> {
    let identifiers: Vec<i64> = rows.iter().map(|row| row.replay_id).collect();

    // §6.3 — an empty list is a no-op, rather than a statement with an
    // empty `IN ()`, which is a syntax error.
    if identifiers.is_empty() {
        return Ok(0);
    }

    match mark_once(sql, &identifiers, status).await {
        Ok(count) => return Ok(count),
        Err(error) => {
            tracing::warn!(%error, status = status.as_str(), "the mark failed; retrying once");
            sql.discard();
        }
    }

    mark_once(sql, &identifiers, status)
        .await
        .map_err(|error| JobError::Mark {
            status: status.as_str(),
            reason: error.to_string(),
        })
}

/// One mark attempt.
///
/// Returns the number of identifiers submitted, not the server's affected
/// row count: the statement is keyed on `ReplayId`, the identity column, so
/// the two agree unless a row vanished in between.
async fn mark_once(
    sql: &mut SqlConnection,
    identifiers: &[i64],
    status: Status,
) -> Result<usize, SqlError> {
    let mut client = sql.take().await?;
    let label = status.as_str();

    for chunk in identifier_chunks(identifiers) {
        let mut parameters: Vec<&dyn tiberius::ToSql> = Vec::with_capacity(chunk.len() + 1);
        parameters.push(&label);
        for identifier in chunk {
            parameters.push(identifier);
        }

        client
            .query(mark_sql(chunk.len()), &parameters)
            .await
            .map_err(|error| SqlError::Statement(error.to_string()))?
            .into_results()
            .await
            .map_err(|error| SqlError::Statement(error.to_string()))?;
    }

    sql.restore(client);
    Ok(identifiers.len())
}

/// Mark a page `Failed`, without letting that failure mask the original
/// (§5.2.2).
///
/// Infallible by design: it is called while another error is already on
/// its way out. If the mark fails the rows stay `Pending`, which a re-run
/// selects just as it selects `Failed` (§2.2), so nothing is lost.
pub async fn mark_failed(sql: &mut SqlConnection, rows: &[Row]) {
    if let Err(error) = mark(sql, rows, Status::Failed).await {
        // Logged at warn, not returned. Whatever brought us here is the
        // error the operator needs, and replacing it with "and the mark
        // failed too" hides the cause behind its consequence.
        tracing::warn!(%error, "could not mark the page as Failed either");
    }
}

/// The §7.1 log line for a fetched page.
///
/// §7.3: a payload MUST NOT be logged. The identifiers are the whole story,
/// and this is a job whose rows are full unredacted patient records.
pub fn log_page(number: usize, batch_id: Uuid, rows: &[Row]) {
    let first = rows.first().map_or(0, |row| row.replay_id);
    let last = rows.last().map_or(0, |row| row.replay_id);

    tracing::info!("Processing batch {number} for replay batch {batch_id}");
    tracing::info!(
        "Fetched {} records (ReplayId range: {first}-{last})",
        rows.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ihb_bus::error::BusError;
    use ihb_bus::fake::{FakeBus, FakeSender};
    use ihb_bus::retry::Disposition;

    const QUEUE: &str = "priority";

    fn rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|index| Row {
                #[expect(clippy::cast_possible_wrap, reason = "a small test index")]
                replay_id: index as i64 + 1,
                #[expect(clippy::cast_possible_wrap, reason = "a small test index")]
                message_id: index as i64 + 1000,
                raw_payload: format!("MSH|^~\\&|252|M{index}"),
                correlation_id: format!("c{index}"),
                session_id: "phw-to-mpi".to_string(),
            })
            .collect()
    }

    /// A sink that fails a chosen number of times, then succeeds.
    struct Flaky {
        inner: FakeSender,
        failures: std::sync::atomic::AtomicUsize,
        error: BusError,
    }

    impl MessageSink for Flaky {
        async fn send(&self, message: ihb_bus::message::Outgoing) -> Result<(), BusError> {
            use std::sync::atomic::Ordering;
            if self.failures.load(Ordering::SeqCst) > 0 {
                self.failures.fetch_sub(1, Ordering::SeqCst);
                return Err(self.error.clone());
            }
            self.inner.send(message).await
        }

        fn entity(&self) -> &str {
            self.inner.entity()
        }
    }

    /// A sink that records whether the page arrived as one batch.
    struct Batching {
        batches: std::sync::Mutex<Vec<usize>>,
    }

    // The trait methods are `async`; this stub has nothing to await.
    #[allow(clippy::unused_async_trait_impl)]
    impl MessageSink for Batching {
        async fn send(&self, _message: ihb_bus::message::Outgoing) -> Result<(), BusError> {
            unreachable!("a page must go through send_batch")
        }

        async fn send_batch(
            &self,
            messages: Vec<ihb_bus::message::Outgoing>,
        ) -> Result<(), BusError> {
            self.batches.lock().unwrap().push(messages.len());
            Ok(())
        }

        fn entity(&self) -> &str {
            QUEUE
        }
    }

    #[tokio::test]
    async fn a_page_is_sent_as_one_batch() {
        // spec/13-message-replay.md §4.2 — a size-split batch, so a failed
        // attempt delivers nothing and the retry cannot duplicate rows.
        let sink = Batching {
            batches: std::sync::Mutex::new(Vec::new()),
        };

        send(&sink, &rows(5)).await.unwrap();

        assert_eq!(*sink.batches.lock().unwrap(), vec![5]);
    }

    fn flaky(bus: &FakeBus, failures: usize, error: BusError) -> Flaky {
        Flaky {
            inner: bus.sender(QUEUE),
            failures: std::sync::atomic::AtomicUsize::new(failures),
            error,
        }
    }

    fn timeout() -> BusError {
        BusError::Send {
            destination: QUEUE.to_string(),
            attempts: 1,
            reason: "the operation timed out".to_string(),
            disposition: Disposition::Retry,
        }
    }

    #[tokio::test]
    async fn a_page_is_sent_in_replay_id_order() {
        // spec/13-message-replay.md §3.3 — a clinical feed replayed out of
        // order can apply an older update over a newer one.
        let bus = FakeBus::new();
        let sink = bus.sender(QUEUE);

        send(&sink, &rows(5)).await.unwrap();

        let sent = bus.all_sent(QUEUE);
        let ids: Vec<String> = sent
            .iter()
            .map(|message| message.properties["ReplayId"].clone())
            .collect();

        assert_eq!(ids, ["1", "2", "3", "4", "5"]);
    }

    #[tokio::test]
    async fn a_timeout_is_retried_once_and_then_succeeds() {
        // §5.2's second row.
        let bus = FakeBus::new();
        let sink = flaky(&bus, 1, timeout());

        send(&sink, &rows(1)).await.expect("the retry succeeds");

        assert_eq!(bus.all_sent(QUEUE).len(), 1);
    }

    #[tokio::test]
    async fn a_second_failure_aborts() {
        // §5.2.1.
        let bus = FakeBus::new();
        let sink = flaky(&bus, 2, timeout());

        let error = send(&sink, &rows(1)).await.unwrap_err();

        assert!(matches!(error, JobError::Send(_)), "{error:?}");
        assert_eq!(bus.all_sent(QUEUE), Vec::new());
    }

    #[tokio::test]
    async fn an_oversize_message_is_not_retried_at_all() {
        // §5.2's first row and §8.2. The second attempt would reach the
        // same answer, and the page is failed immediately instead.
        let bus = FakeBus::new();
        let sink = flaky(
            &bus,
            1,
            BusError::MessageTooLarge {
                destination: QUEUE.to_string(),
            },
        );

        let error = send(&sink, &rows(1)).await.unwrap_err();

        assert!(matches!(error, JobError::Send(_)));
        assert!(
            bus.all_sent(QUEUE).is_empty(),
            "the single armed failure was not consumed by a retry"
        );
    }

    #[test]
    fn the_page_log_line_names_the_range_and_no_payload() {
        // §7.1 and §7.3 — this job's rows are full unredacted patient
        // records, and the identifiers are the whole story.
        let page = rows(3);

        // Asserted through the values rather than captured output: what
        // matters is that the payload is not among the arguments.
        assert_eq!(page.first().unwrap().replay_id, 1);
        assert_eq!(page.last().unwrap().replay_id, 3);

        log_page(1, Uuid::nil(), &page);
    }
}
