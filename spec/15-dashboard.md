# 15 — NOC dashboard

The operations dashboard: flow health, queue depths, exceptions, message
throughput, and three alarm rules with e-mail escalation. Implemented by
`crates/dashboard`.

Reference implementation: `Integration-Hub-Beta/dashboard/` — 6,500 lines
of Python, the largest single component in the estate.

> **Status.** Complete and normative. §4's query texts and §5.3–§5.5's
> per-rule detail were written at the start of Phase 7 from
> `dashboard/services/alarm{1,2,3}.py` and
> `dashboard/services/azure_monitor.py`, as planned. Two things found while
> reading them are recorded as findings in §9 rather than reproduced
> silently.

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
| `/alarms/pauses` | Alarm pauses: active, scheduled, recently ended (§10.2) |
| `/contacts` | Who to call about a flow (§10.3) |
| `/network-test` | TCP connectivity test (§10.4) |
| `/network-test/config` | Flow source servers (§10.4.5) |
| `/set-language` (`POST`) | Set the display language (§6) |

### §2.2 JSON API

Consumed by `dashboard.js`. The shapes are a contract with the browser and
MUST NOT change without changing the script.

| Path | Returns |
| --- | --- |
| `/healthz` | Liveness |
| `/api/status` | The overview payload |
| `/api/refresh` | Force a cache refresh, then the overview payload |
| `/api/flows` | Flow definitions and health |
| `/api/container-app/{name}/history` | Replica and restart history |
| `/api/messages` | Message search results |
| `/api/servicebus-metrics` | Queue and dead-letter depths |
| `/api/hl7-throughput` | Messages per interval |
| `/api/alarms/status` | Alarm state |
| `/api/alarm-pauses`, `/api/contacts`, `/api/network-test/*` | §10.2–§10.4 |

§2.2.1 `/api/status` is served from a **30-second in-memory cache**. The
cache is shared across requests and refreshed on demand by `/api/refresh`.

§2.2.2 Every API route MUST answer within its timeout even when Azure is
unreachable, returning empty state with a flag the browser can render as
"unavailable" rather than "zero". Zero and unknown are different, and an
operator must be able to tell them apart.

### §2.3 The shapes `dashboard.js` reads

These are a contract with the browser, taken from the scripts rather than
from the Python's handlers: what the page **reads** is what must be
returned, and a field the handler emits that nothing reads is not part of
it.

§2.3.1 `/api/status` — read by `dashboard.js :: refreshData`:

| Key | Read as |
| --- | --- |
| `refreshed_at` | a date the page formats |
| `system_health` | `healthy` / `warning` / `critical` / `unknown` |
| `kpis` | `total_active_messages`, `total_dlq_messages`, `exception_count_1h`, `flows_healthy`, `flows_warning`, `flows_critical` |
| `flows` | each with `id` and `health`; `id` matches a `data-flow-id` attribute |
| `queues` | the queue table |
| `alarm1_summary`, `alarm2_summary`, `alarm3_summary` | one per alarm |
| `retry_delays`, `retry_delay_kpis` | the retry panel |

§2.3.1a **A flow carries a queue *pair*, not a combined total.**
`dashboard.js` computes a card's numbers as

```js
const pre = flow.pre_queue || {};
const post = flow.post_queue || {};
const total = (pre.active || 0) + (post.active || 0) + subActive;
```

so `flows[]` MUST carry `pre_queue`, `post_queue` and `subscriptions`.
Each queue carries `name`, `active`, `dlq`, `exists` and `health`;
`updateQueueNode` returns early on a falsy `name`, so a queue that was not
found is `name: null` rather than absent.

`active` and `dlq` are numbers, never `null` — the script adds them, and a
`null` poisons the sum.

**This was got wrong.** An earlier build emitted a flat `active` and `dlq`
per flow. The `|| {}` fallback meant nothing raised: every flow card
rendered **zero**, on the page an operator reads first, with no error in the
console. A wrong number that looks like a right one. Found by capturing the
Python's own `/api/status` and diffing it, which is what §8.5 is for.

The Rust derives the pair from the queue **names** it already matched
(`spec/01-architecture.md` §2.1.1). The Python instead discovers each
container app's `INGRESS_QUEUE_NAME` and `EGRESS_QUEUE_NAME` through ARM;
§3.1 rejects that discovery deliberately, so the two arrive at the same
shape by different routes.

§2.3.2 `/api/servicebus-metrics` and `/api/hl7-throughput` return **two
named series**, not a flat list:

```json
{"incoming": [{"time": "…", "value": 0}], "outgoing": [...], "timespan": "24h"}
```

`sb-chart.js` and `hl7-throughput-chart.js` both read `data.incoming` and
`data.outgoing` and take `time` and `value` off each point. A `points` array
renders as an empty chart with no error in the console.

§2.3.3 Both take `hours`, and the series are **zero-filled** across the
window so a gap draws as zero rather than as a straight line between two
distant points.

§2.3.4 `/api/container-app/{name}/history` — note the shape. It is not
`/api/container-app-history/{name}`; `app-history.js` fetches
`'/api/container-app/' + encodeURIComponent(name) + '/history?hours=' + hours`.

---

§2.2.3 A KPI total that spans a queue whose depth could not be read MUST be
**absent**, not a partial sum, and the payload MUST carry how many queues
were unreadable. A number is the first thing an operator looks at, and a
partial sum presented as a total is a wrong number that looks like a right
one. See §9.8.

---

## §3 Flow health

§3.1 Flow definitions mirror
[`01-architecture.md`](01-architecture.md) §2.1 — the same flows, the same
**`WORKFLOW_ID`s**, the same queue names, resolved from the same environment
variables.

§3.1.1 The `WORKFLOW_ID` is the identity. Every trace, metric and alarm
query joins on it ([`04-observability.md`](04-observability.md) §1), so a
flow the dashboard names differently is a flow whose alarms match nothing —
and match nothing *silently*, reporting `unknown` rather than failing. All
eight flows of §2.1 are present, including `lims-soapserver`, and the
outbound flow is `mpi-to-topic`. See §9.5 and §9.6 for what the Python does
instead.

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
of [`04-observability.md`](04-observability.md) §1 and §3. A change to
either side requires a change to both.

§4.3.1 The queries, from `dashboard/services/azure_monitor.py` and
`services/alarm{1,2,3}.py`. `{...}` marks an interpolated value; every one
is either a number or a string put through §4.4's sanitiser.

