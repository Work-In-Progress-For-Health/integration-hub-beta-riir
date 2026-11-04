# Integration Hub Beta (Rust) — Specification

This directory is the **single source of truth** for the behaviour of every
crate in this workspace. Code implements a section here; a section here does
not describe code. Where the two disagree, the specification is right and
the code is a defect.

The [`plan.md`](../plan.md) at the workspace root says what is being built
and in what order. [`tasks.md`](../tasks.md) is the ordered task list. This
directory says what "built" means.

---

## Sections

| File | Covers |
| --- | --- |
| [`01-architecture.md`](01-architecture.md) | Flows, ports, queues, topology, message lifecycle |
| [`02-configuration.md`](02-configuration.md) | Every environment variable, its type, default, and parse rule |
| [`03-message-bus.md`](03-message-bus.md) | Service Bus senders, receivers, sessions, retry, metadata |
| [`04-observability.md`](04-observability.md) | Event logging, PII redaction, metrics, OTel, trace propagation |
| [`05-health-check.md`](05-health-check.md) | The TCP health-check listener |
| [`06-validation.md`](06-validation.md) | HL7 validation, flow schemas, ER7 ↔ v2.xml conversion |
| [`07-hl7-server.md`](07-hl7-server.md) | MLLP ingress, ACK construction, custom validation |
| [`08-hl7-soap-server.md`](08-hl7-soap-server.md) | SOAP ingress, WSDL, faults |
| [`09-transformers.md`](09-transformers.md) | Transformer runtime; PHW, ChemoCare and PIMS mappings |
| [`10-senders.md`](10-senders.md) | MLLP and SOAP egress, subscriptions, throttling |
| [`11-mock-receivers.md`](11-mock-receivers.md) | HL7 and HTTP mock receivers |
| [`12-message-store.md`](12-message-store.md) | Message store service and its SQL schema |
| [`13-message-replay.md`](13-message-replay.md) | Replay job |
| [`14-buswatch.md`](14-buswatch.md) | Queue inspector |
| [`15-dashboard.md`](15-dashboard.md) | NOC dashboard, flows, alarms, persistence, e-mail |
| [`16-proms-transformer.md`](16-proms-transformer.md) | WPAS XML → FHIR PROMs transformer |
| [`17-testing-and-quality.md`](17-testing-and-quality.md) | Test taxonomy, fixtures, lints, gates |
| [`18-deployment.md`](18-deployment.md) | Container contract, images, Compose, cutover |
| [`19-rest-servers.md`](19-rest-servers.md) | REST ingress: `hl7-rest-server` (RISP REST to HL7) and `rest-server` (generic SOAP / XML) |
| [`20-lookup-service.md`](20-lookup-service.md) | Reference-data lookup service for transformers (Phase 1, not production ready) |
| [`21-local-ci.md`](21-local-ci.md) | Running the gate locally: one `cargo xtask gate`, tiers, hooks, the evidence of a run, what must stay hosted |
| [`integration-hub-tester-component-preview.md`](integration-hub-tester-component-preview.md) | Integration Hub tester: developer tool that previews one component's logic on one message, no Service Bus |
| [`ultra7-test-message-sender.md`](ultra7-test-message-sender.md) | Ultra7: developer tool that sends synthetic test messages (MLLP, REST, SOAP) |
| [`rust-msrv-n-minus-3.md`](rust-msrv-n-minus-3.md) | The minimum supported Rust version, and why |
| [`agents-directory-name-is-lowercase.md`](agents-directory-name-is-lowercase.md) | What an AI agents directory is called |

---

## §0 Conventions

### §0.1 Normative language

- **MUST**, **MUST NOT**, **REQUIRED** — an absolute requirement. A
  violation is a defect, and there is a test for it.
- **SHOULD**, **SHOULD NOT** — a requirement that may be traded away only
  with a recorded reason in the implementing crate's rustdoc.
- **MAY** — genuinely optional.

Anything not stated with one of these words is background, rationale, or
explanation, and is not binding.

### §0.2 Section numbering

Sections are numbered `§N.M` within their file and are **stable
identifiers**. Tests and rustdoc cite them (`See spec/07-hl7-server.md
§3.2`). A section is never renumbered; new material is appended, and
superseded material is struck through with a pointer to what replaced it.

### §0.3 Citing the specification from code

Every public item's rustdoc MUST name the section it implements:

```rust
/// Builds the acknowledgement for a received message.
///
/// Implements `spec/07-hl7-server.md` §4 (ACK construction).
```

A citation is written as a **code span, not a link**: `` `spec/07-…` ``.
Rustdoc reads `[`x`]` as an intra-doc link to a Rust item named `x` and
fails the documentation build when there is none, and `spec/` files are not
Rust items. The gate runs `cargo doc` with `-D warnings`
([`17-testing-and-quality.md`](17-testing-and-quality.md) §1), so getting
this wrong is a build failure rather than a broken link nobody notices.

