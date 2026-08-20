# Plan — Integration Hub Beta, rewritten in Rust

A rewrite of [`Integration-Hub-Beta`](../Integration-Hub-Beta) (Python 3.13,
`uv`, `hl7apy`, `azure-servicebus`, Flask/FastAPI) as a single Cargo
workspace of Rust crates.

This file is the **plan**: what we are building, how it is organised, what
we depend on, what order the work happens in, and which decisions have
already been made so they do not have to be re-litigated. It is not the
specification — [`spec/`](spec/) is the normative source of truth, and every
implementation decision must trace to a section there. It is not the task
list either — [`tasks.md`](tasks.md) is.

---

## Table of contents

- [1. Goal and non-goals](#1-goal-and-non-goals)
- [2. What the system does](#2-what-the-system-does)
- [3. Method: spec-driven development](#3-method-spec-driven-development)
- [4. Workspace layout](#4-workspace-layout)
- [5. Crate-by-crate mapping from Python](#5-crate-by-crate-mapping-from-python)
- [6. Dependencies](#6-dependencies)
- [7. Cross-cutting engineering standards](#7-cross-cutting-engineering-standards)
- [8. Known gaps and how they are closed](#8-known-gaps-and-how-they-are-closed)
- [9. Deliberate deviations from the Python](#9-deliberate-deviations-from-the-python)
- [10. Phasing](#10-phasing)
- [11. Definition of done](#11-definition-of-done)
- [12. Risks](#12-risks)

---

## 1. Goal and non-goals

### Goal

Produce a Rust implementation of the Integration Hub that is **behaviourally
equivalent** to the Python for every message that crosses it, deployable
into the same Azure Container Apps estate with the **same environment
variables, the same queue and topic names, the same ports, and the same
container contract**, so that a flow can be cut over one container at a
time.

Behavioural equivalence is defined at three boundaries, in priority order:

1. **The wire.** Bytes out of an MLLP socket, a SOAP response body, a
   Service Bus message body and its `application_properties`, and a row in
   `monitoring.Message` must match what the Python produced for the same
   input. This is the contract other systems (MPI, PIMS, ChemoCare, PHW,
   Paris, WDS, Mosaiq) already depend on.
2. **The configuration.** Every environment variable keeps its name,
   its default, and its parse semantics — including the Python's habit of
   treating a blank string as absent.
3. **The telemetry.** Event names, event types, metric names, and metric
   dimensions keep their spelling, because Log Analytics queries, the NOC
   dashboard, and the alert rules in `docs/ALERT_STRATEGY.md` are written
   against them.

Where the Python has a defect that the wire does not depend on, the Rust
fixes it and [§9](#9-deliberate-deviations-from-the-python) records the
difference. Nothing is fixed silently.

### Non-goals

- **Not** a redesign. The flow topology, the queue names, the segment-level
  mapping rules, and the validation policy are inherited as-is. Improving
  them is a separate conversation with the Hub team.
- **Not** a port of the Python's *internals*. `hl7apy` object graphs,
  `configparser`, `pyodbc`, and the `AZURE_MONITOR_OWNER` handshake between
  `event_logger_lib` and `metric_sender_lib` are implementation detail; the
  Rust reaches the same observable result by whatever means Rust makes
  natural.
- **Not** a replacement for the Python repository. The Python stays in
  place. Cutover is per-container and is the Hub team's call.
- **Not** the Terraform or the Azure DevOps pipelines. Those live in sibling
  repositories; this workspace only has to produce images that satisfy the
  same contract.

---

## 2. What the system does

NHS Wales systems speak HL7 v2 in mutually incompatible dialects. The
Integration Hub receives their messages, validates them, transforms them
into the dialect the Master Patient Index (MPI) accepts, and delivers them —
persisting every message on the way through so any of it can be replayed.

```
 source system                                              destination
 ─────────────                                              ───────────
  PHW      ─┐                                            ┌─►  MPI
  Paris    ─┤  MLLP  ┌──────────┐   pre-transform queue  │
  ChemoCare─┼───────►│hl7_server├──►(Service Bus)──┐     │
  PIMS     ─┤        └────┬─────┘                  │     │
  Mosaiq   ─┘             │                        ▼     │
                          │              ┌──────────────┐│
  LIMS/WDS ── SOAP ──►┌────────────┐     │ transformer  ││
                      │hl7_soap_srv├──┐  │ (PHW/Chemo/  ││
                      └────────────┘  │  │  PIMS)       ││
                                      │  └──────┬───────┘│
                          │           │         │        │
                          ▼           ▼         ▼        │
                    ┌──────────────────────────────┐     │
                    │ message-store queue          │     │
                    └──────────┬───────────────────┘     │
                               ▼                         │
                  ┌────────────────────────┐             │
                  │ message_store_service   │            │
                  │      → Azure SQL        │            │
                  └────────────┬───────────┘             │
                               │  replay                 │
                  ┌────────────▼───────────┐             │
                  │ message_replay_job      │            │
                  └────────────┬───────────┘             │
                               │      post-transform queue│
                               └──────►(Service Bus)──────┤
                                                          │
                                       ┌──────────────────┴───┐
                                       │ hl7_sender / soap_*  │
                                       │ subscription senders │
                                       └──────────────────────┘
```

Alongside the pipeline: `buswatch` (queue inspector), `dashboard` (NOC
monitoring, alarms, e-mail), and mock receivers used by tests and by the
local Compose stack.

The seven flows, their ports and their queues are normative and are recorded
in [`spec/01-architecture.md`](spec/01-architecture.md).

---

## 3. Method: spec-driven development

The `spec/` directory is the **single source of truth**. Code implements a
spec section; a spec section is not describing code.

The loop for each unit of work is:

1. **Read the Python.** Every behaviour, including the accidental ones.
2. **Write the spec section.** Normative, numbered, with the exact strings,
   the exact defaults, and the exact error text where the wire depends on
   it. Where the Python is ambiguous or buggy, the spec says which
   behaviour is normative and why.
3. **Write the tests from the spec**, before or alongside the code. Every
   normative clause that can be observed gets a test that names it.
4. **Write the code.** Rustdoc on every public item cites the spec section
   it implements (`See spec/07-hl7-server.md §3.2`).
5. **Gate.** `cargo fmt --check`, `cargo clippy --workspace --all-targets
   --all-features -- -D warnings` with pedantic on, `cargo test --workspace`,
   `cargo doc` with no warnings, and `cargo test --doc` so every rustdoc
   example is executed.

Spec sections are numbered `§N.M` and are stable identifiers. Renumbering a
section is a breaking change to the tests that cite it, so sections are
appended rather than renumbered.

### Spec file set

| File | Covers |
| --- | --- |
| `spec/index.md` | Map of the spec, conventions, normative language |
| `spec/01-architecture.md` | Flows, ports, queues, topology, message lifecycle |
| `spec/02-configuration.md` | Every environment variable, its type, default, and parse rule |
| `spec/03-message-bus.md` | Service Bus: senders, receivers, sessions, retry, metadata |
| `spec/04-observability.md` | Event logging, PII redaction, metrics, OTel, trace propagation |
| `spec/05-health-check.md` | TCP health-check server |
| `spec/06-validation.md` | HL7 validation, flow XSD schemas, ER7 ↔ v2.xml conversion |
| `spec/07-hl7-server.md` | MLLP ingress server, ACK construction, custom validation |
| `spec/08-hl7-soap-server.md` | SOAP ingress server, WSDL, faults |
| `spec/09-transformers.md` | Transformer runtime and the PHW / ChemoCare / PIMS mappings |
| `spec/10-senders.md` | MLLP sender, subscription sender, SOAP senders, throttling |
| `spec/11-mock-receivers.md` | HL7 and HTTP mock receivers |
| `spec/12-message-store.md` | Message store service and the SQL schema |
| `spec/13-message-replay.md` | Replay job |
| `spec/14-buswatch.md` | Queue inspector |
| `spec/15-dashboard.md` | NOC dashboard, flows, alarms, Cosmos persistence, e-mail |
| `spec/16-proms-transformer.md` | WPAS XML → FHIR PROMs transformer |
| `spec/17-testing-and-quality.md` | Test taxonomy, fixtures, lints, coverage expectations |
| `spec/18-deployment.md` | Container contract, images, local Compose, cutover |

---

## 4. Workspace layout

One Cargo workspace, one `Cargo.lock`, one `target/`.

```
integration-hub-beta-riir/
├── Cargo.toml                  workspace root: members, shared deps, lints
├── Cargo.lock
├── plan.md                     this file
├── tasks.md                    the ordered task list
├── README.md                   user-facing tour
├── spec/                       the normative specification
├── xtask/                      workspace automation (schema codegen, checks)
├── local/                      Compose stack, .env files, sample messages
└── crates/
    ├── ihb-config/             environment configuration primitives
    ├── ihb-health/             TCP health-check listener
    ├── ihb-telemetry/          events, redaction, metrics, OTel
    ├── ihb-bus/                Azure Service Bus clients
    ├── ihb-fields/             HL7 field navigation and copying helpers
    ├── ihb-validation/         HL7 validation + ER7 ↔ v2.xml + flow schemas
    ├── ihb-runtime/            signals, shutdown, message-processing loop
    ├── ihb-transformer/        transformer trait and its runtime
    ├── ihb-sql/                Azure SQL access shared by store and replay
    ├── hl7-server/             MLLP ingress                     [bin]
    ├── hl7-soap-server/        SOAP ingress                     [bin]
    ├── hl7-phw-transformer/    PHW → MPI                        [bin]
    ├── hl7-chemo-transformer/  ChemoCare → MPI                  [bin]
    ├── hl7-pims-transformer/   PIMS → MPI                       [bin]
    ├── hl7-sender/             MLLP egress from a queue         [bin]
    ├── hl7-subscription-sender/MLLP egress from a subscription  [bin]
    ├── soap-sender/            SOAP egress from a queue         [bin]
    ├── soap-subscription-sender/ SOAP egress from a subscription[bin]
    ├── hl7-mock-receiver/      MLLP test double                 [bin]
    ├── http-mock-receiver/     SOAP/HTTP test double            [bin]
    ├── message-store-service/  Service Bus → Azure SQL          [bin]
    ├── message-replay-job/     Azure SQL → Service Bus          [bin]
    ├── buswatch/               queue inspector                  [bin]
    ├── dashboard/              NOC dashboard                    [bin]
    └── xml-fhir-proms-transformer/ WPAS XML → FHIR              [bin]
```

**Naming.** Shared libraries take the `ihb-` prefix (Integration Hub Beta)
so they never collide with a crates.io name and so a reader can tell at a
glance whether a dependency is ours. Service crates keep the Python service
name with underscores turned into hyphens, because the container image name,
the Azure Container App name, the ADO pipeline name and the `.env` file name
all already use that spelling and changing it would break the deployment
contract for no gain.

**Every service crate is a library plus a thin binary.** The binary reads
configuration, installs signal handlers and logging, and calls into the
library. Everything testable lives in the library, so integration tests can
drive a real listener against a fake bus.

---

## 5. Crate-by-crate mapping from Python

| Python | Rust crate | Notes |
| --- | --- | --- |
| `shared_libs/health_check_lib` | `ihb-health` | Direct port; tokio listener instead of a thread |
| `shared_libs/processor_manager_lib` | `ihb-runtime` | Signal handling + the receive loop + span wrapping |
| `shared_libs/event_logger_lib` | `ihb-telemetry` (`events`, `redact`) | Redaction is ours, not `er7-redact`; see the note below |
| `shared_libs/metric_sender_lib` | `ihb-telemetry` (`metrics`) | Counters and histograms via `opentelemetry` |
| `shared_libs/otel_lib` | `ihb-telemetry` (`otel`) | Tracing subscriber + W3C propagation |
| `shared_libs/message_bus_lib` | `ihb-bus` | Sender/receiver/subscription/store clients, metadata keys, retry |
| `shared_libs/field_utils_lib` | `ihb-fields` | Built on `er7` paths and `hl7-2` typed access; no `hl7apy` fidelity shim needed |
| `shared_libs/hl7_validation` | `ihb-validation` | XSD → `hl7-2` JSON dictionaries at build time; validation via `hl7-2`; conversion via `hl7-2-from-er7-into-xml` / `hl7-2-from-xml-into-er7` |
| `shared_libs/transformer_base_lib` | `ihb-transformer` | `Transformer` trait + `run` |
| `hl7_server` | `hl7-server` | MLLP via `hl7-2-mllp`; ACK via `hl7-2-mllp::ack` |
| `hl7_soap_server` | `hl7-soap-server` | Envelope/fault/WSDL via `hl7-2-soap`; HTTP via `axum` |
| `hl7_phw_transformer` | `hl7-phw-transformer` | |
| `hl7_chemo_transformer` | `hl7-chemo-transformer` | |
| `hl7_pims_transformer` | `hl7-pims-transformer` | |
| `hl7_sender` | `hl7-sender` | MLLP client via `hl7-2-mllp::Transport` |
| `hl7_subscription_sender` | `hl7-subscription-sender` | |
| `soap_sender` | `soap-sender` | `reqwest` + `hl7-2-soap::response::evaluate` |
| `soap_subscription_sender` | `soap-subscription-sender` | |
| `hl7_mock_receiver` | `hl7-mock-receiver` | Adopts the existing `Integration-Hub-Beta/hl7_mock_receiver_rust` |
| `http_mock_receiver` | `http-mock-receiver` | |
| `message_store_service` | `message-store-service` + `ihb-sql` | `tiberius` instead of `pyodbc` |
| `message_replay_job` | `message-replay-job` + `ihb-sql` | |
| `buswatch` | `buswatch` | `axum` + `askama` |
| `dashboard` | `dashboard` | `axum` + `askama`; Azure ARM / Log Analytics / Cosmos over REST |
| `xml_fhir_proms_transformer` | `xml-fhir-proms-transformer` | |
| `my_tools/*`, `network_test_app` | *(not ported in this plan)* | Developer tooling; revisit after the pipeline is green |

**Why redaction is not `er7-redact`.** `er7-redact` de-identifies: a policy
of per-path rules, each saying what to do with one field — pseudonymise this
identifier, truncate that date, clear that address — leaving everything not
named in the policy intact. The Python's log redaction is the opposite
shape: **mask everything, except a fixed allow-list of ten MSH fields**
([`spec/04-observability.md`](spec/04-observability.md) §2.6). Expressing
that as a path policy would need a rule for every field of every segment in
every release, and a segment nobody thought of would default to *visible*,
which is the wrong way for this failure to fall. The Rust reproduces the
Python's algorithm directly — split on the field separator, mask, rejoin —
which is also what makes the output byte-identical rather than
re-rendered. `er7-redact` remains the right tool for de-identifying a
message for a test corpus, which is a different job this workspace does not
currently do.

`hl7_mock_receiver_rust` already exists in the Python repository as a first
Rust service. It is adopted here rather than rewritten: its behaviour has
already been checked field-by-field against the Python, and that work is not
worth redoing. It is moved under `crates/hl7-mock-receiver`, re-pointed at
the workspace's shared crates, and its `hl7-v2-mllp` dependency is updated to
the renamed `hl7-2-mllp`.

---

## 6. Dependencies

### The HL7 crates, which are the point

The user's `~/git/hl7-rust` and `~/git/er7-rust` workspaces are the reason
this rewrite is tractable. They are consumed as **path dependencies** so a
fix in either is picked up here immediately, with a `[patch]`-free
`version = "..."` alongside each path so publishing later is a one-line
change.

| Crate | Used for |
| --- | --- |
| `er7` | ER7 parse/render/paths; the encoding layer under everything |
| `er7-redact` | Structure-preserving redaction of patient detail for logs |
| `hl7-2` | The v2 dictionary: releases 2.1–2.9, data types, structures, `validate`, mutation |
| `hl7-2-mllp` | MLLP framing, `Framer`, `Transport`, and `ack` |
| `hl7-2-soap` | SOAP envelope, faults with their HTTP statuses, payload carriage, WSDL, response evaluation |
| `hl7-2-from-er7-into-xml` | ER7 → `urn:hl7-org:v2xml`, for the message store and SOAP egress |
| `hl7-2-from-xml-into-er7` | v2.xml → ER7, for SOAP ingress |
| `hl7-2-from-xsd-into-json-dictionary` | Build-time: the flow XSDs → the JSON dictionaries `hl7-2` validates against |

This mapping is what makes [§8](#8-known-gaps-and-how-they-are-closed)'s XSD
problem solvable: rather than needing a general XML Schema validator in Rust
(there is no mature one), the flow XSDs are compiled *once, offline* into
`hl7-2` dictionaries, and validation becomes `hl7-2`'s own schema-shape
validation — which is stricter about HL7 semantics than `xmlschema` was, and
much faster.

### Everything else

| Area | Crate | Why |
| --- | --- | --- |
| Async runtime | `tokio` | Sockets, timers, signals, graceful shutdown |
| HTTP server | `axum` + `tower-http` | SOAP server, mock receiver, buswatch, dashboard |
| HTTP client | `reqwest` (rustls) | SOAP egress, Azure REST APIs |
| Templating | `askama` | Compile-time-checked templates for buswatch and dashboard |
| Azure Service Bus | `azservicebus` | AMQP 1.0; the choice already proven in `hl7_mock_receiver_rust` |
| Azure credentials | `azure_identity`, `azure_core` (0.25 line) | Managed identity and `DefaultAzureCredential` |
| Azure SQL | `tiberius` + `tokio-util` | Pure-Rust TDS; no ODBC driver in the image |
| Tracing | `tracing`, `tracing-subscriber` | Structured logs |
| Metrics / traces export | `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp` | W3C propagation and OTLP export |
| Serialisation | `serde`, `serde_json` | Message-store events, Cosmos documents, dashboard APIs |
| XML | `quick-xml` (+ `hl7-2-xml-lite-helper` via the HL7 crates) | Anything the HL7 crates do not already read |
| Time | `time` | HL7 timestamps, ISO-8601, UTC |
| UUID | `uuid` v4 | Correlation IDs, replay batch IDs |
| Errors | `thiserror` in libraries, `anyhow` in binaries | Typed errors where callers branch; context where they do not |
| Config | hand-written in `ihb-config` | The Python's blank-is-absent semantics are load-bearing and no derive crate matches them |
| Test doubles | `wiremock`, `tokio-test`, `insta` | HTTP fakes, async tests, snapshot tests for rendered HL7/XML |

Dependency policy, enforced in `spec/17-testing-and-quality.md`:

- Every dependency is declared once in `[workspace.dependencies]` and
  referenced as `dep.workspace = true` by members.
- `#![forbid(unsafe_code)]` in every crate. No exceptions; the Python had no
  FFI and neither do we, and `tiberius`/`rustls` keep the TLS stack in Rust.
- No `openssl`. `rustls` everywhere, so the image is `distroless`-friendly
  and the corporate CA is injected as a PEM bundle rather than an OS trust
  store rebuild.

---

## 7. Cross-cutting engineering standards

These are requirements on every crate, and `tasks.md` does not repeat them
per task.

### Annotations and documentation

- `#![warn(missing_docs)]` and `#![warn(clippy::missing_docs_in_private_items)]`
  — *everything* is documented, public or not. The Python's own docstrings
  carry hard-won detail (why `AutoLockRenewer` is closed before the receiver,
  why `xmlschema`'s error string must never be logged) and that detail must
  survive the port.
- Every module opens with `//!` crate/module docs explaining what it is for
  and which spec section it implements.
- Every public item has rustdoc with: what it does, its arguments, what it
  returns, what it errors on, and a `# Examples` block that compiles.
- Doc examples are real tests. `cargo test --doc` is part of the gate.
- Comments explain *why*, never *what*. Where the Python has a comment
  explaining a workaround, that reasoning is carried across verbatim in
  substance, with a pointer to the Python file it came from.

### Lints

Workspace `Cargo.toml`:

```toml
[workspace.lints.rust]
missing_docs = "warn"
unsafe_code = "forbid"
unreachable_pub = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
nursery  = { level = "warn", priority = -1 }
cargo    = { level = "warn", priority = -1 }
missing_docs_in_private_items = "warn"
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
todo = "deny"
dbg_macro = "deny"
```

`unwrap`/`expect`/`panic` are warnings rather than denials because tests use
them freely; every `#[allow]` in non-test code carries a comment justifying
it. CI runs clippy with `-D warnings`, so an unjustified allow fails the
build.

### Tests

Four layers, all required (see `spec/17-testing-and-quality.md`):

1. **Unit tests** in `#[cfg(test)] mod tests` beside the code, one per
   normative clause.
2. **Doc tests** — the rustdoc examples, executed.
3. **Integration tests** in `tests/`, driving the public API: a real MLLP
   listener against a fake bus, a real HTTP server against `wiremock`.
4. **Golden tests**: the sample messages in
   `Integration-Hub-Beta/local/sample_messages/` and the Python test
   fixtures (`tests/chemo_messages.py`, `tests/pims_messages.py`,
   `tests/wpas_messages.py`) are copied in as fixtures, and the Rust output
   is asserted against the Python's output for the same input, byte for
   byte. This is the primary equivalence evidence.

### Errors

- Libraries return `Result<_, ThisCrateError>` with `thiserror`. Never a
  `Box<dyn Error>` across a crate boundary.
- **No error message ever contains a message payload or a field value.**
  The Python learned this the hard way — `_format_schema_validation_error`
  exists solely because `xmlschema`'s `str(error)` embeds the whole
  instance document, PII and all. Error types in `ihb-validation` carry a
  structural path and a reason, never a value, and there is a test that
  asserts a known PII string does not appear in a rendered error.
- Binaries use `anyhow` for context and exit non-zero with a single
  human-readable line.

---

## 8. Known gaps and how they are closed

| Gap | Resolution |
| --- | --- |
| **No XML Schema validator in Rust.** Python used `xmlschema` against the per-flow XSDs in `hl7_validation/resources/{phw,paris,chemo,pims,mosaiq,wds}/`. | Compile each flow's XSD set into a `hl7-2` JSON dictionary with `hl7-2-from-xsd-into-json-dictionary`, at build time via `xtask`, and validate with `hl7-2`'s schema-shape validation. The generated dictionaries are committed so a build needs no XSD toolchain, and `xtask verify-dictionaries` fails if they drift from the XSDs. |
| **No Azure Monitor OpenTelemetry exporter for Rust.** Python used `azure-monitor-opentelemetry`. | Export via OTLP to the Azure Monitor OTLP endpoint / an OTel collector sidecar. `ihb-telemetry` keeps the exporter behind a trait so an Azure-native exporter can be dropped in later without touching call sites. `APPLICATIONINSIGHTS_CONNECTION_STRING` is still honoured: absent or blank means no-op, exactly as today. |
| **No `pyodbc` / ODBC Driver 18.** | `tiberius` speaks TDS directly. Managed-identity auth is obtained from `azure_identity` and handed to `tiberius` as an AAD token, replacing `Authentication=ActiveDirectoryMsi` in the ODBC string. `fast_executemany` becomes a multi-row `INSERT` with bound parameters, chunked to stay under the TDS 2100-parameter limit. |
| **`hl7apy`'s object model is not reproducible.** The Python mappers mutate `hl7apy` `Message` objects, and some of their behaviour (auto-populated MSH-7, `add_field` repetition semantics, the `NM`/`SI` rewriting that `hl7apy_fidelity.py` patches out) is `hl7apy`-specific. | Mappers are re-expressed against `er7`/`hl7-2` as explicit "read this path, write that path" rules. The rules are extracted from the Python into `spec/09-transformers.md` as tables, and the golden tests are the proof they agree. `hl7apy_fidelity` has no Rust counterpart: `er7` does not rewrite values, so the defect it patches cannot occur. |
| **`azservicebus` feature coverage.** Sessions, topics/subscriptions, auto-lock-renewal and dead-lettering all need to work. | Verified in Phase 0 against the local Service Bus emulator before any service depends on it. If a capability is missing, `ihb-bus` grows it against the AMQP layer rather than each service working around it. Recorded as a risk in [§12](#12-risks). |
| **Flask/FastAPI templates and i18n.** `buswatch` and `dashboard` render Jinja templates; the dashboard uses Babel (`babel.cfg`). | `askama` templates, ported from the Jinja source. i18n is deferred: the extracted catalogue is carried across but the dashboard ships English-only in the first cut, matching what is actually deployed. |

---

## 9. Deliberate deviations from the Python

Each of these is a place the Rust knowingly differs. Every one is
re-confirmed with the Hub team before the affected container is cut over,
and each is specified in the relevant `spec/` section.

1. **Pipelined MLLP frames each get a reply.** The Python read until its
   buffer *ended* with `<FS><CR>`, so two frames arriving in one read were
   treated as one message and answered once. `hl7-2-mllp::Framer` handles
   them properly. (Already true of `hl7_mock_receiver_rust`.)
2. **The last-resort NACK is framed.** The Python's fallback in
   `hl7_mock_receiver/error_handler.py` returns a bare string while every
   other path returns `to_mllp()`, so a peer waiting for `<FS><CR>` hangs.
3. **An oversized frame closes the connection rather than being buffered.**
   Bounded by `MAX_MESSAGE_SIZE_BYTES`.
4. **No `AZURE_MONITOR_OWNER` handshake.** The Python has three libraries
   racing to call `configure_azure_monitor` and an environment variable to
   arbitrate. `ihb-telemetry` initialises once, from one place, and the
   variable is accepted and ignored.
5. **Configuration is validated once at startup, completely.** The Python
   reads some settings lazily and fails minutes later on the first message.
   Every service parses its whole configuration before it binds a socket,
   and exits non-zero with the full list of problems.
6. **`message_store_service` inserts are chunked.** `fast_executemany` had
   no bound; TDS does, so batches are split at the parameter limit. The
   transaction boundary stays per-batch so the abandon-on-failure contract
   is unchanged.
7. **Blank optional environment variables are absent, uniformly.** The
   Python does this in most places and not others (`_read_env` vs
   `_read_int_env`, which would `int("")` and panic). The Rust does it
   everywhere.
8. **v2.xml → ER7 keeps components in their own positions.** The Python's
   converter joins an element's non-empty children positionally, so an empty
   component shifts every component after it — sending MPI a patient
   identifier whose assigning authority sits in the wrong place, on every
   LIMS SOAP message. The Rust reads position from the element name and is
   deliberately **not** bug-compatible here. Evidence and the affected
   fields: [`spec/06-validation.md`](spec/06-validation.md) §5.9. This one
   needs the Hub team's attention regardless of the rewrite, because it is
   happening in production now.

---

## 10. Phasing

Each phase ends with everything in it specified, implemented, tested,
documented and passing the gate. No phase starts before the one it depends
on is done.

### Phase 0 — Foundation

Workspace skeleton, lint configuration, CI gate, `xtask`. Spec files
`index`, `01-architecture`, `02-configuration`, `17-testing-and-quality`,
`18-deployment`. Prove `azservicebus` against the local emulator, and prove
`hl7-2-from-xsd-into-json-dictionary` against the PHW XSD set. **Nothing
else starts until both probes pass.**

### Phase 1 — Shared crates

`ihb-config`, `ihb-health`, `ihb-runtime`, `ihb-telemetry`, `ihb-fields`.
Specs `02`, `04`, `05`. These are small, pure, and heavily tested; they set
the documentation and testing bar for everything after.

### Phase 2 — Message bus and validation

`ihb-bus`, `ihb-validation`, and the committed flow dictionaries. Specs
`03`, `06`. The golden ER7 ↔ XML corpus is built here from the Python's own
test fixtures, and this is where equivalence stops being a claim.

### Phase 3 — Ingress

`hl7-server`, `hl7-soap-server`, and `hl7-mock-receiver` (adopted). Specs
`07`, `08`, `11`. First end-to-end: a message enters over MLLP, lands on a
queue, and is acknowledged.

### Phase 4 — Transformation

`ihb-transformer`, then PHW, ChemoCare and PIMS. Spec `09`. The mapping
tables are the largest single body of business rules in the estate and get
the most golden tests.

### Phase 5 — Egress

`hl7-sender`, `hl7-subscription-sender`, `soap-sender`,
`soap-subscription-sender`, `http-mock-receiver`. Specs `10`, `11`. At the
end of this phase every flow in [§2](#2-what-the-system-does) runs
end-to-end in Compose against mocks.

### Phase 6 — Persistence

`ihb-sql`, `message-store-service`, `message-replay-job`. Specs `12`, `13`.

### Phase 7 — Operations surfaces

`buswatch`, then `dashboard`. Specs `14`, `15`. The dashboard is the largest
single crate and is sequenced last of the pipeline work because nothing
depends on it.

### Phase 8 — PROMs

`xml-fhir-proms-transformer`. Spec `16`. Independent of the HL7 v2 pipeline
and deliberately last.

### Phase 9 — Delivery

Dockerfiles, the Compose stack, the ADO pipeline definitions, and the
cutover runbook. Spec `18`.

---

## 11. Definition of done

A crate is done when all of the following hold.

- Its spec section is written, numbered, and has no `TODO`.
- Every normative clause has at least one test citing it.
- Golden tests pass against the Python's output for the shared fixtures.
- `cargo fmt --check` is clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  is clean with pedantic, nursery and cargo lints on.
- `cargo test --workspace` passes, including doc tests.
- `cargo doc --no-deps` emits no warnings, and every public item has an
  example.
- Its `README.md` explains what it is, how to run it, and what configures
  it.
- For a service crate: the container builds, starts with the flow's real
  `.env`, and passes a health check.

The **workspace** is done when every flow in [§2](#2-what-the-system-does)
runs end-to-end in the local Compose stack against the Rust containers, with
the same sample messages and the same asserted outputs as the Python.

---

## 12. Risks

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| `azservicebus` lacks a needed capability (session receivers, auto-lock-renewal, subscription rules, dead-letter access) | Medium | High — the whole pipeline is Service Bus | Phase 0 probe against the emulator, before anything depends on it. `ihb-bus` owns the gap so the fix lands in one crate. Fallback: drive AMQP 1.0 directly. |
| XSD → `hl7-2` dictionary conversion loses a constraint the Python enforced | Medium | High — a message that should be rejected is accepted | Differential test: run every Python validation fixture, positive and negative, through both and require the same verdict. Failing pairs are triaged before Phase 3. |
| `hl7apy` mapping behaviour has undocumented edge cases | High | Medium — a field lands in the wrong place | The golden corpus from the Python's own test fixtures is the oracle; the mapping tables in `spec/09` are derived from the Python source line by line. |
| Azure Monitor telemetry shape changes and breaks Log Analytics queries or dashboard alarms | Medium | Medium | Event and metric names and dimensions are frozen in `spec/04`, with tests asserting the exact strings. |
| Scope: the dashboard is 6.5k lines of Python with Azure ARM, Log Analytics, Cosmos and SMTP integrations | High | Medium | Sequenced last, behind everything the pipeline needs. It can ship after cutover of the message path, or stay on Python indefinitely, without blocking anything. |
| Deviation in [§9](#9-deliberate-deviations-from-the-python) turns out to matter to a real peer system | Low | High | Each deviation is confirmed with the Hub team before the affected container is cut over, and each is individually revertible. |
