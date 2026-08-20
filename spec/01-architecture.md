# 01 — Architecture

Flows, ports, queues, topology, and the lifecycle of a message. Normative:
these names are the deployment contract and MUST NOT be changed by this
rewrite.

Reference implementation:
`Integration-Hub-Beta/README.md`, `Integration-Hub-Beta/AGENTS.md`,
`Integration-Hub-Beta/local/*.env`,
`Integration-Hub-Beta/local/ServiceBusEmulatorConfig.json`.

---

## §1 Shape of the system

The Integration Hub is a set of single-purpose processes joined by Azure
Service Bus. No process calls another directly. Every process is one of five
kinds:

| Kind | Reads from | Writes to | Crates |
| --- | --- | --- | --- |
| **Ingress server** | A network socket (MLLP or HTTP/SOAP) | A pre-transform queue or a topic, plus the message-store queue | `hl7-server`, `hl7-soap-server` |
| **Transformer** | A pre-transform queue | A post-transform queue | `hl7-phw-transformer`, `hl7-chemo-transformer`, `hl7-pims-transformer`, `xml-fhir-proms-transformer` |
| **Sender** | A post-transform queue or a topic subscription | A network socket (MLLP or HTTP/SOAP), plus the message-store queue | `hl7-sender`, `hl7-subscription-sender`, `soap-sender`, `soap-subscription-sender` |
| **Persistence** | The message-store queue, or Azure SQL | Azure SQL, or a priority queue | `message-store-service`, `message-replay-job` |
| **Observation** | Service Bus management APIs, Azure Monitor, Cosmos DB | HTTP | `buswatch`, `dashboard` |

§1.1 A process MUST be restartable at any moment without loss. Every
Service Bus receive is `PEEK_LOCK`; a message is completed only after the
work it represents has succeeded.

§1.2 A process MUST NOT hold state that another process needs. All state is
in Service Bus, Azure SQL, or Cosmos DB.

§1.3 Every process MUST expose a TCP health check
([`05-health-check.md`](05-health-check.md)) when `HEALTH_CHECK_PORT` is
configured, and MUST shut down cleanly on `SIGTERM` and `SIGINT`.

---

## §2 Flows

A **flow** is one source-system-to-destination path. Each has a workflow
identifier (`WORKFLOW_ID`), an ingress port, a queue chain, and a
validation profile.

### §2.1 The flow table

| Flow | `WORKFLOW_ID` | Port | Ingress | Pre-transform queue | Transformer | Post-transform queue | Destination |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PHW → MPI | `phw-to-mpi` | 2575 | `hl7-server` | `…-phw-transformer-ingress` | `hl7-phw-transformer` | `…-mpi-sender-ingress` | MPI |
| Paris → MPI | `paris-to-mpi` | 2577 | `hl7-server` | *(none)* | *(none)* | `…-mpi-sender-ingress` | MPI |
| ChemoCare → MPI | `chemocare-to-mpi` | 2578 | `hl7-server` | `…-chemo-transformer-ingress` | `hl7-chemo-transformer` | `…-mpi-sender-ingress` | MPI |
| PIMS → MPI | `pims-to-mpi` | 2579 | `hl7-server` | `…-pims-transformer-ingress` | `hl7-pims-transformer` | `…-mpi-sender-ingress` | MPI |
| MPI → Topic (outbound) | `mpi-to-topic` | 2580 | `hl7-server` | topic `mpihl7input` | *(none)* | subscriptions of `mpihl7input` | Downstream systems |
| WDS → MPI | `wds-to-mpi` | 2582 | `hl7-server` | *(none)* | *(none)* | `…-mpi-sender-ingress` | MPI |
| Mosaiq → MPI | `mosaiq-to-mpi` | 2583 | `hl7-server` | *(none)* | *(none)* | `…-mpi-sender-ingress` | MPI |
| LIMS → MPI | `lims-soapserver` | 8080 | `hl7-soap-server` | `…-lims-transformer-ingress` | *(future)* | — | MPI |

§2.1.1 Queue names shown as `…-` are prefixed by the namespace and
environment: locally `local-inthub-`. The full name always comes from an
environment variable ([`02-configuration.md`](02-configuration.md)); nothing
in this workspace constructs a queue name from parts.

§2.1.2 Ports 2575, 2577–2583 and 8080 are the deployment contract. A
service MUST bind the port given by `PORT` and MUST NOT assume a default
other than the one in [`02-configuration.md`](02-configuration.md).

### §2.2 Per-flow validation profile

Each ingress server is configured with an HL7 version, an allow-list of
sending applications, and a validation flow name. These are the values in
`Integration-Hub-Beta/local/*.env` and are normative for the local stack;
production values come from Terraform.