| Purpose | Table | Query |
| --- | --- | --- |
| Exceptions (§2) | `AppExceptions` | `where TimeGenerated > ago({hours}h)` → `project timestamp=TimeGenerated, type=ExceptionType, outerMessage=coalesce(OuterMessage, Message), severityLevel=SeverityLevel, appName=AppRoleName, operation_Id=OperationId` → `order by timestamp desc` → `take 200` |
| Messages today | `AppTraces` | `where TimeGenerated > startofday(now())` → `where Message == "Integration Hub Event"` → `project timestamp=TimeGenerated, name=Message, customDimensions=Properties, appName=AppRoleName` → `order by timestamp desc` → `take 500` |
| Throughput series | `AppMetrics` | `where TimeGenerated > ago({hours}h)` → `where Name in ("messages_received", "messages_sent")` → `summarize Value = sum(Sum) by bin(TimeGenerated, {bin}), Name` → `order by TimeGenerated asc` |
| Throughput filter options | `AppMetrics` | `where TimeGenerated > ago(30d)` → `where Name in ("messages_received", "messages_sent")` → `extend health_board = tostring(Properties["health_board"])` → `extend workflow_id = tostring(Properties["workflow_id"])` → `summarize by health_board, workflow_id` |
| Retry delays | `AppMetrics` | `where TimeGenerated > ago({hours}h)` → `where Name == "retry_delay_seconds"` → extend `workflow_id`/`microservice_id`/`queue`/`attempt` from `Properties` → `where isnotempty(workflow_id)` → `summarize arg_max(TimeGenerated, Sum, microservice_id, queue, attempt) by workflow_id` → `where delay_seconds > {min}` |
| Alarm 1 — last received | `AppTraces` | `where TimeGenerated > ago(30d)` → `where Message == "Integration Hub Event"` → `where Properties contains "MESSAGE_RECEIVED"` → `extend workflow_id = tostring(parse_json(Properties)["workflow_id"])` → `where workflow_id in ({ids})` → `summarize last_message = max(TimeGenerated) by workflow_id` |
| Alarm 2 — last sent | `AppMetrics` | `where TimeGenerated > ago({days}d)` → `where Name == "messages_sent"` → `extend workflow_id = tostring(Properties["workflow_id"])` → `where workflow_id in ({ids})` → `summarize last_message = max(TimeGenerated) by workflow_id` |
| Alarm 3 — failures | `AppTraces` | `where TimeGenerated > ago({window}m)` → `where Message has "Integration Hub Event"` → extend `workflow_id` and `event_type` from `parse_json(Properties)` → `where workflow_id in ({ids})` → `where event_type == "MESSAGE_FAILED"` → `summarize Total = count() by workflow_id` |

§4.3.2 Note the asymmetry, which is not a mistake to tidy: Alarm 1 reads
**traces** for `MESSAGE_RECEIVED` while Alarm 2 reads **metrics** for
`messages_sent`. The two sides of a flow are therefore measured from
different telemetry, and a gap in one does not imply a gap in the other.

§4.3.3 Every query MAY carry
`| where _ResourceId =~ '{AZURE_APP_INSIGHTS_RESOURCE_ID}'` immediately
after its time filter, when that variable is set. Two of them — the
throughput series and its filter options — do not, so a workspace holding
more than one Application Insights resource mixes them. Recorded as a defect
rather than reproduced: see §9.2.

§4.4 **Interpolation.** These queries are assembled by string formatting, so
every interpolated identifier MUST be sanitised first. The Python's rule is
`re.sub(r"[^a-zA-Z0-9\-_]", "", value)` — strip anything that is not
alphanumeric, hyphen or underscore — and it is what stops a workflow id
reaching KQL as syntax. The Rust MUST apply the same rule and MUST NOT
widen it.

§4.5 A query that returns a partial or failed status is treated as **no
data**, not as an error: §4.2 and §1.2. Alarm rules then report `unknown`
rather than `healthy`, so a broken query never reads as "everything is
fine".

### §4.6 Endpoints and API versions

The Python reaches Azure through the Azure SDK, so the URLs and API
versions live inside that SDK rather than in its source. This
implementation calls the REST APIs directly, which means **this workspace
now owns those versions** — and a wrong one is a `400` at runtime, in a
panel that §4.2 turns into an empty result. So they are pinned here, as
normative values with a test, rather than left as literals in a function.

| Purpose | Host | Path | Version |
| --- | --- | --- | --- |
| AAD token, service principal | `login.microsoftonline.com` | `/{tenant}/oauth2/v2.0/token` | — |
| AAD token, managed identity | `$IDENTITY_ENDPOINT` | *(as given)* | `2019-08-01` |
| Service Bus queue runtime | `management.azure.com` | `…/namespaces/{ns}/queues` | `2021-11-01` |
| Container Apps | `management.azure.com` | `…/providers/Microsoft.App/containerApps` | `2024-03-01` |
| Log Analytics query | `api.loganalytics.io` | `/v1/workspaces/{id}/query` | — |

§4.6.1 **Scopes.** The ARM calls take `https://management.azure.com/.default`;
the Log Analytics call takes `https://api.loganalytics.io/.default`. They
are different tokens and MUST NOT be shared — a dashboard that sent the ARM
token to Log Analytics would get a `401` that looks exactly like a missing
role assignment.

§4.6.2 **Managed identity.** Inside Container Apps the token comes from
`IDENTITY_ENDPOINT` with the `X-IDENTITY-HEADER` header, not from IMDS at
`169.254.169.254`. Both are "managed identity"; only the first works there.
When `IDENTITY_ENDPOINT` is absent the dashboard has no ambient credential
and every integration reports unavailable, per §4.2 — it does not fall back
to IMDS, because a request to a link-local address from a machine that is
not in Azure hangs until it times out, once per panel per refresh.

§4.6.3 **Tokens are cached until shortly before they expire.** An AAD token
lasts about an hour and every panel needs one; fetching per request would
turn one page load into a dozen token requests and hit AAD's own throttle.
The margin is subtracted from the stated expiry so a token is never used in
the last moments of its life.

§4.6.4 **A token failure is an empty result, not an error.** §4.2 admits no
exception for the credential: a dashboard that returned `500` because AAD
was briefly unavailable would be down at the moment somebody needed to look
at why something else was down.

§4.7 **TLS is required and verified.** Every host above is reached over
HTTPS with the Mozilla root store, and certificate verification is not
configurable. `COSMOS_DISABLE_SSL_VERIFY` (§5.6.1) is the single exception
in this specification and it applies only to the local emulator.

---

## §5 Alarms

§5.1 Three rules, `alarm1`, `alarm2` and `alarm3`, sharing a base
(`services/alarm_base.py`). Each has: configuration held in Cosmos,
evaluated state held in Cosmos, and an e-mail escalation.

§5.1.1 **Time periods.** Alarms 1 and 2 vary their thresholds by period.
The classification is done in **Europe/London**, so BST and GMT are handled
by the zone rather than by an offset:

| Period | Window (UK local) |
| --- | --- |
| Weekend | Friday 17:00 → Monday 09:00 |
| Day | Monday–Friday 09:00–17:00 |
| Evening | everything else — Monday–Friday outside the day window |

§5.1.2 **Rule lifecycle, shared by all three.** Each rule is evaluated
against its own state document, and the sequence is normative because the
order decides what an operator sees:

