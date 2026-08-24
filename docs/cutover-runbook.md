# Cutover runbook — Python to Rust

Implements [`spec/18-deployment.md`](../spec/18-deployment.md) §7.

This is the document somebody follows with the console open. It assumes the
Rust image for the container has been built and pushed (§6), and that the
person running it can deploy to the target environment.

**One container at a time.** §7.4: two at once means an incident has two
candidate causes.

---

## 0. The property that must hold throughout

§7.3. Rollback is scaling the Python revision back up and the Rust one to
zero, and it needs **no data migration**, because nothing in the Rust
changes:

- the shape of a queue or a topic subscription,
- the shape of a message or its application properties,
- the SQL schema.

If a step you are about to take would break that, **stop**. It is not a
cutover step; it is a migration, and it needs its own plan and its own
change record.

---

## 1. Two things to know before the first container

### 1.1 There is no "deploy alongside taking no traffic" without a change

§7.2 step 2 says to deploy the Rust revision alongside, taking no traffic.
The release pipeline as written **cannot do that**. It runs:

```sh
az containerapp update --name "$app" --resource-group "$rg" \
  --revision-suffix "$suffix" --image "$acr.azurecr.io/$image:$tag"
```

with no traffic weights. A container app in its default **single-revision
mode** deactivates the old revision and sends 100% of traffic to the new
one the moment that command returns. There is no alongside.

To get §7.2's behaviour the app must be in multiple-revision mode *before*
the deploy, with weights set explicitly:

```sh
az containerapp revision set-mode --name "$app" --resource-group "$rg" --mode multiple
```

Check first — this is a change to the app, and on a live app it is worth
its own approval:

```sh
az containerapp show --name "$app" --resource-group "$rg" \
  --query 'properties.configuration.activeRevisionsMode' -o tsv
```

If the answer is `Single` and you do not want to change it, use §2b below,
which is honest about the fact that you are doing a replace rather than a
shift.

### 1.2 Half of these containers have no traffic to shift at all

Revision traffic weights apply to **ingress**. The transformers, the
senders, `message-store-service` and `message-replay-job` have no ingress:
they pull from Service Bus. Two revisions of a queue consumer running at
once do not split by weight — they **compete for the same queue**.

Find out which kind you have:

```sh
az containerapp show --name "$app" --resource-group "$rg" \
  --query 'properties.configuration.ingress' -o json
```

`null` means a queue consumer: use **§2b**. An ingress object means use
**§2a**.

For a queue consumer, running both at once is not a failure. **No message
is processed twice** whatever the queue: peek-lock gives a message to one
receiver at a time, and it is not completed until it has been forwarded.

Ordering is the part that differs by queue:

- On a **session-enabled** queue — the transformer and sender ingress
  queues — a session is locked to one receiver, so all of one session's
  messages go to one revision and their order is preserved. The revisions
  divide the sessions between them.
- On a **non-session** queue — `local-inthub-messagestore` and
  `local-inthub-mpi-mock-receiver` are the two — messages interleave
  between the revisions. Nothing is lost or duplicated, and nothing
  promised ordering there in the first place.

It is still not a *traffic shift*, and this runbook should not pretend
otherwise: what you actually control is how many replicas of each revision
are running.

---

## 2. Per-container procedure

### Step 1 — confirm the deviations (§7.2.1)

