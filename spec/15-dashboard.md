# 15 — NOC dashboard

The operations dashboard: flow health, queue depths, exceptions, message
throughput, and three alarm rules with e-mail escalation. Implemented by
`crates/dashboard`.

Reference implementation: `Integration-Hub-Beta/dashboard/` — 6,500 lines
of Python, the largest single component in the estate.

> **Status.** §1–§6 are normative now and fix the external contract: routes,
> the JSON shapes `dashboard.js` consumes, the persistence model, and the
> alarm semantics. The per-rule detail of §5.3–§5.5 and the Azure query
> texts of §4 are completed at the start of Phase 7, from
> `dashboard/services/alarm{1,2,3}.py` and `dashboard/services/azure_monitor.py`.
> Nothing else in this workspace depends on this crate, so that sequencing
> costs nothing.

---

## §1 Scope and posture

§1.1 The dashboard is **read-mostly**: it observes Azure and renders. The
only state it writes is alarm configuration and alarm state, in Cosmos DB.

§1.2 **It MUST start and serve every page with no Azure configuration at
all**, returning empty state rather than errors. This is load-bearing for
local development and for the test suite, and it is the single most
frequently exercised behaviour in the Python's tests.

§1.3 It renders message content on the messages and trace pages. That
content MUST be redacted per
[`04-observability.md`](04-observability.md) §2 before display, unless the
deployment explicitly disables redaction. The Python does not do this; it is
a deliberate correction, and it is listed here rather than in
[`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python) because
it is a change in what an operator sees and needs the Hub team's agreement.

---

## §2 Routes

### §2.1 Pages

| Path | Content |
| --- | --- |
| `/` | Overview: flow health, queue depths, alarm state |
| `/flows` | Per-flow detail |
| `/exceptions` | Exceptions from Log Analytics |
| `/service-bus` | Queue and dead-letter depths |
| `/messages` | Message search |
| `/trace/{operation_id}` | One distributed trace |
| `/alarms` | Alarm state |
| `/alarms/config` | Alarm configuration |
| `/language` (`POST`) | Set the display language |

### §2.2 JSON API

Consumed by `dashboard.js`. The shapes are a contract with the browser and
MUST NOT change without changing the script.

| Path | Returns |
| --- | --- |
| `/healthz` | Liveness |
| `/api/status` | The overview payload |
| `/api/refresh` | Force a cache refresh, then the overview payload |
| `/api/flows` | Flow definitions and health |
| `/api/container-app-history/{name}` | Replica and restart history |
| `/api/messages` | Message search results |
| `/api/servicebus-metrics` | Queue and dead-letter depths |
| `/api/hl7-throughput` | Messages per interval |
| `/api/alarms/status` | Alarm state |

§2.2.1 `/api/status` is served from a **30-second in-memory cache**. The
cache is shared across requests and refreshed on demand by `/api/refresh`.

§2.2.2 Every API route MUST answer within its timeout even when Azure is
unreachable, returning empty state with a flag the browser can render as
"unavailable" rather than "zero". Zero and unknown are different, and an
operator must be able to tell them apart.

---

## §3 Flow health

§3.1 Flow definitions mirror
[`01-architecture.md`](01-architecture.md) §2.1 — the same flows, the same
queue names, resolved from the same environment variables.

§3.2 A flow's health is computed from its queue depth, its dead-letter
depth, and its containers' state, against the thresholds of
[`02-configuration.md`](02-configuration.md) §9.2:

| Condition | State |
| --- | --- |
| Dead-letter depth ≥ `DLQ_WARNING_THRESHOLD` | Critical |
| Active depth ≥ `QUEUE_CRITICAL_THRESHOLD` | Critical |
| Active depth ≥ `QUEUE_WARNING_THRESHOLD` | Warning |
| A container in the flow is not running | Critical |
| Otherwise | Healthy |

§3.3 A flow whose data could not be retrieved is **Unknown**, not Healthy.

---

## §4 Azure integrations

| Source | Used for | API |
| --- | --- | --- |
| Service Bus management | Queue and dead-letter depths | ARM REST |
| Log Analytics | Exceptions, message counts, traces | Query REST |
| Container Apps | Replica counts, restarts, revisions | ARM REST |
| Cosmos DB | Alarm configuration and state | Cosmos REST |

§4.1 Credentials are a service principal
(`AZURE_TENANT_ID`/`AZURE_CLIENT_ID`/`AZURE_CLIENT_SECRET`) when configured,
otherwise the ambient credential.

§4.2 Every integration MUST degrade to empty rather than raising, per §1.2.

§4.3 The Log Analytics query texts are a contract with the telemetry names
of [`04-observability.md`](04-observability.md) §1 and §3. They are ported
verbatim from `dashboard/services/azure_monitor.py` at the start of Phase 7,
and a change to either side requires a change to both.

---

## §5 Alarms

§5.1 Three rules, `alarm1`, `alarm2` and `alarm3`, sharing a base
(`services/alarm_base.py`). Each has: configuration held in Cosmos,
evaluated state held in Cosmos, and an e-mail escalation.

§5.2 **Out-of-hours guard.** Escalation e-mail is suppressed outside
configured hours (`tests/test_email_ooh_guard.py`). The guard's exact
schedule is part of §5.3's completion.

§5.3–§5.5 *(Phase 7.)* One section per rule, giving its trigger condition,
its evaluation window, its de-duplication and its recovery condition,
derived from `services/alarm{1,2,3}.py`.

§5.6 **Persistence.** Alarm configuration and state live in Cosmos DB,
database `COSMOS_DATABASE`, container `COSMOS_CONTAINER`, partition key
`/pk`.

| `COSMOS_ENDPOINT` | `COSMOS_KEY` | Behaviour |
| --- | --- | --- |
| blank | — | Persistence disabled; reads return empty, writes are no-ops |
| set | set | Key authentication; the database and container are auto-created |
| set | blank | AAD RBAC; the database and container MUST be provisioned externally |

§5.6.1 `COSMOS_DISABLE_SSL_VERIFY` MAY be true **only** for the local
emulator's self-signed certificate. A deployment with it true and a
non-local endpoint is a startup failure.

---

## §6 Internationalisation

§6.1 The Python carries a Babel catalogue (`babel.cfg`) and a `/language`
route.

§6.2 The Rust ships **English only** in its first cut, matching what is
actually deployed. The `/language` route is retained and accepts the
existing values so a bookmarked URL does not 404; it is a no-op until a
catalogue is wired up.

---

## §7 Implementation notes

§7.1 `axum` for routing, `askama` for templates ported from the Jinja
originals, `reqwest` for the Azure REST calls.

§7.2 The DHCW brand applies, as
[`14-buswatch.md`](14-buswatch.md) §7.2.

§7.3 `FLASK_SECRET_KEY` keeps its name and signs the session cookie, so a
deployment's existing secret keeps working.

---

## §8 Tests

§8.1 Every route renders with **no** Azure configuration (§1.2). This is
the first test written and the last one allowed to fail.

§8.2 §3.2's health table, every row, including the Unknown of §3.3.

§8.3 §5.6's three persistence modes, including the disabled mode returning
empty.

§8.4 §5.2's out-of-hours guard, on both sides of the boundary.

§8.5 Each `/api/*` shape asserted against a golden JSON file captured from
the Python, so `dashboard.js` needs no change.

§8.6 §1.3's redaction: a message containing a known marker is rendered and
the marker is absent from the HTML.
