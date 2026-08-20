# 04 — Observability

Event logging, PII redaction, metrics, tracing, and trace propagation.
Implemented by `crates/ihb-telemetry`.

Reference implementation:
`Integration-Hub-Beta/shared_libs/event_logger_lib/`,
`Integration-Hub-Beta/shared_libs/metric_sender_lib/`,
`Integration-Hub-Beta/shared_libs/otel_lib/`.

Every name in this document — event types, metric names, dimension keys,
field names — is consumed by Log Analytics queries, by the NOC dashboard,
and by the alert rules in `Integration-Hub-Beta/docs/ALERT_STRATEGY.md`.
**They MUST NOT change.**

---

## §1 Events

### §1.1 Event types

`event_logger_lib/log_event.py :: EventType`. Serialised as the exact
strings below.

| Value | Emitted when |
| --- | --- |
| `MESSAGE_RECEIVED` | A message arrives at a component |
| `MESSAGE_PROCESSED` | A component finished with a message successfully |
| `MESSAGE_SENT` | Reserved; emitted by no current component |
| `MESSAGE_FAILED` | A component could not process a message |
| `VALIDATION_SUCCESS` | A validation step passed |
| `VALIDATION_FAILED` | A validation step failed |

### §1.2 Event shape

`event_logger_lib/log_event.py :: LogEvent`, flattened into the log record's
structured fields.

| Field | Type | Notes |
| --- | --- | --- |
| `workflow_id` | string | `WORKFLOW_ID` |
| `microservice_id` | string | `MICROSERVICE_ID` |
| `event_type` | string | §1.1 |
| `timestamp` | string | RFC 3339, UTC |
| `message_content` | string | **Redacted** per §2 unless redaction is disabled |
| `validation_result` | string? | Human-readable outcome |
| `error_details` | string? | Present on `MESSAGE_FAILED` |
| `correlation_id` | string? | Absent when unknown; never the literal `N/A` |
| `otel_trace_id` | string? | 32 hex characters, when a span is active |
| `otel_span_id` | string? | 16 hex characters, when a span is active |

§1.2.1 The log message text is the literal `Integration Hub Event`. When
telemetry export is disabled the whole event is rendered into the message
text as `Integration Hub Event: {…}`; when enabled it is the message text
plus structured fields. Log Analytics queries match on this string.

§1.2.2 An event whose emission fails MUST log the failure at error and
re-raise (`event_logger.py :: _send_log_event`), because a component that
silently stops producing audit events is worse than one that crashes.

§1.2.2a **In Rust this clause is vacuous, and deliberately so.** Emission is
a `tracing` macro call, which cannot fail: there is no fallible step to
catch and nothing a caller could do if there were. The Python's `try`/
`raise` existed because `configure_azure_monitor` could throw during a log
call. `ihb-telemetry` initialises the exporter once, from one place
(§6.5), so that failure mode does not exist on the emission path. The intent
of §1.2.2 — never silently stop producing audit events — is met by
construction rather than by error handling.

### §1.3 The event API

`ihb-telemetry::events::EventLogger` MUST expose:

| Method | Event type |
| --- | --- |
| `log_message_received(content, validation_result?, correlation_id?)` | `MESSAGE_RECEIVED` |
| `log_message_processed(content, validation_result?, correlation_id?)` | `MESSAGE_PROCESSED` |
| `log_message_failed(content, error_details, validation_result?, correlation_id?)` | `MESSAGE_FAILED` |
| `log_validation_result(content, validation_result, is_success, correlation_id?)` | `VALIDATION_SUCCESS` or `VALIDATION_FAILED` |

---

## §2 Redaction

`event_logger_lib/redaction.py`. This section is a patient-safety
requirement; see [`index.md`](index.md) §0.8.

§2.1 Redaction is **on by default** and is disabled only by
`HL7_LOG_REDACTION_ENABLED` resolving to false.

§2.2 The redaction mask is the literal `***`.

§2.3 **Empty or whitespace-only input is returned unchanged.**

§2.4 **Input that is not a recognisable ER7 message MUST be replaced
entirely by the mask.** Recognisable means: after normalising `\n` to `\r`
and dropping empty segments, the first segment starts with `MSH` and is at
least 4 **characters** long — characters, not bytes; see §2.5a. Anything else — JSON, XML, a fragment, a partial
frame — is masked whole, because its content is unknown.

§2.5 For a recognisable message, the field separator is taken from the
fourth character of the `MSH` segment, not assumed to be `|`.

§2.5a **The separator is the fourth *character*, not the fourth byte.**
Taking it as a byte panics when that byte falls inside a multi-byte
character, which `MSH` followed by any non-ASCII character produces. Found
by fuzzing in seconds with a five-byte input
([`17-testing-and-quality.md`](17-testing-and-quality.md) §10.5); an ingress
server reads whatever arrives on a socket, so a five-byte frame must not be
able to stop one. A header with fewer than four characters is masked whole
per §2.4.

