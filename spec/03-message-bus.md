# 03 — Message bus

Azure Service Bus: clients, sessions, metadata, retry, and the message-store
protocol. Implemented by `crates/ihb-bus`.

Reference implementation:
`Integration-Hub-Beta/shared_libs/message_bus_lib/`.

---

## §1 Connection

§1.1 A client is built from exactly one of:

- a **connection string** (`SERVICE_BUS_CONNECTION_STRING`), or
- a **namespace** (`SERVICE_BUS_NAMESPACE`) plus the ambient Azure
  credential.

When both are configured, the connection string wins
(`connection_config.py :: is_using_connection_string`).

§1.2 The namespace form appends `.servicebus.windows.net` and authenticates
with the equivalent of `DefaultAzureCredential`.

§1.3 The AMQP connection MUST be configured with a **30-second keep-alive**.
Without it Azure silently drops idle connections and the next send fails
with a stale-link error (`servicebus_client_factory.py ::
AMQP_KEEP_ALIVE_INTERVAL`).

§1.4 A client MUST be able to rebuild its underlying connection. `ihb-bus`
exposes this as a recreate hook used by §5.3's stale-link recovery; a
failure to close the old connection MUST be logged at warn and MUST NOT
prevent the rebuild.

---

## §2 Clients

`ihb-bus` provides five client types, mirroring the Python factory:

| Client | Reads | Writes |
| --- | --- | --- |
| `QueueSender` | — | A queue, optionally under a session |
| `TopicSender` | — | A topic, optionally under a session |
| `QueueReceiver` | A queue, optionally a session | — |
| `SubscriptionReceiver` | A topic subscription, optionally a session | — |
| `MessageStore` | — | The message-store queue, as structured JSON |

§2.1 Every client is `Send + Sync` and cheap to clone or share. Sends are
serialised internally so a shared sender is safe from several tasks
(`message_sender_client.py` uses a `threading.Lock` for the same reason).

§2.2 `SubscriptionReceiver` MUST report its source as
`"{topic}/{subscription}"` wherever a receiver names its queue — in logs,
in metric dimensions, and in span attributes
(`subscription_receiver_client.py`).

§2.3 Every client MUST close cleanly on drop or on an explicit `close`, and
a close error MUST be logged rather than propagated.

---

## §3 Sending

### §3.1 Message shape

A sent message carries:

| Part | Source |
| --- | --- |
| body | The payload bytes, UTF-8 |
| `message_id` | `MSH-10` at ingress; unset elsewhere unless specified |
| `session_id` | The configured egress session, if any |
| `application_properties` | The tracking metadata of §4, plus W3C trace context |

§3.1.1 An empty `application_properties` map MUST be sent as absent, not as
an empty map.

### §3.2 Send retry

§3.2.1 A send is attempted at most **3 times**
(`message_sender_client.py :: MAX_SERVICE_BUS_RETRIES`).

§3.2.2 Classification of failures:

| Failure | Behaviour |
| --- | --- |
| Operation timeout | Retry, without recording an error |
| Message too large | **Fail immediately.** Never retried; it cannot succeed |
| Other Service Bus error | Retry, remembering the error |
| Stale AMQP link | Recreate the sender, then retry |
| Anything else | Propagate |

§3.2.3 If all attempts are exhausted, the last remembered error is returned.

### §3.3 Batch sending

§3.3.1 `send_batch` adds messages to a batch one at a time. When a message
would exceed the batch size limit, the current batch is flushed and a new
one started with that message.

§3.3.2 A single message that does not fit an *empty* batch is an
unrecoverable error naming the destination, and MUST NOT be retried.

§3.3.3 `send_batch` returns the total number of messages sent.

---

## §4 Tracking metadata

The `application_properties` keys are a cross-service contract. Their exact
spelling is normative (`metadata_utils.py`).

| Key | Value |
| --- | --- |
| `CorrelationId` | UUID v4, minted at ingress, propagated unchanged |
| `MessageReceivedAt` | RFC 3339, UTC, minted at ingress |
| `SourceSystem` | `MSH-3` of the received message, or `""` |
| `WorkflowID` | `WORKFLOW_ID`. Note the capitalisation: `ID`, not `Id` |
| `traceparent`, `tracestate` | W3C trace context |

MPI-outbound routing properties, added by `hl7-server` when
`HL7_VALIDATION_FLOW` is `mpi` (`custom_message_properties.py`):

| Key | Value |
| --- | --- |
| `MessageType` | `MSH-9.2` |
| `UpdateSources` | Pipe-wrapped `PID-2` assigning authority codes |
| `AssigningAuthorities` | Pipe-wrapped `PID-3` assigning authority codes |
| `DateDeath` | `PID-29.1` |
| `ReasonDeath` | `PID-30` |