Every test whose subject is a normative clause MUST name it, either in the
test name or in a comment on its first line.

### §0.4 The Python is the oracle, not the design

The Python at [`../../Integration-Hub-Beta`](../../Integration-Hub-Beta) is
the reference implementation. Where this specification records an exact
string, an exact default, or an exact ordering, it is because the Python
produces it and something outside this workspace depends on it.

Where the Python's behaviour is accidental and nothing depends on it, this
specification says what the Rust does instead, and
[`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python) lists
every such divergence in one place.

Citations to the Python use a repository-relative path and, where useful, a
symbol: `Integration-Hub-Beta/hl7_server/hl7_server/generic_handler.py ::
GenericHandler.reply`.

### §0.5 Terminology

| Term | Meaning |
| --- | --- |
| **ER7** | The pipe-and-hat encoding of an HL7 v2 message. `MSH\|^~\&\|…` |
| **v2.xml** | The official XML representation of HL7 v2, namespace `urn:hl7-org:v2xml` |
| **MLLP** | Minimal Lower Layer Protocol: `<VT> message <FS><CR>` over TCP |
| **Flow** | One source-system-to-destination path, e.g. `PHW → MPI` |
| **Pre-transform queue** | The Service Bus queue an ingress server writes to |
| **Post-transform queue** | The queue a sender reads from |
| **Correlation ID** | A UUID v4 minted at ingress and carried on every hop |
| **Message store** | The Azure SQL table `monitoring.Message` and the queue feeding it |
| **MPI** | Master Patient Index — the principal destination system |
| **WRDS** | Welsh Reference Data Service — the source of code mappings |
| **Health board** | An NHS Wales organisational unit; a metric dimension |

### §0.6 Character encoding and line endings

- All message payloads are **UTF-8**. A payload that is not valid UTF-8 MUST
  be rejected at the boundary that received it, and MUST NOT be forwarded.
- HL7 v2 segments are terminated by a single **`\r`** (0x0D). A `\n` in
  received text MUST be normalised to `\r` before parsing, and MUST NOT
  appear in anything this workspace emits as ER7.
- Nothing in this workspace emits a trailing newline after a message.

### §0.7 Time

- All timestamps recorded in telemetry, in the message store, and in
  Service Bus metadata are **UTC**, formatted exactly as Python's
  `datetime.now(timezone.utc).isoformat()` produces:

  ```
  2026-08-20T16:09:01.991442+00:00     microseconds present
  2026-08-20T09:15:00+00:00            microseconds exactly zero
  ```

  Three details are load-bearing, because `MessageReceivedAt` crosses a
  queue and is parsed at the other end
  ([`12-message-store.md`](12-message-store.md) §3.5):

  1. The offset is written **`+00:00`**, not `Z`.
  2. The fractional part has **six digits** when present.
  3. The fractional part is **omitted entirely** when the microsecond
     component is zero — Python's `isoformat` does this, and a consumer
     that string-matches on a fixed width would be surprised by it once in
     a million messages rather than never.
- HL7 timestamps in message content use HL7's own `YYYYMMDDHHMMSS` form and
  are **local to the message**, not converted. Timezone handling per flow is
  specified in [`09-transformers.md`](09-transformers.md).
- `MSH-7` on a generated ACK uses local wall-clock time formatted
  `%Y%m%d%H%M%S`, matching
  `Integration-Hub-Beta/hl7_server/hl7_server/hl7_ack_builder.py`.

### §0.8 Patient-identifiable information

This is the rule that overrides every other consideration in this
specification.

1. A **log line, an error message, a span attribute, a metric dimension, or
   an HTTP response body MUST NOT contain an unredacted message payload or
   any field value taken from one.** The only exceptions are the fields
   explicitly allow-listed in [`04-observability.md`](04-observability.md)
   §2.
2. The **message store MAY hold the full raw payload**, because replay
   requires it and the store is access-controlled.
3. An error type MUST carry a structural location (a segment name, a field
   path, an XPath) and a reason, never a value.
4. Every crate that can render an error containing message-derived text MUST
   have a test that feeds a known PII marker through the failure path and
   asserts the marker does not appear in the rendered error.

### §0.9 Failure vocabulary

| Outcome | Meaning | Service Bus disposition |
| --- | --- | --- |
| **Accepted** | Processed and forwarded | `complete` |
| **Rejected** | Structurally or semantically invalid; will never succeed | `complete` after logging, or dead-letter where the flow says so |
| **Deferred** | A transient failure; the same message may succeed later | `abandon`, with backoff |

A handler MUST NOT abandon a message that can never succeed, and MUST NOT
complete a message that was not delivered.