§2.6 **MSH allow-list.** In the `MSH` segment, these HL7 field numbers are
retained verbatim; every other populated field is masked:

```
MSH-2  encoding characters      MSH-9   message type
MSH-3  sending application      MSH-10  message control id
MSH-4  sending facility         MSH-11  processing id
MSH-5  receiving application    MSH-12  version id
MSH-6  receiving facility
MSH-7  date/time of message
```

`MSH-8` (security) is **not** in the list and is masked. `MSH-1` is
structural: it is the separator itself and is reconstructed on join.

§2.7 In every other segment, the segment identifier is retained and all
fields are masked.

§2.8 **An empty field stays empty.** Only a populated field becomes `***`.
This preserves field positions so a reader can see the shape of a message
without seeing its content.

§2.9 Segments are rejoined with `\r`.

§2.10 Worked example — the input

```
MSH|^~\&|252|252|100|100|20260729095037|SEC|ADT^A28^ADT_A05|MSG1|P|2.5
PID|1||B0000010612^^^328^PI||SURNAME^FORENAME||20010909|M
```

redacts to

```
MSH|^~\&|252|252|100|100|20260729095037|***|ADT^A28^ADT_A05|MSG1|P|2.5
PID|***||***||***||***|***
```

§2.11 A future extension to JSON and XML payloads is anticipated but **not
specified**; until it is, a non-ER7 payload is masked whole per §2.4.

### §2.13 Open finding — `MSH-10` can carry an NHS number

**Status: reproduce as-is, raise with the Hub team. Do not change
unilaterally.**

`MSH-10` (message control id) is on the §2.6 allow-list, on the reasonable
assumption that a control id is an opaque identifier. For the **PHW flow it
is not**. In `local/sample_messages/phw-to-mpi.sample.hl7`:

```
MSH-10  = 202505052323364444444444
          └── 20250505232336 ─┘└ 4444444444 ┘
                 timestamp        NHS number
PID-3   = 8888888^^^252^PI~4444444444^^^NHS^NH
```

The sending system builds the control id by concatenating a timestamp with
the patient's NHS number, so the NHS number survives redaction and reaches
Azure Monitor in every PHW log event. The other five sample flows do not do
this — their control ids are unrelated to any patient identifier — so this
is one source system's convention rather than a flaw in the allow-list
principle.

**What this specification requires today:** the Rust reproduces the Python
exactly, including retaining `MSH-10`. Narrowing the allow-list would change
what is logged, break Log Analytics queries that join on `MSH-10`, and do it
during a cutover, when a behavioural difference is the last thing anyone
needs to be diagnosing.

**What is owed:** this is a live data-protection question about the Python
in production now, not a rewrite decision. It goes to the Hub team with the
options — leave it, mask `MSH-10` for the PHW flow only, or ask PHW to
change the convention — and this section records their answer. Until then
the tests in `ihb-telemetry` document the behaviour rather than asserting it
is acceptable.

§2.12 `ihb-telemetry` MUST have a test that redacts a payload containing a
distinctive marker (an NHS number, a surname) and asserts the marker is
absent from the result, and a test for each of §2.3, §2.4, §2.6, §2.8.

---

## §3 Metrics

`metric_sender_lib/metric_sender.py`.

### §3.1 Instruments

| Name | Kind | Emitted by |
| --- | --- | --- |
| `messages_received` | counter, +1 | Ingress servers, on receipt |
| `messages_sent` | counter, +1 | Senders, on a positive acknowledgement |
| `retry_delay_seconds` | histogram, unit `s` | The receive loop, per [`03-message-bus.md`](03-message-bus.md) §5.4.4 |

§3.1.1 A counter or histogram is created lazily on first use and cached by
name.

§3.1.2 Additional instruments MAY be added; existing ones MUST NOT be
renamed or change kind.

### §3.2 Dimensions

Every metric carries these four, always:

| Key | Source |
| --- | --- |
| `workflow_id` | `WORKFLOW_ID` |
| `microservice_id` | `MICROSERVICE_ID` |
| `health_board` | `HEALTH_BOARD` |
| `peer_service` | `PEER_SERVICE` |

§3.2.1 Per-call dimensions are merged over these and MAY override them.

§3.2.2 A dimension value MUST NOT be derived from message content beyond
what §3.1 lists. In particular no patient identifier, no name, and no
free-text field ever becomes a dimension — an unbounded dimension is both a
PII leak and a cardinality explosion.

§3.2.3 When a metric dimension is not configured, these defaults apply
(`message_receiver_client.py`): `unknown-workflow`, `unknown-microservice`,
`UNKNOWN`, `service_bus`.

### §3.3 When export is disabled

§3.3.1 With no `APPLICATIONINSIGHTS_CONNECTION_STRING`, a metric MUST be
logged at info as

```
Integration Hub Metric (local log): {key}={value}, attributes: {…}
```

