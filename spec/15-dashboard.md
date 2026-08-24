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
2. A rule whose `paused_until` is in the future reports `paused` and is not
   evaluated further. A `paused_until` in the past is **cleared** and
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