§4.1 **Pipe wrapping.** A non-empty list of codes renders as
`"|" + join("|") + "|"`; an empty list renders as `""`. The leading and
trailing pipes exist so a SQL `LIKE '%|108|%'` cannot match inside `|1088|`.
This MUST be reproduced exactly, including the empty-list case.

§4.2 **Reading metadata.** Values arrive as AMQP-typed values. A reader MUST
decode `bytes` as UTF-8 and stringify everything else. A property whose
value is an integer, float, bool or null MUST be **excluded** from the
carrier handed to the trace-context extractor, because it is not a valid
propagation header and the W3C propagator will fail on it
(`message_receiver_client.py :: _invoke_with_trace_context`).

§4.3 **Missing metadata.** When a log line reports metadata, an absent value
renders as the literal `N/A` (`metadata_utils.py :: NA`). When a correlation
id is passed to the event logger, `N/A` MUST become absent rather than being
logged as the string `N/A`
(`metadata_utils.py :: correlation_id_for_logger`).

---

## §5 Receiving

### §5.1 Mode

§5.1.1 All receives are **peek-lock**. A message is completed only after its
processing succeeded.

§5.1.2 The maximum wait for a receive is **60 seconds**
(`MAX_WAIT_TIME_SECONDS`). An empty result is not an error.

§5.1.3 A session receiver MUST have automatic lock renewal enabled, with a
renewal duration of **300 seconds** (`LOCK_RENEWAL_DURATION_SECONDS`).

§5.1.4 The lock renewer MUST be shut down **before** the receiver is
closed, waiting for any in-flight renewal to finish. Closing it afterwards
races the receiver's own teardown and surfaces as
`'NoneType' object has no attribute 'create_receiver_link'`. This ordering
is normative; the Python carries a nine-line comment explaining why, and the
reason survives the port.

### §5.2 Dispositions

Two processing modes, matching `receive_messages` and
`receive_messages_batch`:

**Per-message.** Messages are processed one at a time in order.

§5.2.1 On success, the message is completed and the next is processed.

§5.2.2 On failure — a handler returning "not handled", or a panic caught at
the boundary — the failing message **and every message after it in the
batch** are abandoned, and the batch is reported as failed. The messages
behind it are not attempted, because §6.2 of
[`01-architecture.md`](01-architecture.md) requires in-order delivery within
a session.

**Batch.** All received messages are handed to the handler together.

§5.2.3 On success, every message is completed. On failure, every message is
abandoned.

### §5.3 Failure handling in the receive loop

| Condition | Behaviour |
| --- | --- |
| Session cannot be locked | Log at warn, sleep 60 s, return |
| Transient Service Bus error | Log at warn, schedule a retry per §5.4 |
| Stale AMQP session (`create_receiver_link` in the error text) | Close and recreate the client, then schedule a retry |
| Any other error | Log at warn, schedule a retry |

§5.3.1 The receive loop MUST NOT propagate errors to its caller. It absorbs
them, schedules a retry, and returns, so the outer `while running` loop is a
plain loop with no error handling of its own.

### §5.4 Backoff

§5.4.1 Initial delay **5 seconds**; doubled after each failure; capped at
**900 seconds** (15 minutes).

§5.4.2 Any successful batch resets the delay to 5 seconds and the attempt
counter to zero.

§5.4.3 While waiting, the loop sleeps in slices of at most
`MAX_WAIT_TIME_SECONDS` so shutdown stays responsive; if the retry time has
not arrived when the slice ends, the loop returns without receiving.

§5.4.4 Each scheduled retry emits the gauge metric `retry_delay_seconds`
with dimensions `queue` and `attempt`, in addition to the standard
dimensions of [`04-observability.md`](04-observability.md) §3.2.

§5.4.5 When a retry window elapses and the queue returns nothing, the loop
MUST log at info that the message may not yet be re-available, naming the
queue, the attempt number, and how long ago the retry was due. This is the
single most useful line for diagnosing a stalled flow and MUST be preserved.

---

## §6 Message store protocol

The message-store queue carries JSON, not HL7. The document shape is a
contract between every producer and `message-store-service`
(`message_store_client.py :: send_to_store`).

```json
{
  "MessageReceivedAt": "2026-08-20T09:15:00.123456+00:00",
  "CorrelationId": "3f6d…",
  "SourceSystem": "252",
  "ProcessingComponent": "phw_hl7_server",
  "TargetSystem": "MPI",
  "RawPayload": "MSH|^~\\&|…",
  "XmlPayload": "<ADT_A05 xmlns=\"urn:hl7-org:v2xml\">…</ADT_A05>",
  "SessionId": "phw-to-mpi"
}
```

§6.1 Keys are `PascalCase` exactly as shown. `XmlPayload` and
`TargetSystem` are nullable; every other key is required and a consumer MUST
reject a document missing one.

§6.2 `ProcessingComponent` is the producer's `MICROSERVICE_ID`.
`TargetSystem` defaults to the producer's `PEER_SERVICE` when not given
explicitly.