so local runs and tests can assert on metric emission without a collector.

---

## §4 Tracing

`otel_lib/otel.py`.

§4.1 `configure_otel(service_name)` is called once per process, before
anything else. It MUST be idempotent: a second call is a no-op returning
success.

§4.2 `OTEL_SERVICE_NAME` is set to `service_name` only when absent or blank.
A blank value in a container configuration MUST be replaced, not respected.

§4.3 With no connection string, a **no-op tracer provider with no span
processor** is installed. It MUST NOT be an in-memory exporter: the Python
learned this the hard way, because `InMemorySpanExporter` retained every
span for the process lifetime and leaked memory in long-running services.

§4.4 Every auto-instrumentation MUST be disabled. The Python disables
`azure_sdk`, `django`, `fastapi`, `flask`, `psycopg2`, `requests`, `urllib`
and `urllib3` because their per-call metric time series grew without bound
and cost measurable CPU over a container's lifetime. The Rust has no
auto-instrumentation to disable, but MUST NOT add HTTP or SDK
instrumentation to these services without a specification change.

§4.5 Every log record MUST carry `otel_trace_id` and `otel_span_id`. With no
active span they are `"0" * 32` and `"0" * 16` respectively — the zero value
is emitted, not omitted, because a Log Analytics join on a missing column
behaves differently from one on a zero value.

§4.5a **Not yet implemented, deliberately.** §4.5 is satisfied only once a
tracer provider exists to produce a real trace id; until then every record
would carry the zeros, which is a column of no information rather than a
partial implementation. The constants are defined
(`ihb_telemetry::otel::NO_TRACE_ID`, `NO_SPAN_ID`) and the field emission
lands with the OTLP exporter of §6.3, in the same change. Anything that
depends on these fields — a Log Analytics query, a dashboard join — MUST NOT
be built against a Rust container until then. Tracked as a Phase 1 follow-up
in [`tasks.md`](../tasks.md).

### §4.6 Propagation

§4.6.1 On send, W3C trace context MUST be injected into a **copy** of the
application properties; the caller's map MUST NOT be mutated.

§4.6.2 On receive, the trace context is extracted from the application
properties and attached for the duration of the handler, then detached —
including when the handler fails.

§4.6.3 If no `traceparent` is present, the handler MUST log a warning naming
the message id and stating that this attempt will appear under a new
operation id, and pointing at `CorrelationId` as the way to correlate
attempts.

§4.6.4 Non-string property values MUST be dropped from the extraction
carrier per [`03-message-bus.md`](03-message-bus.md) §4.2.

### §4.7 Handler spans

§4.7.1 A message handler is wrapped in a span named
`"{service_name}.process_message"` with attributes:

| Attribute | Value |
| --- | --- |
| `messaging.system` | `azure_service_bus` |
| `messaging.destination` | The queue or `topic/subscription` name |
| `messaging.message_id` | The Service Bus message id, when present |

§4.7.2 A handler failure MUST record the error on the span and set the span
status to error before propagating.

§4.7.3 When tracing is not configured, the handler MUST be invoked with
**zero** wrapping overhead.

---

## §5 Logging

§5.1 Logs are structured, emitted through `tracing`.

§5.2 The default level is per-service, per
[`02-configuration.md`](02-configuration.md) §2. Azure SDK targets default
to `WARN`.

§5.3 The Python's plain-text format is
`%(asctime)s [%(levelname)s] %(message)s`. The Rust default is the
`tracing-subscriber` compact formatter with timestamps, which is close
enough that no downstream consumer depends on the difference; JSON output
MAY be enabled and is preferable in Container Apps.

§5.4 **No log line, at any level, may contain an unredacted payload or a
field value taken from one**, except the allow-list of §2.6. This includes
`debug` and `trace`, which are enabled in production more often than anyone
intends.

---

## §6 Export

§6.1 An empty or absent `APPLICATIONINSIGHTS_CONNECTION_STRING` disables
export entirely, for traces, metrics and events alike. The process MUST
start cleanly and log at info that telemetry is disabled.

§6.2 Export failures MUST NOT stop the process. A failure to initialise the
exporter is logged at error and the process continues with export disabled.

§6.3 Export is via OTLP. There is no Azure Monitor exporter for Rust; see
[`plan.md` §8](../plan.md#8-known-gaps-and-how-they-are-closed).
`ihb-telemetry` MUST keep the exporter behind a trait so an Azure-native
exporter can replace it without touching a call site.

§6.4 The credential for telemetry ingestion is a user-assigned managed
identity when `INSIGHTS_UAMI_CLIENT_ID` is set, and the ambient credential
otherwise.

§6.5 Exactly one component initialises export. The Python's
`AZURE_MONITOR_OWNER` arbitration between `event_logger_lib`,
`metric_sender_lib` and `otel_lib` has no Rust counterpart: `ihb-telemetry`
initialises once, from `configure_otel`, and the variable is accepted and
ignored.
