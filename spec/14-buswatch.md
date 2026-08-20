# 14 — BusWatch

A queue inspector for local development: peek at what is sitting on the
Service Bus, look at one message in detail, and clear a queue. Implemented
by `crates/buswatch`.

Reference implementation: `Integration-Hub-Beta/buswatch/`.

---

## §1 Scope

§1.1 BusWatch is a **developer tool**. It peeks; it does not consume. The
only mutation it offers is clearing a queue, which is a local-emulator
operation.

§1.2 It MUST NOT be deployed into an environment holding real patient data.
Its whole purpose is displaying message bodies, and it applies no redaction.
This is stated here so the decision is explicit rather than assumed.

---

## §2 Configuration

Per [`02-configuration.md`](02-configuration.md) §9.1. Note the variable
name: `SERVICEBUS_CONNECTION_STRING`, without the underscore the rest of the
estate uses. It is preserved so an existing `buswatch.env` keeps working.

§2.1 The default connection string points at
`Endpoint=sb://localhost;…;UseDevelopmentEmulator=true;`, which is right for
a host-run process and wrong inside Compose, where it is overridden to
`sb://sb-emulator`.

§2.2 When `BUSWATCH_QUEUE_NAMES` is blank, queue names are **discovered**
from `local/ServiceBusEmulatorConfig.json`. Discovery reads
`UserConfig.Namespaces[].Queues[].Name`.

§2.3 The Python also reads a `config.ini` beside the package as a fallback.
The Rust does not: environment variables and the discovery of §2.2 are the
only sources.

---

## §3 Pages

| Method | Path | Behaviour |
| --- | --- | --- |
| `GET` | `/` | Queue snapshots (§4) |
| `GET` | `/messages/{queue}/{sequence}` | One message in detail (§5) |
| `POST` | `/queues/{queue}/clear` | Clear a queue, then redirect to `/` |
| `GET` | `/static/*` | Bundled assets |

§3.1 `/` accepts `refresh_queue={name}` to refresh one queue, and
`refresh_all=true` to refresh every queue.

§3.2 Queue snapshots are **cached** per queue. A page render serves the
cache for every queue, refreshing only those missing from it plus any
explicitly requested. This is what keeps the page responsive against an
emulator that answers slowly.

§3.3 Refreshes across queues run concurrently, bounded by a configured
parallelism limit.

§3.4 A clear operation produces a one-shot feedback message, consumed by the
next render and then discarded — a flash message.

---

## §4 Snapshots

§4.1 Each queue snapshot carries: the queue name, its active and
dead-letter message counts, up to `BUSWATCH_PEEK_COUNT` message summaries,
and the time it was taken.

§4.2 A message summary carries its sequence number, enqueued time, message
id, session id, size, and a short preview of the body.

§4.3 Counts come from the Service Bus management API; bodies come from a
peek, which does not lock or consume.

---

## §5 Detail

§5.1 A message is located by scanning from the start of the queue for its
sequence number, up to `BUSWATCH_DETAIL_SEARCH_LIMIT` messages.

§5.2 Not found within the limit is a 404 that says so, naming the limit, so
the reader knows to raise it rather than concluding the message is gone.

§5.3 The detail view shows the full body, every application property, and
the broker properties.

§5.4 When the body is an ER7 message it is rendered segment by segment; when
it is the message-store JSON of
[`03-message-bus.md`](03-message-bus.md) §6 it is pretty-printed. Neither
rendering may fail the page: an unrecognised body is shown raw.

---

## §6 Errors

§6.1 The local emulator has a hard connection quota. An error whose text
contains `connectionsquotaexceeded` or `resource-limit-exceeded` MUST be
surfaced with an actionable hint rather than a stack trace — the Python
detects exactly these two substrings, and the hint is the difference between
a five-minute and a fifty-minute debugging session.

§6.2 A failure to reach one queue MUST NOT fail the page. That queue shows
an error and the rest render.

---

## §7 Implementation notes

§7.1 `axum` for routing, `askama` for templates ported from
`buswatch/buswatch/templates/`, and the bundled static assets copied across
unchanged.

§7.2 The DHCW brand applies: NHS Wales Blue `#325083`, DHCW Blue `#12A3C9`,
DHCW Navy `#1B294A`, DHCW Yellow `#F8CA4D`, and the Rubik typeface.

§7.3 AMQP resources are closed on shutdown.

---

## §8 Tests

§8.1 Unit: §2.2's discovery, against the real
`ServiceBusEmulatorConfig.json`.

§8.2 Unit: §6.1's error classification, both substrings and a negative case.

§8.3 Unit: §5.4's body rendering for ER7, for store JSON, and for an
unrecognised body.

§8.4 Integration: every route renders with a fake reader, including the
empty-queue and unreachable-queue cases.

§8.5 The templates compile: `askama` makes this a build-time guarantee, so
there is no runtime template test to write.