Before anything is deployed. [`deviation-sign-off.md`](deviation-sign-off.md)
is the pack: all thirteen deviations from
[`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python), each
with what the Python does, what the Rust does instead, and a row to record
who confirmed it and when.

Its Summary table maps deviations to containers. A container may be cut
over once every row touching it is filled in.

Deviation **8** (v2.xml → ER7 component positions) needs the Hub team's
attention **regardless of the rewrite**, because the defect is in
production now. Do not let it ride along as a rewrite detail.

### Step 2 — point the build pipeline at this repository (§6.4)

The release refers to its build by **ADO pipeline name**, not by file path.
So the cutover step is repointing the named definition — for example
`HL7 Server - Build` — at this repository and at
`pipeline-ado/hl7serverrust-build.yml`.

The name stays. The ACR repository stays. The release definition is not
edited. Run the build and confirm it publishes an `imageTag` artefact as
before.

### Step 3a — deploy, ingress services

With the app in multiple-revision mode:

```sh
# Deploy the Rust image as a new revision with no traffic.
az containerapp update --name "$app" --resource-group "$rg" \
  --revision-suffix "rust-$tag" --image "$acr.azurecr.io/$image:$tag"

# Confirm it is Running and Healthy before it sees anything.
az containerapp revision list --name "$app" --resource-group "$rg" \
  --query '[].{name:name,active:properties.active,state:properties.runningState,traffic:properties.trafficWeight}' -o table

# All traffic still on the Python revision.
az containerapp ingress traffic set --name "$app" --resource-group "$rg" \
  --revision-weight "$python_revision=100" "$rust_revision=0"
```

Then shift in steps, checking §3 between each: `10` → `50` → `100`.

```sh
az containerapp ingress traffic set --name "$app" --resource-group "$rg" \
  --revision-weight "$python_revision=90" "$rust_revision=10"
```

> **MLLP is TCP ingress, not HTTP.** Weighted splitting happens per
> *connection*, not per message. A sending system that holds one long-lived
> connection will sit entirely on one revision — a 10% weight may deliver
> 0% or 100% of that peer's messages. Judge the shift by the counters in
> §3, never by the weight you set.

### Step 3b — deploy, queue consumers

There is no traffic to weight. Bring the Rust revision up beside the Python
one and let them share the queue, or replace outright — the first is
reversible in seconds and is preferred.

```sh
# Deploy. In single-revision mode this replaces; in multiple-revision mode
# it adds a revision you then scale.
az containerapp update --name "$app" --resource-group "$rg" \
  --revision-suffix "rust-$tag" --image "$acr.azurecr.io/$image:$tag"

# Multiple-revision mode: run one replica of each and watch the split.
az containerapp revision set --name "$app" --resource-group "$rg" \
  --revision "$rust_revision" --min-replicas 1 --max-replicas 1
```

Watch §3. When the Rust revision is processing cleanly, scale the Python
revision to zero:

```sh
az containerapp revision deactivate --name "$app" --resource-group "$rg" \
  --revision "$python_revision"
```

### Step 4 — hold for one full business day (§7.2.4)

Not one hour. The failures this catches are the ones tied to a daily
pattern: a nightly batch, a shift change, the message type that only one
site sends and only in the afternoon.

### Step 5 — scale the Python revision to zero, and **do not delete it**

§7.2.5. It is the rollback.

```sh
az containerapp revision deactivate --name "$app" --resource-group "$rg" \
  --revision "$python_revision"
```

---

## 3. What to watch

§7.2.3: the flow's queue depth, its dead-letter depth, and its
`messages_received` / `messages_sent` counters.

> **`MESSAGE_SENT` is emitted by nothing.** It is defined in both the
> Python and the Rust — `event_logger_lib/log_event.py` and
> `ihb-telemetry`'s `EventType` — and no component ever logs it. A panel or
> query watching "messages sent" reads a permanent zero in **both**
> implementations, so it tells you nothing about a cutover. The counter
> that means "this component finished with the message successfully" is
> `MESSAGE_PROCESSED`; for a sender, that is the delivery. See
> `spec/18-deployment.md` §7.9.

### 3.1 Queue and dead-letter depth

```sh
az servicebus queue show --namespace-name "$ns" --resource-group "$rg" \
  --name "$queue" --query 'countDetails' -o json
```

`activeMessageCount` climbing steadily means the consumer is not keeping
up. `deadLetterMessageCount` moving **at all** during a cutover is a stop
signal: investigate before shifting further.

### 3.2 Messages through the flow

Built from the same fields the dashboard parses
(`crates/dashboard/src/kql.rs`), so the two agree on where `workflow_id`
and `event_type` come from — this query is composed for the cutover rather
than copied from a panel:

```kql
AppTraces
| where TimeGenerated > ago(60m)
| where Message == "Integration Hub Event"
| extend workflow_id   = tostring(parse_json(Properties)["workflow_id"]),
         microservice  = tostring(parse_json(Properties)["microservice_id"]),
         event_type    = tostring(parse_json(Properties)["event_type"])
| where workflow_id == "phw-to-mpi"
| summarize count() by microservice, event_type, bin(TimeGenerated, 5m)
| order by TimeGenerated desc
```

`MICROSERVICE_ID` is **unchanged** across the cutover (§8.1), so this query
spans it and the counts should be continuous. A gap is the thing you are
watching for.

### 3.3 Failures

```kql
AppTraces
| where TimeGenerated > ago(60m)
| where Message has "Integration Hub Event"
| extend workflow_id = tostring(parse_json(Properties)["workflow_id"]),
         event_type  = tostring(parse_json(Properties)["event_type"])
| where event_type == "MESSAGE_FAILED"
| summarize Total = count() by workflow_id
```

### 3.4 Telling the two revisions apart

Not by `MICROSERVICE_ID` — that is deliberately identical (§8.2). Use the
container app revision name, or the OTel `service.version` resource
attribute.

---

## 4. Rollback

§7.3. At any point, and it needs no data migration.

```sh
# Ingress services: put the traffic back.
az containerapp ingress traffic set --name "$app" --resource-group "$rg" \
  --revision-weight "$python_revision=100" "$rust_revision=0"

# Queue consumers, and anything in single-revision mode:
az containerapp revision activate   --name "$app" --resource-group "$rg" --revision "$python_revision"
az containerapp revision deactivate --name "$app" --resource-group "$rg" --revision "$rust_revision"
```

Messages already in flight are safe either way: nothing is acknowledged
until it has been forwarded, so a message the Rust revision was holding
when you rolled back is redelivered to the Python one.

**Roll back rather than debug forward.** The Python revision is sitting
there costing nothing. Restore the flow first, then work out what happened
from the logs, which are still there.

---

## 5. The order

§7.1. Chosen so the first thing to move is the thing whose failure costs
least.

| # | Container | Kind | Why here |
|---|---|---|---|
| 1 | `hl7-mock-receiver` | ingress (MLLP) | a test double; nothing clinical depends on it |
| 2 | `http-mock-receiver` | ingress (HTTP) | likewise |
| 3 | `message-store-service` | queue consumer | a failure delays persistence; the queue absorbs the backlog and the pipeline keeps running |
| 4 | `hl7-sender`, one flow then the rest | queue consumer | first delivery-affecting move, contained to one flow |
| 5 | one transformer, then the rest | queue consumer | |
| 6 | `hl7-server`, one flow then the rest | ingress (MLLP) | the sending systems connect here; moved once the rest of the flow is known good |
| 7 | `hl7-soap-server` | ingress (HTTP) | |
| 8 | `message-replay-job` | queue consumer | |
| 9 | `buswatch`, then `dashboard` | ingress (HTTP) | operational tools; moved last so they are trustworthy while the rest moves |

Confirm the kind with the `az containerapp show` query in §1.2 rather than
trusting this column — the deployment is the authority, not this table.

---

## 6. Before you call it done

- [ ] Every deviation touching this container confirmed, with names and a date.
- [ ] The named ADO build pipeline points at this repository.
- [ ] One full business day held (§7.2.4).
- [ ] Queue depth stable; dead-letter depth unchanged.
- [ ] `MESSAGE_RECEIVED` / `MESSAGE_PROCESSED` counts continuous across the
      cutover. (Not `MESSAGE_SENT` — see §3.)
- [ ] The Python revision is scaled to zero and **still exists**.
- [ ] The alert rules still fire (§8.3).

The last one needs saying plainly: `Integration-Hub-Beta/docs/ALERT_STRATEGY.md`
is a discussion of *how alerting should be owned*, not a list of rules. The
concrete rules live in the IaC repository. §8.3 requires them to keep
firing unchanged, and confirming that means looking at the rules
themselves, not at that document.
