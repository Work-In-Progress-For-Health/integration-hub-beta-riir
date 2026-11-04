# 13 — Message replay job

A one-shot job that pushes stored messages back onto a priority queue.
Implemented by `crates/message-replay-job` over `crates/ihb-sql`.

Reference implementation: `Integration-Hub-Beta/message_replay_job/`,
`Integration-Hub-Beta/local/MESSAGE_REPLAY.md`.

---

## §1 Shape

§1.1 This is a **job**, not a service: it runs to completion and exits. Exit
code 0 on success, 1 on failure. There is no health check and no signal
handling beyond the default.

§1.2 It processes exactly **one** `REPLAY_BATCH_ID` per execution. Selecting
which messages belong to a batch is an operator action performed in SQL
before the job runs; the job does not choose.

§1.3 It loops: fetch a page of rows, send them, mark them, repeat, until a
fetch returns nothing.

---

## §2 The queue table

```sql
monitoring.MessageReplayQueue (
    ReplayId       -- identity, the ordering key
    MessageId      -- foreign key into monitoring.Message
    ReplayBatchId  -- the batch this row belongs to
    Status         -- 'Pending' | 'Failed' | 'Loaded'
    ProcessedAt    -- set when the status last changed
)
```

§2.1 The status values are exactly `Pending`, `Failed` and `Loaded`,
case-sensitive.

§2.2 `Failed` rows are picked up again by a subsequent run — the fetch in
§3 selects `Pending` **and** `Failed` — so re-running the job is the
recovery procedure.

---

## §3 Fetching

The statement is normative
(`message_replay_job/db_client.py :: FETCH_BATCH_SQL`):

```sql
WITH Batch AS (
    SELECT TOP (?) ReplayId, MessageId
    FROM monitoring.MessageReplayQueue WITH (READPAST)
    WHERE Status IN ('Failed', 'Pending')
    AND ReplayBatchId = ?
    ORDER BY ReplayId
)
SELECT b.ReplayId, m.Id AS MessageId, m.RawPayload, m.CorrelationId, m.SessionId
FROM Batch b
JOIN monitoring.Message m ON m.Id = b.MessageId
ORDER BY b.ReplayId;
```

§3.1 `READPAST` skips locked rows so two workers on the same batch do not
block each other.

§3.2 `TOP (?)` is `REPLAY_BATCH_SIZE`, parameterised.

§3.3 Ordering is by `ReplayId` throughout, so replay preserves the order in
which messages were stored.

§3.4 A fetch failure discards the connection and propagates
([`12-message-store.md`](12-message-store.md) §5.2).

---

## §4 Sending

§4.1 Each row becomes a Service Bus message:

| Part | Value |
| --- | --- |
| body | `RawPayload` |
| `session_id` | `SessionId` from the stored row |
| `application_properties.CorrelationId` | `CorrelationId` from the stored row |
| `application_properties.ReplayId` | `ReplayId`, as a string |
| `application_properties.MessageId` | `MessageId`, as a string |

§4.1.1 The original correlation id is **preserved**, so a replayed message
is traceable to its first passage through the Hub. `ReplayId` and
`MessageId` are how a replayed message is distinguished from the original.

§4.1.2 No `message_id` is set, so Service Bus duplicate detection does not
suppress a deliberate replay.

§4.2 The page is sent as a batch, split by size per
[`03-message-bus.md`](03-message-bus.md) §3.3. The implementation calls
`MessageSink::send_batch`, whose `LiveSender` override is a real Service Bus
batch; a failed attempt therefore delivers nothing from the batch it failed
in, and the §5.2 retry cannot re-send rows that already arrived. (An earlier
revision sent rows one at a time and could. Found in the 2026-10 upstream
sync, corrected 2026-11-04.)

§4.3 **Redirecting a topic-fed sender to the priority queue** (upstream
`8c1d012`, `docs/MESSAGE_REPLAY.md`). A sender reads either a queue or a
topic and subscription, never both
([`02-configuration.md`](02-configuration.md) §6.1.1; the error is
`Invalid ingress configuration: INGRESS_QUEUE_NAME cannot be set together
with INGRESS_TOPIC_NAME/INGRESS_SUBSCRIPTION_NAME. …`). So for a sender whose
`.env` has `INGRESS_TOPIC_NAME`/`INGRESS_SUBSCRIPTION_NAME` — today
`paris-mpi-hl7-sender`, `mosaiq-mpi-hl7-sender` and `risp-mpi-hl7-sender`
([`01-architecture.md`](01-architecture.md) §2.1.3) — the operator MUST
**remove** those (and `INGRESS_ACCEPT_ANY_SESSION`, if set) before setting
`INGRESS_QUEUE_NAME="local-inthub-priority-messagequeue"`; adding the queue
beside them stops the sender at startup. `INGRESS_SESSION_ID` stays as it
is: each replayed message already carries its stored `SessionId` (§4.1).
After the replay the original ingress is restored and the profile restarted.

---

## §5 Retry and failure

`message_replay_job.py`. Every database operation and every send is
attempted **twice**: once, then once more on failure.

§5.1 **Fetch** and **mark-as-loaded** use a plain retry-once. A second
failure logs at error and aborts the job with exit code 1.

§5.2 **Send** classifies the first failure:

| First failure | Behaviour |
| --- | --- |
| A single message exceeds the Service Bus size limit | **No retry.** Mark the page `Failed`, log at error, abort |
| Send timeout | Retry, and log at warn that the broker outcome is unknown and the retry **may produce duplicates** |
| Anything else | Retry, logged at warn |

§5.2.1 A second send failure marks the page `Failed` and aborts.

§5.2.2 A failure to mark a page `Failed` is logged at warn and does not mask
the original error.

§5.3 The duplicate warning in §5.2 is important and MUST be preserved
verbatim in substance: a timed-out send may already have reached the broker,
and the operator needs to know that the replay could double-deliver.

---

## §6 Marking

§6.1 After a page is sent, its rows are marked `Loaded`:

```sql
UPDATE monitoring.MessageReplayQueue
SET Status = ?, ProcessedAt = SYSUTCDATETIME()
WHERE ReplayId IN (?, ?, …);
```

with one placeholder per identifier.

§6.2 The `IN` list is built from parameter placeholders, never from
interpolated values. The row count per statement MUST respect the TDS
parameter limit of [`12-message-store.md`](12-message-store.md) §5.6:
with one parameter for the status plus one per identifier, a statement
carries at most **2099** identifiers.

§6.3 An empty identifier list is a no-op.

---

## §7 Logging

§7.1 Per page, at info:

```
Processing batch {n} for replay batch {uuid}
Fetched {n} records (ReplayId range: {first}-{last})
Batch {n}: {n} records marked as Loaded
```

§7.2 Completion, at info: `No more pending records found. Job complete.`
then `Message replay job finished successfully`.

§7.3 A payload MUST NOT be logged. The identifiers are the whole story.

---

## §8 Tests

§8.1 Unit: §4.1's message construction, every field.

§8.2 Unit: §5.2's classification table, asserting that an oversize message
is not retried and that a timeout is.

§8.3 Unit: §6.2's identifier chunking at the parameter limit.

§8.4 Integration, against a real SQL Server and the fake bus: a batch of
rows is fetched in `ReplayId` order, sent, and marked `Loaded`; a send
failure leaves them `Failed`; a re-run picks the `Failed` rows up again
(§2.2).

§8.5 Integration: the job exits 0 when there is nothing to do, and 1 on an
unrecoverable failure.