| Flow | `HL7_VERSION` | `SENDING_APP` | `HL7_VALIDATION_FLOW` | `HEALTH_BOARD` | `PEER_SERVICE` |
| --- | --- | --- | --- | --- | --- |
| PHW | `2.5` | `252` | `phw` | `PHW` | `MPI` |
| Paris | `2.5.1` | `169` | `paris` | `PARIS` | `MPI` |
| ChemoCare | `2.4` | `245,212,192,224` | `chemo` | `CHEMO` | `MPI` |
| PIMS | `2.3.1` | *(per-site)* | `pims` | `PIMS` | `MPI` |
| MPI outbound | `2.5` | `100` | `mpi` | `MPI` | `TOPIC` |
| WDS | `2.5` | `WDS` | `wds` | `WDS` | `MPI` |
| Mosaiq | `2.5` | `285` | `mosaiq` | `MOSAIQ` | `MPI` |
| LIMS (SOAP) | — | — | schema group `phw` | `WLIMS` | `MPI` |

§2.2.1 `HL7_VALIDATION_FLOW` selects the XSD-derived dictionary used for
schema validation, per [`06-validation.md`](06-validation.md) §3. The value
`mpi` is special: it selects custom field validation and the MPI routing
property builder, and it **suppresses** flow-schema XML validation
(`Integration-Hub-Beta/hl7_server/hl7_server/generic_handler.py` guards on
`self.flow_name != "mpi"`).

§2.2.2 `SENDING_APP` is a comma-separated allow-list matched against
`MSH-3`. An empty or absent value disables the check.
[`07-hl7-server.md`](07-hl7-server.md) §3.2.

### §2.3 Sessions

§2.3.1 Every pipeline queue in the local emulator configuration has
`RequiresSession: true`. A sender MUST set the session identifier given by
`EGRESS_SESSION_ID`, and a receiver MUST open a session receiver for
`INGRESS_SESSION_ID`.

§2.3.2 Session identifiers are flow names, not message identifiers:
`phw-to-mpi`, `chemocare-to-mpi`, `mpi`, `mpi-outbound`, `lims-soapserver`.
Ordering within a flow is therefore guaranteed and a stalled message blocks
its flow. This is the intended behaviour and MUST be preserved.

§2.3.3 The message-store queue is **not** session-enabled. The store
receiver opens a non-session receiver
(`message_store_service.py` passes `session_id=None`), but the *event* it
consumes carries the originating component's session identifier in its
`SessionId` field so replay can restore it.

---

## §3 The lifecycle of a message

The normative end-to-end sequence for the PHW flow. Other flows differ only
in which steps are present.

```
 1. PHW opens a TCP connection to hl7-server:2575 and writes an MLLP frame.
 2. hl7-server de-frames it, decodes UTF-8, and parses the ER7.
 3. EventLogger: MESSAGE_RECEIVED.               metric: messages_received +1
 4. HL7Validator: version, sending application, flow-specific field rules.
 5. EventLogger: VALIDATION_SUCCESS (or VALIDATION_FAILED and stop).
 6. Tracking metadata is minted:
       MessageReceivedAt = now, UTC, RFC 3339
       CorrelationId     = a fresh UUID v4
       WorkflowID        = WORKFLOW_ID
       SourceSystem      = MSH-3
 7. Flow-schema validation produces the v2.xml rendering of the message.
 8. Optional standard-version validation (HL7_VALIDATION_STANDARD).
 9. Flow-specific routing properties are added (MPI flow only).
10. A store event {raw, xml, metadata} is sent to the message-store queue.
      Failure here is logged and does NOT stop the message.
11. The raw ER7 is sent to the pre-transform queue with the tracking
    metadata as application_properties and MSH-10 as the message id.
      Failure here DOES stop the message; no ACK is returned.
12. An ACK is built from the inbound MSH and written back on the socket.
13. EventLogger: MESSAGE_PROCESSED.

14. hl7-phw-transformer receives from the pre-transform queue.
15. EventLogger: MESSAGE_RECEIVED.
16. The message is transformed (spec 09).
17. The transformed ER7 is sent to the post-transform queue, carrying the
    SAME application_properties it arrived with — the correlation id is
    preserved across the hop.
18. EventLogger: MESSAGE_PROCESSED. The Service Bus message is completed.

19. hl7-sender receives from the post-transform queue.
20. On the FIRST delivery attempt only, a store event is emitted.
21. The throttle is applied (MAX_MESSAGES_PER_MINUTE).
22. The message is written to MPI over MLLP and an ACK is awaited.
23. MSA-1 in {AA, CA} → success: metric messages_sent +1, complete.
    Anything else, or a timeout → failure: abandon, backoff, retry.
```

§3.1 The correlation identifier MUST be minted exactly once, at ingress, and
MUST be propagated unchanged on every subsequent hop. A component that
receives a message without a `CorrelationId` application property MUST log a
warning and continue.

§3.2 W3C trace context (`traceparent`, `tracestate`) MUST be injected into
`application_properties` on send and extracted on receive, so a message is
one distributed trace end to end.
[`04-observability.md`](04-observability.md) §4.

§3.3 The message store write at step 10 is **non-blocking**: an exception
MUST be logged and swallowed. A failure to persist MUST NOT prevent the
acknowledgement of a message that was successfully forwarded.

§3.4 The Service Bus send at step 11 is **blocking**: an exception MUST
propagate, so no ACK is sent and the source system retries. Acknowledging a
message that was not forwarded is the one failure this system must never
produce.

