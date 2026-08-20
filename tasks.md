# Tasks

The ordered work list for the Rust rewrite. [`plan.md`](plan.md) says what
and why; [`spec/`](spec/) says exactly how; this file says in what order and
tracks what is done.

## How to read this

- Tasks are `P<phase>.<n>`. A phase does not start until the previous one is
  complete.
- Each task names the spec sections it satisfies. **A task is not done until
  every normative clause in those sections has a test citing it**
  ([`spec/17-testing-and-quality.md`](spec/17-testing-and-quality.md) §7.2).
- The standards in [`plan.md` §7](plan.md#7-cross-cutting-engineering-standards)
  — rustdoc on everything, doc examples that compile, clippy pedantic clean,
  `#![forbid(unsafe_code)]` — apply to every task and are not repeated.
- Status: `[ ]` not started · `[~]` in progress · `[x]` done · `[!]` blocked

---

## Phase 0 — Foundation

Nothing else starts until P0.4 and P0.5 both pass. They are the two
technical bets the whole plan rests on. **Both have now passed.**

- [x] **P0.1** Write `plan.md`.
- [x] **P0.2** Write `spec/index.md` and `spec/01`–`spec/18`.
- [x] **P0.3** Write `tasks.md`.
- [x] **P0.4** **Probe: Service Bus.** Prove `azservicebus` against the local
  emulator for every capability the estate needs — session send and receive,
  topic and subscription, peek-lock with auto lock renewal across a
  processing window longer than the lock duration, batch send with
  auto-split, delivery count on a redelivered message, and reconnection
  after the broker drops the link. Write findings to
  `spec/03-message-bus.md` as an appendix. Run under Podman
  ([`spec/18`](spec/18-deployment.md) §5.0). **Passed: 7 of 7 capabilities
  present; three findings recorded, the load-bearing ones being that
  `azservicebus` ignores `UseDevelopmentEmulator` and has no
  `AutoLockRenewer`.**
  [`spec/03`](spec/03-message-bus.md) §7.3, §8
- [x] **P0.5** **Probe: XSD → dictionary.** Run
  `hl7-2-from-xsd-into-json-dictionary` over the PHW schema group and
  validate a known-good and a known-bad PHW message with `hl7-2` in
  schema-shape mode. Confirm both verdicts match the Python's. *Blocks
  Phase 2.* **Passed: 12 of 12 verdicts agree across all six groups; one
  adjustment found and specified (`ValueFormat` diagnostics excluded).**
  [`spec/06`](spec/06-validation.md) §3.2, §7, §7.6
- [x] **P0.6** Workspace skeleton: root `Cargo.toml` with members,
  `[workspace.dependencies]`, `[workspace.lints]`; `rust-toolchain.toml`;
  `rustfmt.toml`; `deny.toml`; `.gitignore`.
  [`spec/17`](spec/17-testing-and-quality.md) §2, §8
- [x] **P0.7** `xtask` crate with `build-dictionaries` and
  `verify-dictionaries`. [`spec/06`](spec/06-validation.md) §3.2
- [ ] **P0.8** CI workflow running the gate of
  [`spec/17`](spec/17-testing-and-quality.md) §1, plus the emulator-backed
  job of §9.2, plus the MSRV job of §1.4 — `cargo +$(cargo xtask msrv) check
  --workspace --all-targets`, with the toolchain read from the declaration
  rather than pinned a second time in the workflow.
  [`spec/rust-msrv-n-minus-3.md`](spec/rust-msrv-n-minus-3.md)
- [x] **P0.11** MSRV policy: current stable minus three, declared once under
  `[workspace.package]`, inherited by every member, printed by `cargo xtask
  msrv`, and **verified** — the workspace builds clean on 1.95 with
  `--all-targets --all-features`.
  [`spec/rust-msrv-n-minus-3.md`](spec/rust-msrv-n-minus-3.md)
- [x] **P0.9** Workspace `README.md`: what this is, how to build, how to run
  a flow locally, and the pointer to `spec/`.
- [x] **P0.10** Copy `local/` across from the Python: `docker-compose.yml`
  skeleton, `.env` files unchanged, `ServiceBusEmulatorConfig.json`,
  `sample_messages/`. **Podman; the emulator and its SQL Edge backing store
  come up and P0.4's probe runs against them.**
  [`spec/18`](spec/18-deployment.md) §5

---

## Phase 1 — Shared crates

Small, pure, heavily tested. These set the documentation and testing bar for
everything after them.

- [x] **P1.1** `ihb-config`: readers, blank-is-absent, error accumulation,
  secret-safe `Debug`. [`spec/02`](spec/02-configuration.md) §1, §10
- [x] **P1.2** `ihb-health`: TCP listener, scope-based shutdown.
  [`spec/05`](spec/05-health-check.md)
- [x] **P1.3** `ihb-telemetry::redact`: the ER7 redactor, the MSH allow-list,
  the mask-everything fallback. **Byte-identical to the Python for all six
  sample messages; one open finding raised — see
  [`spec/04`](spec/04-observability.md) §2.13.**
  [`spec/04`](spec/04-observability.md) §2
- [x] **P1.4** `ihb-telemetry::events`: `EventLogger`, the six event types,
  the event shape, the `Integration Hub Event` message text.
  [`spec/04`](spec/04-observability.md) §1
- [x] **P1.5** `ihb-telemetry::metrics`: the three instruments, the four
  standard dimensions, the disabled-export log line.
  [`spec/04`](spec/04-observability.md) §3
- [x] **P1.6** `ihb-telemetry::otel`: subscriber, W3C inject/extract, the
  handler span. **Subscriber, filter resolution, propagation helpers and the
  span are done; the two pieces below are deliberately deferred rather than
  faked.** [`spec/04`](spec/04-observability.md) §4, §5, §6
- [ ] **P1.6a** OTLP exporter, and with it the real trace and span ids on
  every record. Deferred because there is nothing truthful to put in those
  fields until a tracer provider exists; see
  [`spec/04`](spec/04-observability.md) §4.5a and §6.3. **Nothing downstream
  may be built against those fields until this lands.**
- [x] **P1.7** `ihb-runtime`: signal handling, cooperative shutdown, and the
  backoff schedule. The receive loop itself lands with `ihb-bus`, which is
  where the thing being received lives.
  [`spec/03`](spec/03-message-bus.md) §5.4, [`spec/01`](spec/01-architecture.md) §1.3
- [x] **P1.8** `ihb-fields`: path read, conditional copy, range copy with
  repetitions, the `CX.4.1` code extractor, pipe wrapping. **Found and
  specified the repetition-escaping trap of
  [`spec/09`](spec/09-transformers.md) §2.2.1 — copying a repeating field
  whole silently collapses it.**
  [`spec/09`](spec/09-transformers.md) §2, [`spec/03`](spec/03-message-bus.md) §4.1

---

## Phase 2 — Message bus and validation

Where equivalence stops being a claim.

- [x] **P2.1** `ihb-bus`: connection, and the emulator-versus-TLS selection
  of §8.1.1 — including the tests that a production-shaped connection string,
  a namespace, and a string that merely mentions `localhost` never select
  plaintext. [`spec/03`](spec/03-message-bus.md) §1, §8.1.1
- [x] **P2.2** `ihb-bus`: retry classification and the message/outcome
  types. [`spec/03`](spec/03-message-bus.md) §2, §3
- [x] **P2.3** `ihb-bus`: **our own lock renewer**, since `azservicebus` has
  none (§8.1.2), with the §5.1.4 shutdown ordering made real rather than
  remembered — `close()` refuses while the renewer still holds the receiver,
  and an emulator test proves it; `session-cannot-be-locked` classified
  retryable (§8.1.3).
  [`spec/03`](spec/03-message-bus.md) §5, §8.1.2, §8.1.3
- [x] **P2.4** `ihb-bus`: metadata encode/decode, and the message-store
  document. [`spec/03`](spec/03-message-bus.md) §4, §6
- [x] **P2.5** `ihb-bus`: the fake bus. **Models dispositions rather than
  pretending to be a broker: an unsettled message is visible, and an
  abandoned one returns to the front with its delivery count raised.**
  [`spec/03`](spec/03-message-bus.md) §7.1
- [x] **P2.6a** `ihb-bus`: the live AMQP senders and receivers over
  `azservicebus`, behind the same shapes as the fake, plus the
  emulator-backed tests of §7.3. **8 of 8 pass against a real broker under
  Podman.** They earned their keep immediately: they caught that a
  sessionless receiver on a session-required queue fails at connect rather
  than returning empty, which corrected `spec/01` §4.1 — only
  `local-inthub-messagestore` and `local-inthub-mpi-mock-receiver` are
  non-session, not just the former.
  [`spec/03`](spec/03-message-bus.md) §2, §5, §7.3
- [x] **P2.6** Copy the six XSD groups into
  `crates/ihb-validation/schemas/`, unmodified.
  [`spec/06`](spec/06-validation.md) §3.2.1
- [x] **P2.7** Generate and commit the six dictionaries; wire
  `verify-dictionaries` into CI. **Done ahead of its phase, because P0.5
  needed the same generation step; the committed output is byte-identical to
  the `dictionary.json` files already in the Python's `resources/`.**
  [`spec/06`](spec/06-validation.md) §3.2.2–§3.2.5
- [x] **P2.8** `ihb-validation`: structure resolution and the trigger
  mapping. [`spec/06`](spec/06-validation.md) §2
- [x] **P2.9** `ihb-validation`: ER7 → v2.xml, including required-field
  padding, repetition gating, and the `EVN`/`PV1` insertion.
  [`spec/06`](spec/06-validation.md) §4
- [x] **P2.10** `ihb-validation`: v2.xml → ER7. **Two upstream defects found
  and worked around (§5.8), and one Python defect found that corrupts every
  LIMS SOAP message (§5.9) — the Rust is deliberately not bug-compatible
  there.** [`spec/06`](spec/06-validation.md) §5
- [x] **P2.11** `ihb-validation`: flow-schema validation, the PII-safe error
  type, standard-release validation, `ValidationResult`.
  [`spec/06`](spec/06-validation.md) §3.3, §6, §8
- [x] **P2.12** **Build the differential corpus.** Extract every fixture
  named in [`spec/17`](spec/17-testing-and-quality.md) §4.4.1, capture the
  Python's verdict and XML for each, and commit both.
  [`spec/06`](spec/06-validation.md) §7
- [x] **P2.13** **Run the differential.** Every corpus message must produce
  the same verdict and canonically equal XML. Triage every disagreement
  before Phase 3. **30 of 30 agree; three disagreements triaged and
  resolved, one of them by deliberately reproducing a Python defect.**
  [`spec/06`](spec/06-validation.md) §7.2–§7.4, §7.5a

- [x] **P2.14** `ihb-validation::xsd`: real XSD validation through two
  independent engines, compared for redundancy. **`xsd-schema` agrees with
  the Python on 24 of 24; `uppsala` found defective on these schemas and
  reported. Not the ingest path yet — §9.6 lists what must be true first.**
  [`spec/06`](spec/06-validation.md) §9
- [x] **P2.15** Fuzz targets for every surface that takes uncontrolled
  input. **Found a panic in the redactor triggerable by a five-byte frame.**
  [`spec/17`](spec/17-testing-and-quality.md) §10
- [x] **P2.16** Criterion benchmarks for the validation routes, conversion
  and redaction. [`spec/17`](spec/17-testing-and-quality.md) §11
- [ ] **P2.17** Report the `uppsala` type-resolution defect upstream, and
  promote the cross-check when it is fixed.
  [`spec/06`](spec/06-validation.md) §9.4.1

---

## Phase 3 — Ingress

- [ ] **P3.1** Adopt `hl7_mock_receiver_rust` as
  `crates/hl7-mock-receiver`: move it, re-point it at the workspace crates,
  rename `hl7-v2-mllp` → `hl7-2-mllp`, keep its tests passing.
  [`spec/11`](spec/11-mock-receivers.md) §2
- [ ] **P3.2** `hl7-server`: configuration, MLLP listener, framing, size
  limit, the partial-message event.
  [`spec/07`](spec/07-hl7-server.md) §2, [`spec/02`](spec/02-configuration.md) §3
- [ ] **P3.3** `hl7-server`: the three validation layers and every error
  string. [`spec/07`](spec/07-hl7-server.md) §3
- [ ] **P3.4** `hl7-server`: ACK construction, every field.
  [`spec/07`](spec/07-hl7-server.md) §4
- [ ] **P3.5** `hl7-server`: the pipeline of §5 with its exact ordering and
  its blocking/non-blocking split; tracking metadata and MPI routing
  properties. [`spec/07`](spec/07-hl7-server.md) §5, §6
- [ ] **P3.6** `hl7-server`: the accepted message-type allow-list and the
  two rejection paths. [`spec/07`](spec/07-hl7-server.md) §7
- [ ] **P3.7** `hl7-server`: graceful shutdown in the specified order.
  [`spec/07`](spec/07-hl7-server.md) §8
- [ ] **P3.8** `hl7-server`: golden tests for every flow's sample message.
  [`spec/07`](spec/07-hl7-server.md) §9.8
- [ ] **P3.9** `hl7-soap-server`: admission checks, envelope parsing, the
  fault table, XML hardening.
  [`spec/08`](spec/08-hl7-soap-server.md) §2, §3
- [ ] **P3.10** `hl7-soap-server`: assigning-authority extraction, responses
  and escaping. [`spec/08`](spec/08-hl7-soap-server.md) §4, §5
- [ ] **P3.11** `hl7-soap-server`: WSDL generation and the absolute-form
  request target. [`spec/08`](spec/08-hl7-soap-server.md) §1.2, §6
- [ ] **P3.12** `hl7-soap-server`: TLS, requiring both files.
  [`spec/08`](spec/08-hl7-soap-server.md) §7
- [ ] **P3.13** End-to-end: a message over MLLP reaches the fake bus and is
  acknowledged; the same over SOAP.

---

## Phase 4 — Transformation

The largest body of business rules in the estate.

- [ ] **P4.1** `ihb-transformer`: the trait, the runtime, property
  forwarding, the audit texts, the failure table.
  [`spec/09`](spec/09-transformers.md) §1
- [ ] **P4.2** `hl7-phw-transformer`: `MSH`, `EVN`, `PID`, the datetime and
  date-of-death transforms, the transformation-detail audit text.
  [`spec/09`](spec/09-transformers.md) §3
- [ ] **P4.3** `hl7-chemo-transformer`: `MSH`, `EVN`, `PID` with the
  health-board table, `PD1`, `NK1`, the non-specific segments.
  [`spec/09`](spec/09-transformers.md) §4
- [ ] **P4.4** `hl7-pims-transformer`: the flat reparse, `MSH` with trigger
  normalisation, `EVN`, `PID` with its guarded repetitions, `PD1`, `PV1`,
  `MRG`, timezone stripping.
  [`spec/09`](spec/09-transformers.md) §5
- [ ] **P4.5** Golden tests for all three, against every Python fixture.
  [`spec/09`](spec/09-transformers.md) §6.6

---

## Phase 5 — Egress

At the end of this phase every flow runs end-to-end in Compose against
mocks.

- [ ] **P5.1** `hl7-sender`: the common pipeline, first-attempt detection,
  batch sizing, throttling.
  [`spec/10`](spec/10-senders.md) §2
- [ ] **P5.2** `hl7-sender`: MLLP delivery, connection reuse, the single
  timeout retry, ACK evaluation.
  [`spec/10`](spec/10-senders.md) §3
- [ ] **P5.3** `hl7-subscription-sender`: the subscription variant.
  [`spec/10`](spec/10-senders.md) §5
- [ ] **P5.4** `soap-sender`: envelope wrapping, escaping, the response
  evaluation table. [`spec/10`](spec/10-senders.md) §4
- [ ] **P5.5** `soap-subscription-sender`.
  [`spec/10`](spec/10-senders.md) §5
- [ ] **P5.6** `http-mock-receiver`: forgiving parsing, version detection,
  payload extraction, ack and fault responses.
  [`spec/11`](spec/11-mock-receivers.md) §3
- [ ] **P5.7** End-to-end in Compose: `phw-to-mpi`, `paris-to-mpi`,
  `chemo-to-mpi` and `pims-to-mpi` each carry a sample message from ingress
  to mock receiver, with the same asserted output as the Python.

---

## Phase 6 — Persistence

- [ ] **P6.1** `ihb-sql`: `tiberius` connection, both auth modes, lazy open,
  discard-on-error, explicit transactions, parameter chunking, no
  credentials in logs. [`spec/12`](spec/12-message-store.md) §5
- [ ] **P6.2** `message-store-service`: the record builder and its error
  cases. [`spec/12`](spec/12-message-store.md) §3
- [ ] **P6.3** `message-store-service`: the batch loop and its
  all-or-nothing disposition.
  [`spec/12`](spec/12-message-store.md) §4
- [ ] **P6.4** `message-replay-job`: fetch, send, mark, and the retry-once
  policy with its failure classification.
  [`spec/13`](spec/13-message-replay.md) §3–§6
- [ ] **P6.5** Integration against a real SQL Server in a Podman container, for both
  crates. [`spec/12`](spec/12-message-store.md) §6.5, [`spec/13`](spec/13-message-replay.md) §8.4

---

## Phase 7 — Operations surfaces

- [ ] **P7.1** `buswatch`: configuration, queue discovery, snapshot cache,
  concurrent refresh. [`spec/14`](spec/14-buswatch.md) §2, §3, §4
- [ ] **P7.2** `buswatch`: detail view, body rendering, the emulator quota
  hint. [`spec/14`](spec/14-buswatch.md) §5, §6
- [ ] **P7.3** `buswatch`: templates and static assets, ported.
  [`spec/14`](spec/14-buswatch.md) §7
- [ ] **P7.4** **Complete `spec/15` §4 and §5.3–§5.5** from the Python's
  alarm rules and Azure queries.
- [ ] **P7.5** `dashboard`: routing, the no-Azure-configuration guarantee,
  the 30-second status cache.
  [`spec/15`](spec/15-dashboard.md) §1.2, §2
- [ ] **P7.6** `dashboard`: the Azure integrations, each degrading to empty.
  [`spec/15`](spec/15-dashboard.md) §4
- [ ] **P7.7** `dashboard`: flow health.
  [`spec/15`](spec/15-dashboard.md) §3
- [ ] **P7.8** `dashboard`: the three alarms, Cosmos persistence, the
  out-of-hours guard, e-mail.
  [`spec/15`](spec/15-dashboard.md) §5
- [ ] **P7.9** `dashboard`: templates ported; message rendering redacted.
  [`spec/15`](spec/15-dashboard.md) §1.3, §7
- [ ] **P7.10** `dashboard`: golden JSON for every `/api/*` route so
  `dashboard.js` needs no change.
  [`spec/15`](spec/15-dashboard.md) §8.5

---

## Phase 8 — PROMs

- [ ] **P8.1** **Complete `spec/16` §6** from the eleven Python mappers.
- [ ] **P8.2** `xml-fhir-proms-transformer`: WPAS parsing, routing, source
  systems. [`spec/16`](spec/16-proms-transformer.md) §2, §5
- [ ] **P8.3** The six bundle shapes with their positional entry order and
  the injectable UUID factory.
  [`spec/16`](spec/16-proms-transformer.md) §3, §4
- [ ] **P8.4** The resource mappings and the reference-data resolver.
  [`spec/16`](spec/16-proms-transformer.md) §6, §7
- [ ] **P8.5** Golden tests against every WPAS fixture.
  [`spec/16`](spec/16-proms-transformer.md) §8.1

---

## Phase 9 — Delivery

- [ ] **P9.1** A `Dockerfile` per binary, two-stage, distroless, UID 5678.
  [`spec/18`](spec/18-deployment.md) §1, §2
- [ ] **P9.2** `local/docker-compose.yml` complete, every profile healthy,
  runnable mixed with Python containers.
  [`spec/18`](spec/18-deployment.md) §5
- [ ] **P9.3** ADO build definitions, one per service.
  [`spec/18`](spec/18-deployment.md) §6
- [ ] **P9.4** Certificate handling and its startup validation.
  [`spec/18`](spec/18-deployment.md) §4
- [ ] **P9.5** The cutover runbook, with the per-container procedure and the
  rollback. [`spec/18`](spec/18-deployment.md) §7
- [ ] **P9.6** Confirm every deviation in
  [`plan.md` §9](plan.md#9-deliberate-deviations-from-the-python) with the
  Hub team, and record the outcome next to each one.

---

## Not in scope

Recorded so the omission is deliberate rather than forgotten.

- `my_tools/integration_hub_tester`, `my_tools/proms_tester` — developer
  tooling; revisit once the pipeline is green.
- `network_test_app` — a connectivity probe with no Python source in the
  repository.
- `shared_libs/field_utils_lib/hl7apy_fidelity.py` — patches a defect in
  `hl7apy` that `er7` does not have.
- `xml_fhir_proms_transformer/mappers/_deprecated/` — resources in no
  current bundle.
- Dashboard internationalisation — English only in the first cut
  ([`spec/15`](spec/15-dashboard.md) §6).
