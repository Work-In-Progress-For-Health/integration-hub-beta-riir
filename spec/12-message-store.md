# 12 — Message store service

Drains the message-store queue into Azure SQL. Implemented by
`crates/message-store-service` over `crates/ihb-sql`.

Reference implementation: `Integration-Hub-Beta/message_store_service/`.

---

## §1 Responsibility

Every message that crosses the Hub is persisted, so that it can be
inspected, audited, and replayed. This service is the only writer of
`monitoring.Message`.

§1.1 It receives **batches** from a non-session queue and inserts them in
one transaction. Batch semantics are all-or-nothing: either every record in
the batch is committed, or none is and the whole batch is abandoned.

---

## §2 The table

```sql
monitoring.Message (
    Id                   -- identity, assigned by the database
    ReceivedAt           -- datetime2, from the event's MessageReceivedAt
    StoredAt             -- datetime2, UTC now at insert time
    CorrelationId        -- the Hub correlation id
    SourceSystem         -- MSH-3 of the original message
    ProcessingComponent  -- the producing component's MICROSERVICE_ID
    TargetSystem         -- nullable
    RawPayload           -- the ER7, in full and unredacted
    XmlPayload           -- nullable, the v2.xml rendering
    SessionId            -- the producing component's Service Bus session
)
```

§2.1 The insert statement is parameterised, exactly:

```sql
INSERT INTO monitoring.Message (
    ReceivedAt, StoredAt, CorrelationId, SourceSystem,
    ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
```

§2.2 `StoredAt` is computed **once per batch**, not per row, so a batch
shares an insert timestamp.

§2.3 `RawPayload` holds the message in full. This is the one place patient
data is stored deliberately; see [`index.md`](index.md) §0.8.2.

---

## §3 Building a record

`message_record_builder.py`.

§3.1 The body is parsed as the JSON document of
[`03-message-bus.md`](03-message-bus.md) §6.

§3.2 A body that is not valid JSON is an error:
`Invalid JSON in message body`, logged with a backtrace.

§3.3 These keys are **required**; a missing one is an error
`Missing required field: {key}`, logged with the correlation id or `UNKNOWN`:

```
MessageReceivedAt  CorrelationId  SourceSystem  ProcessingComponent
RawPayload         SessionId
```

§3.4 `TargetSystem` and `XmlPayload` are optional and become SQL nulls when
absent.

§3.5 `MessageReceivedAt` is parsed as ISO-8601. A value that does not parse
is an error `Invalid received_at timestamp: {value!r}` — note that the
*value* appears in this message. It is a timestamp, not patient data, so
this is permitted; nothing else from the document may appear in an error.

§3.6 A failure building **any** record fails the whole batch (§4.2).

---

## §4 The loop

§4.1 Receive `MAX_BATCH_SIZE` messages, build records for all of them,
insert them in one transaction, and complete every message.

§4.2 On any failure — a malformed document, a database error — the batch is
logged at error with
`Failed to process batch of {n} message(s): {e}`, a `MESSAGE_FAILED` event
is emitted with content `Batch of {n} message(s)`, error details
`Batch processing failed: {e}` and validation result `Batch insert failed`,
and **every message in the batch is abandoned**.

§4.2.1 The event content is the literal string `Batch of {n} message(s)`,
never the batch's payloads.

§4.3 A single poison document therefore blocks its batch until Service Bus
dead-letters it at `MaxDeliveryCount`. This is the Python's behaviour. It
is a known weakness — one bad document costs 10 retries of up to 99 good
ones — and is recorded here rather than fixed, because changing it changes
the failure semantics operators are used to. Revisit with the Hub team.

§4.4 The `WorkflowID` used by this service's event logger is the literal
`message_store`, not an environment variable.

---

## §5 Database access

`ihb-sql`, shared with [`13-message-replay.md`](13-message-replay.md).

§5.1 **One lazily-opened, long-lived connection**, reused across batches.

§5.2 On any database error the connection is closed and discarded, so the
next call reconnects transparently. A rollback is attempted first, and a
rollback failure is logged at warn rather than masking the original error.

§5.3 The transaction is explicit: autocommit off, commit on success,
rollback on failure. The rollback matters — without it, an abandoned batch
could be re-delivered and re-inserted, producing duplicates.

§5.4 **Authentication.**

| `SQL_USERNAME` | `MSSQL_SA_PASSWORD` | Mode |
| --- | --- | --- |
| set | set | Password |
| unset | unset | Managed identity |
| exactly one set | | **Startup failure**, naming the missing one |

§5.4.1 Managed identity: an access token for
`https://database.windows.net/` is obtained from the ambient credential —
or from the user-assigned identity named by `MANAGED_IDENTITY_CLIENT_ID` —
and presented to the server as an AAD token. This replaces the Python's
`Authentication=ActiveDirectoryMsi` ODBC parameter.

§5.4.2 The token MUST be refreshed before expiry; a connection rebuilt per
§5.2 acquires a fresh one.

§5.5 `SQL_ENCRYPT` and `SQL_TRUST_SERVER_CERTIFICATE` map to the TDS
encryption mode and certificate-trust setting. `Yes`/`No`, case-insensitive.

§5.6 **Parameter chunking.** TDS allows at most 2100 parameters per
statement. With 9 parameters per row, a batch is split at **233 rows**
per statement, all statements sharing one transaction. `MAX_BATCH_SIZE` of
100 never reaches this, but the limit MUST be enforced rather than assumed
([`plan.md` §9.6](../plan.md#9-deliberate-deviations-from-the-python)).

§5.7 A connection string, a username, or a password MUST NOT appear in any
log line or error, at any level.

---

## §6 Tests

§6.1 Unit: §3's builder, for a valid document, each missing required key,
invalid JSON, and an unparseable timestamp.

§6.2 Unit: §3.4's optional fields becoming nulls.

§6.3 Unit: §5.6's chunking arithmetic, at 232, 233 and 234 rows.

§6.4 Unit: §5.4's authentication-mode selection, including the mismatch
failure.

§6.5 Integration, against a real SQL Server in a Podman container: a batch is inserted
and committed; a failing insert rolls back and leaves no rows; a discarded
connection is transparently reopened on the next call.

§6.6 Integration: a poison document abandons its whole batch and the good
messages are re-delivered (§4.2).

§6.7 A test asserting no credential appears in any captured log output
(§5.7).