1. A rule with `deleted` set, or without `alarm_enabled`, is not evaluated
   at all.
2. A rule covered by an active pause record (§10.2), or whose `paused_until`
   is in the future, reports `paused` and is not evaluated further. A `paused_until` in the past is **cleared** and
   evaluation continues, so a lapsed pause does not linger.
3. No data at all reports `unknown`. Not `healthy`: a silent query and a
   silent flow must not look the same (§4.5).
4. Below the trip condition reports `healthy`, and **clears** the rule's
   state entry — so the next trip fires immediately rather than being
   swallowed by a stale cooldown.
5. At or above the trip condition with no `last_alarm_at` reports
   `critical`, stamps `last_alarm_at`, and escalates.
6. At or above it within `alerting_gap_minutes` of `last_alarm_at` reports
   `suppressed` and does not escalate. Past that gap it reports `critical`
   again, re-stamps, and escalates again.

§5.1.3 State is persisted so a cooldown survives a restart. Without that, a
dashboard restart would re-escalate every firing alarm.

§5.1.4 Rows are ordered `paused`, `critical`, `suppressed`, `unknown`,
`healthy`. An unrecognised status sorts last.

---

§5.2 **Out-of-hours guard.** Each rule carries `email_alerts_enabled` and
`email_ooh_enabled`. `email_ooh_enabled` is forced to false whenever
`email_alerts_enabled` is false, on write and on read, so a
checked-but-hidden checkbox cannot leave a stale value behind
(`tests/test_email_ooh_guard.py`).

§5.2.1 A rule with `email_alerts_enabled` and **without**
`email_ooh_enabled` MUST NOT escalate by e-mail during the Evening or
Weekend periods of §5.1.1. It still reports `critical` on the page — the
guard suppresses the **e-mail**, not the alarm.

§5.2.2 **This is a divergence, and it is deliberate.** The Python stores
`email_ooh_enabled`, validates it in all three config routes and displays
it on the page, but **no send path ever reads it**: `_send_alarm_email` in
each of `alarm1.py`, `alarm2.py` and `alarm3.py` gates only on
`email_alerts_enabled`. The out-of-hours guard is configured and never
enforced, so a rule set to stay quiet overnight e-mails overnight anyway.
Implementing it is the intent the toggle already advertises to whoever
ticked it. Raised with the Hub team; see §9.1.

---

§5.3 **Alarm 1 — incoming inactivity.** Partition `alarm1`.

§5.3.1 Per rule: `workflow_id`, `day_threshold_minutes` (default 60),
`evening_threshold_minutes` (120), `weekend_threshold_minutes` (240),
`alerting_gap_minutes` (60), `display_name`, and the two e-mail toggles.

§5.3.2 **Trip condition.** Minutes since the most recent `MESSAGE_RECEIVED`
trace for the rule's `workflow_id` **exceeds** the threshold for the current
period. Strictly greater than, not at.

§5.3.3 **Window.** A 30-day lookback. No trace inside it is `unknown`, not
`critical`: a workflow nobody has ever sent to is not a workflow that has
failed.

§5.3.4 Query: §4.3.1's "Alarm 1 — last received". Note it reads `AppTraces`
and matches `Properties contains "MESSAGE_RECEIVED"` before extracting the
workflow id, so a message whose *payload* happened to contain that string
would match. Reproduced as-is; the alternative changes which rows count.

---

§5.4 **Alarm 2 — outgoing inactivity.** Partition `alarm2`.

§5.4.1 Configuration is Alarm 1's, with the same defaults. The difference is
entirely in what is measured.

§5.4.2 **Trip condition.** Minutes since the most recent `messages_sent`
**metric** for the rule's `workflow_id` exceeds the current period's
threshold.

§5.4.3 Query: §4.3.1's "Alarm 2 — last sent". Metrics, not traces — see
§4.3.2.