§3.5 At step 20, the store event is emitted only when the Service Bus
delivery count indicates a first attempt
(`hl7_sender/application.py :: _is_first_delivery_attempt`). A missing or
unparseable delivery count MUST be treated as a first attempt.

---

## §4 Queues and topics

### §4.1 Local names

From `Integration-Hub-Beta/local/ServiceBusEmulatorConfig.json` and the
`.env` files. Sessions required unless noted — and **only two are not**:
`local-inthub-messagestore` and `local-inthub-mpi-mock-receiver`. Reaching
any of the others without a session id is refused by the broker with
`it is not possible for an entity that requires sessions to create a
non-sessionful message receiver`, which is a configuration error rather than
an idle queue.

| Name | Kind | Used by |
| --- | --- | --- |
| `local-inthub-phw-ingress` | queue | *(reserved)* |
| `local-inthub-phw-transformer-ingress` | queue | PHW server → PHW transformer |
| `local-inthub-chemo-transformer-ingress` | queue | Chemo server → Chemo transformer |
| `local-inthub-pims-transformer-ingress` | queue | PIMS server → PIMS transformer |
| `local-inthub-lims-transformer-ingress` | queue | LIMS SOAP server → *(future)* |
| `local-inthub-mpi-sender-ingress` | queue | Transformers, Paris/WDS/Mosaiq servers → MPI sender |
| `local-inthub-mpi-mock-receiver` | queue, **no session** | Mock receiver's forwarding sink |
| `local-inthub-messagestore` | queue, **no session** | All components → message store service |
| `local-inthub-priority-messagequeue` | queue | Replay job → priority processing |
| `local-inthub-soap-sender` | queue | SOAP sender ingress |
| `mpihl7input` | topic | MPI outbound server → subscription senders |
| `mpihl7input-test-subscription` | subscription | Chemo subscription sender |

### §4.2 Alert thresholds

Operational thresholds, consumed by the dashboard
([`15-dashboard.md`](15-dashboard.md)):

| Variable | Default |
| --- | --- |
| `QUEUE_WARNING_THRESHOLD` | `10` |
| `QUEUE_CRITICAL_THRESHOLD` | `50` |
| `DLQ_WARNING_THRESHOLD` | `1` |

---

## §5 Message identity

| Identifier | Minted by | Carried in | Purpose |
| --- | --- | --- | --- |
| `MSH-10` message control id | The source system | Service Bus `message_id` | Duplicate detection; the ACK's `MSA-2` |
| `CorrelationId` | The ingress server, per §3.1 | `application_properties`, the store event, every log line | Following one message across hops and retries |
| `traceparent` | The OTel SDK | `application_properties` | Distributed tracing |
| `SessionId` | Configuration | Service Bus session | Ordering within a flow |
| `ReplayBatchId` | An operator | `REPLAY_BATCH_ID` | Selecting rows for a replay run |

§5.1 A component MUST NOT mint a new `CorrelationId` for a message that
already has one.

§5.2 The Service Bus `message_id` MUST be set to `MSH-10` on ingress
(`generic_handler.py :: _send_to_service_bus`), so Service Bus duplicate
detection, where enabled, keys on the source system's own identifier.

---

## §6 Ordering and delivery guarantees

§6.1 **At-least-once.** Every hop may deliver a message more than once. A
transformer or sender MUST be safe to re-run on the same input: transforms
are pure functions of the message, and delivery is idempotent at the
destination by `MSH-10`.

§6.2 **In-order within a session.** §2.3.2's flow-level sessions mean a
failing message blocks the messages behind it in its flow. The receive loop
therefore abandons the failing message *and every message already received
behind it* in the same batch
(`message_receiver_client.py :: per_message_adapter`), so the batch is
retried as a unit rather than skipping ahead.

§6.3 **Bounded retry with backoff.** A failing receive schedules the next
attempt after a delay that starts at 5 s and doubles to a ceiling of 15
minutes, reset on any success.
[`03-message-bus.md`](03-message-bus.md) §5.

§6.4 Service Bus's own `MaxDeliveryCount` (10 in the local configuration)
eventually dead-letters a message that never succeeds. Nothing in this
workspace dead-letters explicitly.

---

## §7 Deployment topology

§7.1 Each crate with a `[[bin]]` ships as one container image, running as a
non-root user, in Azure Container Apps. [`18-deployment.md`](18-deployment.md).

§7.2 A single crate is deployed multiple times with different environment
variables: `hl7-server` runs once per flow (PHW, Paris, ChemoCare, PIMS,
MPI outbound, WDS, Mosaiq), and `hl7-sender` runs once per destination.
Nothing in the code branches on which deployment it is; only configuration
differs.

§7.3 Web-facing services (`hl7-soap-server`, `http-mock-receiver`,
`buswatch`, `dashboard`) listen on port 8080. Background workers listen on
no port other than their health check.

§7.4 Corporate CA certificates are mounted as a PEM bundle and are trusted
by the process's TLS stack via `SSL_CERT_FILE`; the image does not rebuild
the OS trust store. [`18-deployment.md`](18-deployment.md) §4.