§6.3 `SessionId` is the *producer's* session, recorded so replay can restore
the message onto the right session.

§6.4 When the store is disabled (§2.1.2 of
[`02-configuration.md`](02-configuration.md)), `send_to_store` MUST be a
no-op that logs at debug with the correlation id, and MUST NOT construct a
document.

§6.5 A send failure MUST be logged at error and re-raised by `ihb-bus`;
callers decide whether it is fatal. Per §3.3 of
[`01-architecture.md`](01-architecture.md), ingress and egress callers
swallow it.

---

## §7 Testing requirements

§7.1 `ihb-bus` MUST expose a **fake bus** behind the same traits as the real
clients, in a `testing` feature, so every downstream crate's integration
tests run with no emulator.

§7.2 The following MUST have tests that do not need a broker: metadata
encoding and decoding (§4), pipe wrapping (§4.1), store document shape
(§6), send-retry classification (§3.2.2), batch splitting (§3.3), backoff
schedule (§5.4.1–§5.4.2), and the per-message abandon-the-rest rule
(§5.2.2).

§7.3 The following MUST have tests against the local Service Bus emulator,
run behind a feature flag and skipped when the emulator is absent: session
send and receive, topic and subscription, lock renewal over a processing
window longer than the lock duration, and reconnection after the broker
drops the link.

---

## §8 Phase 0 probe result

`azservicebus` 0.25.1, against
`mcr.microsoft.com/azure-messaging/servicebus-emulator:latest` under Podman,
using this workspace's own `local/ServiceBusEmulatorConfig.json`.

**Result: 7 of 7 capabilities present.**

| Capability | Result |
| --- | --- |
| Session send and receive | Round-tripped on `local-inthub-phw-transformer-ingress`, session `phw-to-mpi`, `message_id` preserved |
| `application_properties` | Three properties round-tripped, values readable as strings |
| Topic and subscription | Published to `mpihl7input`, received on `mpihl7input-test-subscription` under its session |
| Abandon raises `delivery_count` | `Some(1)` then `Some(2)` — §2.2 of [`10-senders.md`](10-senders.md) can rely on it |
| Batch send | 50 messages in one batch, 50 received back |
| Lock renewal | `renew_session_lock` and `renew_message_lock` both work; lock moved to 59 s ahead |
| Dead-letter sub-queue | Dead-lettered, then read back through `SubQueue::DeadLetter` |

### §8.1 Three findings, each with a consequence for `ihb-bus`

§8.1.1 **`UseDevelopmentEmulator` is ignored.** `azservicebus` does not
parse that connection-string flag at all; it dials TLS on 5671 regardless,
so a connection string that works for the Python fails here with a bare
`Connection refused`. The emulator speaks plaintext AMQP on 5672.

The way in is the crate's `unsecured` feature and its
`WithCustomRetryPolicy::<_>::unsecured()` builder. `ihb-bus` MUST therefore:

- depend on `azservicebus` with `default-features = false` and features
  `["rustls", "unsecured"]` — `rustls` because
  [`17-testing-and-quality.md`](17-testing-and-quality.md) §8.2 forbids
  `openssl`, and `unsecured` for the emulator;
- detect `UseDevelopmentEmulator=true` in the connection string **itself**
  and select the unsecured builder when it is present;
- **never** select the unsecured builder otherwise. A plaintext connection
  to a real namespace would put clinical messages on the wire unencrypted,
  so this MUST be driven by the emulator flag alone and MUST have a test
  asserting a production-shaped connection string selects TLS.

§8.1.2 **There is no `AutoLockRenewer`.** The Python's
`azure.servicebus.AutoLockRenewer` has no counterpart: renewal is manual,
through `renew_session_lock` and `renew_message_lock`. §5.1.3's automatic
renewal is therefore something `ihb-bus` implements — a task alongside the
receive loop that renews at an interval shorter than the lock duration and
stops before the receiver is disposed.

§5.1.4's ordering requirement survives the change and gets easier to honour:
with renewal as our own task, "shut the renewer down before closing the
receiver" is a `JoinHandle` to await rather than a library's internal race.

§8.1.3 **A session lock is not released the instant `dispose` returns.**
Accepting the same session again immediately fails with
`com.microsoft:session-cannot-be-locked`. This is not a defect in the crate
or the emulator — it is how session ownership works — but it is a real
behaviour on every reconnect, so `ihb-bus`'s §5.3 stale-session recovery MUST
treat `session-cannot-be-locked` as retryable with backoff rather than
fatal, which §5.3 already requires. The probe needed 20 attempts at 500 ms to
make its own reuse reliable.

### §8.2 Not measured

Reconnection after the broker drops the link. The emulator gives no
convenient way to sever a connection mid-flight; that case is covered by
§5.3's stale-session recovery and is tested in `ihb-bus` by faulting the
transport rather than the broker.