§5.4.4 A seed rule `phw-to-mpi-outgoing` (`phw-to-mpi`, "PHW → MPI
Outgoing") is always listed, enabled or not, so the page is never empty
before anything has been configured.

---

§5.5 **Alarm 3 — processing failures.** Partition `alarm3`.

§5.5.1 Per rule: `workflow_id`, `window_duration_minutes` (default 15),
`threshold` (default 1), `alerting_gap_minutes` (60), `display_name`, and
the two e-mail toggles.

§5.5.2 **Trip condition.** The count of `MESSAGE_FAILED` events for the
rule's `workflow_id` within `window_duration_minutes` is **at or above**
`threshold`. At or above, unlike §5.3.2's strictly-greater — the difference
is in the Python and it matters at the default of 1, where one failure
fires.

§5.5.3 **Window.** The rule's own `window_duration_minutes`, so this alarm
is a rate rather than an age. Rules sharing a window are queried together,
one query per distinct window.

§5.5.4 Query: §4.3.1's "Alarm 3 — failures". It matches `Message has
"Integration Hub Event"`, where the other two use `==`.

§5.5.5 A seed rule `phw-to-mpi-failures` is always listed, as §5.4.4.

§5.6 **Persistence.** Alarm configuration and state live in Cosmos DB,
database `COSMOS_DATABASE`, container `COSMOS_CONTAINER`, partition key
`/pk`.

| `COSMOS_ENDPOINT` | `COSMOS_KEY` | Behaviour |
| --- | --- | --- |
| blank | — | Persistence disabled; reads return empty, writes are no-ops |
| set | set | Key authentication; the database and container are auto-created |
| set | blank | AAD RBAC; the database and container MUST be provisioned externally |

§5.6.0 The container defaults to `dashboard` and holds more than alarms:
see §10.1.

§5.6.1 `COSMOS_DISABLE_SSL_VERIFY` MAY be true **only** for the local
emulator's self-signed certificate. A deployment with it true and a
non-local endpoint is a startup failure.

§5.6.2 **A failed state write MUST still suppress the next escalation.**
The cooldown of §5.1.2 step 6 is what stops a firing alarm e-mailing on
every evaluation cycle. If a write fails and the process keeps no record,
the next cycle reads the old state, finds no `last_alarm_at`, and escalates
again — every thirty seconds, for as long as the write keeps failing.

So the implementation holds the state it wrote **in memory** as well, and
uses that when Cosmos cannot be read or written. The cost is that a restart
during an outage loses the cooldowns, which is the durability §5.1.3 asks
for and is a smaller harm than the alternative. Raised as §9.9.

§5.6.3 A store that is **unreachable** and a store with **no rules
configured** MUST NOT produce the same result. The first is an outage
somebody has to fix; the second is an ordinary Tuesday. Returning empty for
both silently disables every alarm in the estate while the page reports
nothing wrong.

### §5.8 E-mail escalation

Azure Communication Services, per `services/email_service.py`.

§5.8.1 The endpoint is `POST {acs-endpoint}/emails:send?api-version=2023-03-31`,
signed with the same HMAC scheme Cosmos uses (§5.7) over the ACS connection
string's `accesskey`, with the addition of an `x-ms-content-sha256` header
holding the base64 SHA-256 of the body.

§5.8.2 The message is
`{"senderAddress", "recipients": {"to": [{"address"}]}, "content": {"subject", "html"}}`
— the Python's shape, so a working sender configuration keeps working.

§5.8.3 **Sending MUST NOT be able to fail an evaluation.** A failure is
logged and the cycle continues. An alarm that could not be e-mailed is
still an alarm on the page, and an ACS outage must not stop the dashboard
evaluating the other rules.

§5.8.4 **A `429` is retried with backoff**, honouring `Retry-After` when the
response carries one and doubling `ALERT_EMAIL_RETRY_BACKOFF_SECONDS`
otherwise, up to `ALERT_EMAIL_MAX_RETRIES`. The ACS sandbox domain has a
low fixed quota and several alarms firing in one cycle reach it easily.

§5.8.5 E-mail is sent only when every part is configured:
`ALERT_EMAIL_ENABLED`, a connection string, `ALERT_EMAIL_TO` and
`ALERT_EMAIL_FROM`. A partially configured deployment MUST NOT enable the
controls that offer it, or an operator ticks a box that silently does
nothing.

§5.8.6 The connection string is a secret and MUST NOT reach a log line, an
error or a `Debug` rendering — it carries the account access key.

---

### §5.7 Cosmos authentication

§5.7.1 **Key authentication** signs every request. The `Authorization`
header is

```
type=master&ver=1.0&sig={base64(HMAC-SHA256(key, string_to_sign))}
```

percent-encoded, where `string_to_sign` is five lines, each terminated by
`\n`:

```
{verb lowercase}\n{resource type lowercase}\n{resource link}\n{date lowercase}\n\n
```

§5.7.2 Three details in that string are easy to get wrong and each produces
an indistinguishable `401`:

- The **verb** and **resource type** are lowercased; the **resource link**
  is not — `dbs/x/colls/Y` keeps `Y`'s case.
- The **date** is lowercased RFC 1123 in GMT, and it is the same value sent
  in the `x-ms-date` header. A mismatch between the two is a `401`, not a
  clock error.
- The string ends with an **empty fifth line**, so it finishes with two
  newlines.

§5.7.3 The signing key is base64; it is decoded before use. Signing with the
base64 *text* produces a valid-looking header that is always rejected.

§5.7.4 **AAD RBAC** sends `Authorization: Bearer {token}` for the
`https://{account}.documents.azure.com/.default` scope instead, and the
database and container MUST already exist (§5.6) — the dashboard's identity
is not expected to hold control-plane rights.

§5.7.5 A key MUST NOT reach a log line, an error message or a `Debug`
rendering. It is a full-access credential for the account.

---

## §6 Internationalisation

§6.1 The Python carries a Babel catalogue (`babel.cfg`) and a `/language`
route.

§6.2 The Rust ships **English only** in its first cut. The `/set-language`
route is retained and accepts the existing values (`en`, `cy`) so an
existing form does not 404; the preference is stored and honoured as soon
as a catalogue exists.

§6.3 This matches what the Python actually *delivers*, which is not what it
appears to offer: `translations/cy/LC_MESSAGES/messages.po` holds 352
message ids and 352 **empty** translations, so selecting Welsh renders
English. See §9.12 — this is a finding for the Hub team rather than a
detail of the port.

§6.4 **The redirect after setting a language MUST NOT leave the site.** The
`Referer` header is client-supplied. The Python rebuilds the target from
its path and query only, discarding scheme and host, and normalises `\` to
`/` first because some browsers resolve `/\evil.example` as a
network-path reference. That behaviour is preserved, and it is specified
here rather than left as a comment in one implementation.

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
the Python, so `dashboard.js` needs no change. The goldens live in
`crates/dashboard/tests/golden/`, captured from the Python dashboard run
locally with **no** Azure configuration — which §1.2 makes a valid way to
run it.

§8.5.1 §8.5 and §2.3 pull in opposite directions, and §2.3 wins. §2.3 takes
the contract from the **scripts**, on the grounds that a field the handler
emits and nothing reads is not part of it; §8.5 says to assert against the
Python's **output**. Read literally, §8.5 would require the Rust to emit
`colour`, `icon` and `topic`, which no script reads.

The goldens are therefore used as a **source of candidate keys, not as an
equality assertion**: every key in a golden is checked against the scripts,
and the ones a script reads become requirements. That is how §2.3.1a was
found — the diff surfaced `pre_queue`, and `dashboard.js` proved it
mattered.

§8.6 §1.3's redaction: a message containing a known marker is rendered and
the marker is absent from the HTML.

§8.7 §5.1.1's period boundaries, on **both** sides of each: Friday 16:59 is
Day and 17:00 is Weekend; Monday 08:59 is Weekend and 09:00 is Day. Also a
date inside British Summer Time and one outside it, since the classification
is a wall-clock rule in Europe/London and an offset-based implementation
would be an hour wrong for half the year.

§8.8 §5.1.2's lifecycle, as a table: every step, in order, including the
lapsed pause being cleared and the return to `healthy` clearing the state so
the next trip fires immediately.

§8.9 §5.3.2's strictly-greater and §5.5.2's at-or-above, each at its exact
boundary. They differ, they came from the Python, and a test that only
exercised a value well past the threshold would not have noticed.

§8.10 §4.4's sanitiser: a workflow id carrying quotes, a pipe or a KQL
comment comes out as alphanumerics, hyphens and underscores.

---

## §9 Findings raised with the Hub team

Behaviour in the Python that this specification does not reproduce as-is.
Each is a decision that needs confirming before the affected container is
cut over.

§9.1 **The out-of-hours guard is configured but never enforced.**
`email_ooh_enabled` is written by all three config routes, forced consistent
with `email_alerts_enabled`, persisted to Cosmos and shown on the page — and
read by no send path. `_send_alarm_email` in `alarm1.py`, `alarm2.py` and
`alarm3.py` gates on `email_alerts_enabled` alone. A rule configured to stay
quiet overnight e-mails overnight. §5.2.1 implements the guard; if the Hub
team wants the current behaviour, that clause comes out.

§9.2 **Two queries ignore `AZURE_APP_INSIGHTS_RESOURCE_ID`.** The throughput
series and its filter options carry no `_ResourceId` filter, while every
other query does (§4.3.3). In a workspace holding more than one Application
Insights resource, the throughput chart mixes them and no other panel does.
Reproduced as-is so the numbers do not silently change; flagged because the
inconsistency looks like an omission rather than a decision.

§9.3 **`FLASK_SECRET_KEY` has a hardcoded default.**
`dashboard/config.py` falls back to the literal
`dev-secret-key-change-in-production`, which is in a public repository. Any
deployment that has not set the variable is signing its session cookies with
a key an attacker already has. This specification makes an absent key mean
"generate one at startup" instead
([`02-configuration.md`](02-configuration.md) §9.2.3) — that costs a logout
on restart and closes the hole. A deployment wanting sessions to survive a
restart sets the variable, which is what it should have been doing.

§9.4 **`COSMOS_DATABASE` defaults to `integration-hub`.** Recorded here
because an earlier draft of
[`02-configuration.md`](02-configuration.md) §9.2 said
`integration-hub-dashboard`, and that mistake would have been invisible: the
dashboard would have connected successfully to a database that did not
exist, found no alarm rules, and shown an empty configuration page. Nothing
would have looked broken.

§9.5 **The outbound flow is named `mpi-outbound` on the dashboard and
`mpi-to-topic` everywhere else.** `local/mpi-hl7-server.env` and
`local/mpi-hl7-chemo-sender.env` both set `WORKFLOW_ID="mpi-to-topic"`, and
that is the value every trace, metric and alarm query joins on
([`04-observability.md`](04-observability.md) §1). `flows.py`'s
`_FLOW_DEFS` calls the same flow `mpi-outbound`.

The consequence is quiet, which is what makes it worth raising: an alarm
rule created against `mpi-outbound` matches no telemetry, so §5.1.2 step 3
reports it `unknown` — indefinitely, and indistinguishably from a workspace
that is simply not configured. Nobody is told the name is wrong. This
specification uses `mpi-to-topic` (§3.1).

§9.6 **The LIMS flow is missing from the dashboard.** `_FLOW_DEFS` has no
entry for `lims-soapserver`, so the SOAP ingress of
[`01-architecture.md`](01-architecture.md) §2.1 does not appear on the flow
page and cannot be alarmed on. Included here; if it was left out
deliberately, that decision needs recording rather than inferring.

§9.7 **The static flow fallback builds queue names from parts.**
`flows.py` resolves queues by matching a suffix such as
`sbq-phw-hl7-transformer` against the namespace, using the Terraform naming
convention, whenever Container Apps discovery is unavailable — which is
every local run. [`01-architecture.md`](01-architecture.md) §2.1.1 says the
opposite: "The full name always comes from an environment variable; nothing
in this workspace constructs a queue name from parts." A renamed queue would
silently stop matching and the flow would read as unknown. §3.1 keeps the
environment-variable rule.

§9.8 **KPI totals count an unreadable queue as zero.**
`status_builder.py :: build_status` computes
`sum(q.get("active_message_count", 0) for q in queues)`. A queue the
management API could not answer for contributes `0`, so during an outage an
operations centre reads "0 messages waiting" and cannot tell that from a
quiet estate — in the largest number on the page. §2.2.3 makes the total
absent instead, with a `queues_unknown` count beside it. This is the same
correction as §3.3, applied to the KPI band rather than the flow table.

§9.9 **A Cosmos write failure e-mails on every cycle.**
`cosmos_store.upsert_document` catches `CosmosHttpResponseError` and logs
it — "Errors are logged and swallowed so a persistence outage never breaks
a request". The alarm evaluator treats the write as done and carries on.

The consequence is not the outage case, which is quiet: if the **config**
read also fails, `load_config` returns `{"rules": {}}`, no rule is enabled,
and nothing is evaluated at all (that is §9.10). It is the case where reads
succeed and writes do not — which is precisely what Cosmos RU exhaustion
looks like, because a write costs more RUs than a read and throttles first.

Then, per cycle: `load_state` returns no `last_alarm_at`, the rule reads as
`critical` rather than `suppressed`, `_send_alarm_email` fires, the write to
record the cooldown fails silently, and the next cycle does the same. With
`API_CACHE_TTL` at 30 seconds that is up to two e-mails a minute per firing
rule — against an ACS sandbox whose quota the Python's own
`email_service.send_alert_email` carries retry-on-429 code for.

§5.6.2 keeps the written state in memory so the cooldown still applies when
the write does not land.

§9.10 **A Cosmos outage silently disables every alarm.**
The same swallow on the read side: `load_config` turns an unreachable store
into `{"rules": {}}`, so `get_alarm_status` finds no enabled rules and
returns `[]`. The alarm page then shows nothing — indistinguishable from a
deployment where nobody has configured any rules. Every alarm in the estate
is off and the dashboard reports no problem. §5.6.3 makes the two
distinguishable, and the page says which it is.

§9.11 **This specification had the container-app history route wrong.**
§2.2 listed `/api/container-app-history/{name}`; the Python registers
`/api/container-app/<name>/history` and `app-history.js` fetches that.
Implementing the specification's spelling would have produced a route
nothing calls and a chart that never loads, with no error anywhere —
`app-history.js` logs a fetch failure to the console and draws an empty
state. Corrected against the source and the script. Recorded because it is
the same class of mistake as §9.4: a wrong name that fails silently.

§9.12 **The Welsh language option renders English.**
`translations/cy/LC_MESSAGES/messages.po` contains 352 `msgid` entries and
352 empty `msgstr` entries. `gettext` falls back to the message id when a
translation is empty, so every string comes out in English. The language
toggle is present, selectable, and has no effect.

This is raised as a service finding rather than a porting detail. NHS Wales
operates under the Welsh Language Standards; a control that offers Welsh and
silently delivers English is worse than one that offers nothing, because a
user who selects it will reasonably conclude the service is broken or is
ignoring them. Either the catalogue is completed or the control is removed
until it is — and that is the Hub team's decision, not this port's.

§6.2 keeps the route and stores the preference so that completing the
catalogue is the only remaining work.

---

## §10 Upstream sync, October 2026

Twenty commits landed on the Python dashboard after this specification was
written (`docs/upstream-sync-2026-10.md`). Each subsection below records one
of them: what it changed, and where this port deliberately differs. The
clauses are normative; the upstream commit is cited so a future reader can
read the Python.

### §10.1 One container, many kinds of document (upstream `78f2d23`)

§10.1.1 **The default `COSMOS_CONTAINER` is `dashboard`**, not `alarms`. The
container now holds everything the dashboard owns — alarm configuration and
state, alarm pauses (§10.2), contacts (§10.3), flow source servers and
network test history (§10.4) — so it is named for the owner rather than for
one kind of data. A deployment that sets `COSMOS_CONTAINER` explicitly is
unaffected. One that relied on the default MUST have a container called
`dashboard` provisioned (§5.6's AAD row: the dashboard cannot create it).

§10.1.2 **A storage-managed `type` discriminator.** Every document written
carries `type`, so the mixed container can be browsed and queried by kind:

| `type` | Partition (`pk`) | Written by |
| --- | --- | --- |
| `alarm_config` | `alarm1` / `alarm2` / `alarm3` | the Python's config pages |
| `alarm_state` | `alarm1` / `alarm2` / `alarm3` | §5.1.3 |
| `alarm_pause` | `alarm-pause` | §10.2 |
| `contact` | `contacts` | §10.3 |
| `flow_source_server` | `flow-source-server` | §10.4 |
| `network_test_history` | `network-test` | §10.4 |

§10.1.3 `id`, `pk` and `type` are **routing metadata**. They are stamped on
write — overriding any value of the same name in the caller's payload — and
stripped on read, together with Cosmos' own `_`-prefixed properties, so a
caller gets back exactly the payload it stored. The consequence, which upstream
hit as a bug (`326b3ec`): a record's own identifier MUST be stored under a
field of its own (`pause_id`, `source_id`, `contact_id`), because `id` does
not survive the round trip.

§10.1.4 **A read that fails is an error, not an empty list** (§5.6.3 applies
to every kind of document). **A write of a record somebody typed in is
refused when persistence is disabled**, rather than accepted and dropped: a
pause that silently was not stored pages on-call. Alarm *state* keeps §5.6's
no-op, because it is a cooldown and not a record.

§10.1.5 *Limit.* A partition is read in one request
(`x-ms-max-item-count: -1`, server-sized, up to 4 MB). The HTTP client
returns only the body, so a continuation token is not followed. Every
partition here is small — tens to low thousands of documents. A partition
that outgrew one page would be truncated without any signal, so the limit is
written down here to be a known limit rather than a surprise; following
`x-ms-continuation` needs the client to return headers and is the fix.

### §10.2 Scheduled alarm pauses (upstream `5eae123`)

An alarm can be paused ahead of planned maintenance: for one rule, for every
alarm of one or more flows, or for everything. A pause starts now or at a
future time, and ends after a duration, at a set time, or when cancelled.

§10.2.1 **A pause is a document of its own**, one per pause, in partition
`alarm-pause` with `type` `alarm_pause` (§10.1.2). It is *not* a field of the
alarm's state document (§5.1.3): the evaluator rewrites the whole state
document on every change, so a pause written into it would race with the
evaluation cycle and lose. `RuleState.paused_until` remains and is still
honoured (§5.1.2 step 2) so a pause made before the upgrade runs out
normally; nothing new writes it.

§10.2.2 Fields: `pause_id` (§10.1.3), `scope_type` (`rule` / `flows` /
`all`), `targets`, `start_at`, `end_at` (null = until cancelled), `reason`,
`requested_by`, `created_at`, `cancelled_at`. `targets` is `[]` for `all`; a
list of workflow ids for `flows`; and for `rule` a list of
`{alarm_type, rule_id, workflow_id}` — the workflow id is stored so a flow can
be badged with the pauses touching it. Times are stored as RFC 3339 in UTC.

§10.2.3 **Status** is derived from the clock, never stored:

| Status | When |
| --- | --- |
| `cancelled` | `cancelled_at` is set and not after `start_at` |
| `scheduled` | `start_at` is in the future |
| `active` | started, and `end_at` is null or in the future |
| `ended` | otherwise (including a pause cancelled after it began) |

A pause with no readable `start_at` is `ended`. Cancelling an active pause
sets `cancelled_at` **and** `end_at` to now, so it reads `ended` with a
`cancelled_early` mark.

§10.2.4 **A flow-scoped pause covers rules added later.** `flows` and `all`
are matched against the rule at evaluation time rather than expanded to rule
ids when created, so a rule added to a paused flow during the maintenance
window is covered too. `rule` matches `alarm_type` and `rule_id`.

§10.2.5 **Evaluation.** Before §5.1.2 step 3, a rule covered by an active
pause reports `paused`, is not evaluated further and does not escalate. Where
several active pauses cover a rule the one that ends **last** is shown, an
indefinite pause counting as latest. A rule with a *scheduled* pause carries
the soonest start so the page can say so.

§10.2.6 **Reads fail open for evaluation, closed for writes.** If the pauses
cannot be read while evaluating, no pause applies and alarms keep firing: a
Cosmos outage must not silence the estate, and the failure is logged. But a
write path (create, cancel) that cannot read the existing pauses MUST fail
(HTTP 503) rather than act on a guess. A create against a store with
persistence disabled is refused (503, "Pause persistence is not configured.")
because a pause that was accepted and not kept would page on-call at 02:00
(§10.1.4).

§10.2.7 **Validation**, each failure a `400` carrying a message safe to show:

- `scope_type` is one of the three; `rule` and `flows` need at least one
  target and at most 500; a flow id matches
  `^[A-Za-z0-9][A-Za-z0-9_-]{0,119}$`; a rule target must name an existing,
  non-deleted rule of that alarm type.
- `reason` (at most 200 characters) and `requested_by` (at most 100) are
  required and trimmed.
- `start` is empty / `now` or a UK-time `YYYY-MM-DDTHH:MM`. A start up to two
  minutes in the past is taken as now (a form left open, clock skew); earlier
  is refused; later than 365 days ahead is refused.
- `end_mode` is `duration` (1 minute to 365 days), `until` (UK time, strictly
  after the start) or `indefinite`.
- **UK time.** Wall-clock times are `Europe/London`. A time that does not
  exist (the spring-forward gap) is refused; an ambiguous autumn time takes
  the first occurrence (BST).

§10.2.8 **Routes.** `GET /alarms/pauses` (page: active, scheduled, recently
ended — the last seven days — and a form), `GET /api/alarm-pauses` (the same
as `{groups, summary}`), `GET /api/alarm-pauses/options` (flow and rule
pickers), `POST /api/alarm-pauses` (create, `201 {ok, pause}`),
`POST /api/alarm-pauses/{pause_id}/cancel` (`200 {ok, pause}`; `404` unknown;
`400` already ended). Ended or cancelled pauses older than 30 days are purged
on the next create or cancel.

§10.2.9 The alarm rows of `/api/alarms/status` and the alarm page carry
`pause_id`, `pause_scope`, `paused_by`, `pause_reason`, `pause_remaining`
(minutes; null when indefinite), `pause_indefinite` and `scheduled_pause`
(`{pause_id, start}` or null). `/api/status` and `/api/alarms/status` carry
`pause_summary` `{active, scheduled}`.

§10.2.10 *Not ported.* Upstream's `POST /alarmN/pause|unpause/<rule>` shims
and the per-flow alarm configuration screens that arrived in the same commit
(`/alarms/by-flow`, `/alarms/config?flow=`) have no counterpart here: this
port has no alarm configuration UI at all (§2.1 lists `/alarms/config`, which
is not yet built), and a single-rule pause is `POST /api/alarm-pauses` with
scope `rule`. Upstream's *conflict* rule — a single-rule resume refused while
a wider pause covers the rule — is therefore moot; the cancel route always
cancels exactly the pause named.

### §10.3 Contacts (upstream `e0c9d88`)

A page of the people to call about a flow: staff in a team, a health board or
a supplier, each linked to the flows they can speak to.

§10.3.1 **A contact is a document**, partition `contacts`, `type` `contact`
(§10.1.2), identified by `contact_id` (§10.1.3). The API presents that as
`id`. Fields: `first_name`, `last_name`, `email`, `phone_number`,
`organisation_type`, `organisation_name`, `notes`, `flow_ids`, `deleted`,
`deleted_at`, `created_at`, `updated_at`.

§10.3.2 **Validation**, each failure a `400` with a message safe to show:

- `first_name` and `last_name`: strings, trimmed, non-empty, at most 100.
- `email`: optional, trimmed and lowercased, matching
  `^[^@\s.]+(\.[^@\s.]+)*@[^@\s.]+(\.[^@\s.]+)+$`.
- `phone_number`: optional, matching `^[0-9+()\- ]{5,30}$` (whitespace
  inside is allowed; the value is trimmed).
- **At least one** of `email` and `phone_number`: a contact nobody can reach
  is not a contact.
- `organisation_type`: exactly `Team`, `Health Board` or `Supplier`.
- `organisation_name`: non-empty, at most 200. `notes`: optional, at most
  2,000.
- `flow_ids`: a list of strings, blanks dropped, duplicates collapsed, each
  one a flow of §3.1. *Deviation:* upstream checks against live discovery and
  skips the check when discovery is down; §3.1 rejects discovery and the set
  of flows is closed, so the check here is unconditional.

§10.3.3 **Deletion is soft.** `DELETE` sets `deleted` and `deleted_at`;
`POST …/restore` clears both. A deleted contact stays visible, badged, to
everyone, and is excluded from the per-flow contact lists on the Flows page.
Deleting a contact that does not exist is a no-op.

§10.3.4 **Retention.** A contact soft-deleted for more than
`CONTACT_RETENTION_DAYS` (default 90) is permanently deleted. The age is
`deleted_at`, else `updated_at`; one with neither readable is left alone,
because purging on a guess is the one irreversible mistake here. The purge
runs at most once a day, when the Contacts page is viewed, and a failure never
breaks the page.

§10.3.5 **List.** `GET /api/contacts?q=&flow=&page=&per_page=&include_deleted=`
returns `{contacts, total, page, per_page}`. `q` is a case-insensitive
substring over names, e-mail, phone, organisation name and type. Sorted by
last name then first name, case-insensitively. `page` and `per_page` below 1
are 1; `per_page` defaults to 25. `include_deleted` defaults to **true**.

§10.3.6 **Import.** `POST /api/contacts/import` takes a CSV. Required columns
`first_name`, `last_name`, `organisation_type`, `organisation_name`; optional
`email`, `phone_number`, `flow_ids` (`;`-separated) and `notes`; headers are
case-insensitive and in any order. A bad row is reported (`Row N: …`) and
skipped rather than aborting the import. A row matches an existing contact by
**e-mail** (case-insensitive), else by **phone number**, and updates it;
otherwise it creates one; a later row in the same file matches an earlier one.
Result: `{created, updated, errors, persistence_failed}`, `503` when
`persistence_failed`. *Deviation:* the body is the CSV itself
(`Content-Type: text/csv`, at most 1,000,000 bytes, UTF-8, a leading byte
order mark tolerated) rather than a multipart form — this workspace's `axum`
is built without multipart, and the only client is this page's own script.

§10.3.7 **Persistence disabled** (§10.1.4): a write is refused with `503`
and "Contact persistence is not configured."; the page renders and says so.
A failed write is `503` "Contact persistence is currently unavailable —
please try again shortly."

§10.3.8 **Contact details are personal data** of people who are not patients.
They are shown on this page and the Flows page, which is the feature, and
they MUST NOT reach a log line, a metric dimension or an error message: logs
carry the contact's `id` only.

§10.3.9 Routes: `GET /contacts`; `GET`/`POST /api/contacts`;
`GET`/`PUT`/`DELETE /api/contacts/{id}`; `POST /api/contacts/{id}/restore`;
`POST /api/contacts/import`. `PUT` of an unknown id is a `400` "Contact not
found." (upstream's behaviour, kept so the script need not change); `GET` and
`restore` of one are `404`.

### §10.4 Network Test and flow source servers (upstream `4b4f589`, `326b3ec`, `cdde18d`)

A tool for support staff to ask, from inside the dashboard's own network,
"can I reach this host and port, and how quickly?" — the question that comes
before every "is the sender's firewall open" conversation.

> **No separate application is needed.** Upstream also added
> `network_test_app/`, a bare container that sleeps so an engineer can
> `exec` into it with `nc`, `dig` and `curl`. That is an infrastructure
> convenience with no dashboard behaviour, and is not ported. The Network
> Test page is part of the dashboard and uses only the standard library's
> sockets: no subprocess, no raw sockets, no elevated container privileges.
> A hop-by-hop traceroute is out of scope for that reason.

§10.4.1 **A test** resolves the host, then makes `attempts` (default 4)
sequential TCP connects to the first resolved address, each with a timeout
(default 3 s), and the resolution has its own timeout (default 3 s). Connecting
to the address resolved once, rather than the name each time, stops a slow
resolver being paid for on every attempt. If resolution fails or times out,
every attempt is reported as failed with that error and no connect is tried.

§10.4.2 `POST /api/network-test/run` takes `{"host", "port"}` and returns

```json
{"host": "", "port": 0, "dns": {"resolved": true, "addresses": [], "resolve_time_ms": 0.0, "error": "only on failure"},
 "attempts": [{"success": true, "latency_ms": 0.0, "error": null}],
 "success_count": 0, "failure_count": 0, "loss_percent": 0.0,
 "min_latency_ms": null, "avg_latency_ms": null, "max_latency_ms": null,
 "success": true, "timestamp": 0.0}
```

`success` is true if any attempt connected. Latencies are milliseconds to two
places and `null` when nothing connected; `timestamp` is Unix seconds.
`history_warning` is added when the result could not be saved (§10.4.4); the
test itself has still run and its result is returned.

§10.4.3 **Validation** is syntactic only and is the boundary that keeps free
text out of the socket calls: a host is an IP address or a hostname (RFC 1123
labels of at most 63 characters, no leading or trailing hyphen, at most 253
in all); a port is an integer from 1 to 65535. Failure is a `400`
`{"error": "Invalid host or port."}` — the same message for both, naming
neither, so a response never echoes what was typed.

§10.4.4 **History.** Every run appends a sample (`timestamp`, `success`,
`avg_latency_ms`, `min_latency_ms`, `max_latency_ms`, `loss_percent`) to the
endpoint's document — partition `network-test`, id `history:{host}:{port}`,
type `network_test_history` — keeping the most recent 200.
`GET /api/network-test/history?host=&port=` returns `{host, port, samples}`;
`DELETE` removes the endpoint's history and with it the endpoint from the
list. `GET /api/network-test/list` returns every tested endpoint with its
latest sample, most recently tested first, and a `description` matched from
the flow source servers (case-insensitive host, exact port) or `null`; an
endpoint with no samples is omitted. With persistence disabled a run is not
an error and carries no warning (§10.1.4: nothing was ever going to be kept);
with persistence configured and failing it carries `history_warning`.

§10.4.5 **Flow source servers** are the managed list the page's "Flow
Source Server" picker draws from: `description` (non-empty, at most 200),
`url` (validated as a host, §10.4.3) and `port`. They are documents in
partition `flow-source-server`, type `flow_source_server` (§10.1.2), with
the public identifier in `source_id` (§10.1.3).

§10.4.6 **A source is identified by `url:port`, case-insensitively**, and two
sources MUST NOT share one. The document id is derived from that key
(`flow-source:{url lowercase}:{port}`), and a source is **created, not
upserted**: the store refuses to create a document whose id exists, so two
people adding the same server at once cannot both succeed. Editing a source
to a different `url:port` creates the new document first, then deletes the
old; if the delete fails the new one is removed again, so a failed edit never
leaves both.

§10.4.7 **Routes.** `GET /network-test`; `GET /network-test/config`;
`GET`/`POST /api/network-test/sources` (`201` on create);
`PUT`/`DELETE /api/network-test/sources/{id}`;
`POST /api/network-test/sources/import`. Statuses: `400` for a failed
validation or a duplicate, `503` for persistence, with the same
disabled/unavailable messages as §10.3.7 ("Source persistence is …").
Deleting a source that is not there is a no-op.

§10.4.8 **Import.** A CSV with columns `description`, `url`, `port`
(case-insensitive, any order). A row failing validation, or repeating an
earlier row of the same file (`Row N: duplicate of row M (…)`), is reported
and skipped; a row duplicating a stored source is reported
(`description (url:port): message`) and skipped; a persistence failure stops
the import. Result `{imported, errors, persistence_failed}`; `503` when
`persistence_failed`. As §10.3.6, the body is the CSV itself, not a multipart
form.

§10.4.9 *Deviation: no destination picker.* Upstream also fills a
"destination" picker from the destination host and port each flow was
discovered to deliver to. §3.1 rejects that discovery and `FLOWS` carries no
destination address, so the picker is not ported; any destination can be
tested by typing it, and the history list names it if it matches a source.

§10.4.10 **Finding for the Hub team: this is a connectivity probe with no
authentication.** Any host and port reachable from the dashboard's network
can be tested by anyone who can reach the dashboard, including loopback,
link-local (the instance metadata address) and the rest of the VNet. A
bare connect reveals only open or closed, and the dashboard has no
authentication of its own anywhere, so this is not newly exposed — but it is a
port scanner inside the VNet. Reproduced as upstream has it; the dashboard
MUST be reachable only by support staff (`spec/18-deployment.md`). Nothing
the operator types reaches a log line: the host and port are validated and
the log carries neither.

### §10.5 Topics and subscriptions on the dashboard (upstream `f7da495`)

The Service Bus page showed queues only; the estate also publishes to topics
(the MPI outbound flow, WDS). Upstream added an inventory of topics and their
subscriptions.

§10.5.1 **The snapshot.** The Service Bus namespace is read for its topics,
and each topic for its subscriptions, through the same management API and
version as queues (§4.6). A topic and a subscription each carry `name`,
`active` and `dead_letter`; a subscription also carries its `topic`. The
entity name of a subscription is `{topic}/{subscription}`.

§10.5.2 **Unknown is not zero** (§2.2.2, §3.3) applies to every count. A
topic or subscription whose depths could not be read has them absent, and
its health is `Unknown`. A subscription that ARM listed without counts is
unknown for the same reason, and not "empty".

§10.5.3 **Health** of a topic or subscription is §3.2's queue rule applied to
its own depths and no container: a dead-lettered message is `Critical`;
waiting messages at the critical threshold `Critical`, at the warning
threshold `Warning`; anything unread `Unknown`; otherwise `Healthy`.
*Deviation:* upstream's `_entity_health` has no dead-letter-critical rule
(it warns at `DLQ_WARNING_THRESHOLD`); this port keeps one rule for every
Service Bus entity rather than a second, weaker one for topics.

§10.5.4 **KPIs** over the snapshot: counts of queues, topics and
subscriptions, and the totals of waiting and dead-lettered messages for each
kind. As §2.2.3, a total spanning an entity that could not be read is
**absent**, and the count of unreadable entities is carried alongside.

§10.5.5 **Surfaces.** `/service-bus` lists topics with their subscriptions
beneath the queues; `/api/servicebus-metrics` carries `topics` (each with its
`subscriptions`, `health`, `known`) and `namespace_kpis` beside the existing
fields, which are unchanged (§2.3.2). With no Azure configuration both are
empty and the page says why (§1.2).

§10.5.6 *Not ported.* Upstream also (a) discovers each subscription's
consuming Container Apps and their `MICROSERVICE_ID`, (b) counts a flow's
owning topic into that flow's health and totals, attributing a shared topic
to its publisher only, (c) filters the Messages page and throughput chart by
queue, topic or subscription, and (d) adds `wds-to-wis` and
subscription-transformer roles to flow discovery. All four rest on
Container App environment discovery that §3.1 rejects, and `FLOWS` carries
no topic or subscription names to attribute. They are recorded rather than
approximated: a flow health computed from a guess at which topic is whose is
the class of error §2.3.1a records. Adding `topic` and `subscriptions` to a
`Flow` is the prerequisite for (b).

§10.1.6 **The store is wired.** `main` builds a Cosmos-backed store whenever
`COSMOS_ENDPOINT` is set (key or AAD per §5.6) and passes it to the router;
before this it always passed the disabled store, so a deployment's alarm
state, pauses, contacts and history went nowhere whatever the configuration
said. The HTTP client verifies certificates unconditionally (§4.7), so
`COSMOS_DISABLE_SSL_VERIFY` for the emulator's self-signed certificate is
accepted by configuration but not yet honoured by the transport; running
against the emulator needs the emulator's CA in `SSL_CERT_FILE`.

### §10.6 Alarms operator guide (upstream `daf7fdb`)

§10.6.1 `crates/dashboard/docs/working_with_alarms.md` is the guide for
support staff: reading the alarm status, pausing now or in advance (single
rule, flows, everything; after a duration, until a time, until cancelled),
the Alarm Pauses page and what a paused row means. It is written against this
port's pages (§10.2) and omits the per-flow configuration walk-through and
screenshots of upstream's guide, which describe screens this port does not
have (§10.2.10).

§10.6.2 Upstream's other two fixes in that commit do not apply: edits to
built-in seed rules being dropped on save and the pause dialog's custom
duration row always showing are defects of the Python's configuration screens
and of its Bootstrap markup, neither of which exists here. The seed rules of
§5.4.4 and §5.5.5 are listed by the evaluator and are not editable in this
port.
