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
- [x] **P0.8** CI workflow running the gate of
  [`spec/17`](spec/17-testing-and-quality.md) §1, plus the emulator-backed
  job of §9.2, plus the MSRV job of §1.4 — `cargo +$(cargo xtask msrv) check
  --workspace --all-targets`, with the toolchain read from the declaration
  rather than pinned a second time in the workflow.
  [`spec/rust-msrv-n-minus-2.md`](spec/rust-msrv-n-minus-2.md)
  `.github/workflows/gate.yml`, three jobs: `gate`, `msrv`, `emulator`.
  Writing it found two things the local habit had hidden — `dashboard` was
  never added to `xtask`'s member list (so §1.0's drift check had never
  seen it), and `buswatch`'s emulator tests had the **same** parallel
  contention defect §9.2.1 exists for, hidden because they were only ever
  run with `--test-threads=1`. Both fixed; the emulator job deliberately
  does not pass that flag.
  A fourth defect surfaced once the emulator job ran with the SQL password
  loaded: `a_batch_shares_one_stored_at` scoped its assertion by a `LIKE`
  prefix that matched **every previous run's** rows, and — separately —
  matched only one row of a three-row batch, so it asserted its property
  while looking at a single row. Both fixed; recorded as
  [`spec/17`](spec/17-testing-and-quality.md) §9.2.1d. Verified by making
  it fail for the right reason before restoring it.
  **Not verifiable from here:** GitHub Actions cannot be run locally, so
  the workflow's syntax is checked and its logic reviewed but no run has
  happened. The sibling-crate clone in particular assumes the standard
  runner layout.
- [x] **P0.11** MSRV policy: current stable minus three, declared once under
  `[workspace.package]`, inherited by every member, printed by `cargo xtask
  msrv`, and **verified** — the workspace builds clean on 1.95 with
  `--all-targets --all-features`.
  [`spec/rust-msrv-n-minus-2.md`](spec/rust-msrv-n-minus-2.md)
  `.github/workflows/gate.yml`, three jobs: `gate`, `msrv`, `emulator`.
  Writing it found two things the local habit had hidden — `dashboard` was
  never added to `xtask`'s member list (so §1.0's drift check had never
  seen it), and `buswatch`'s emulator tests had the **same** parallel
  contention defect §9.2.1 exists for, hidden because they were only ever
  run with `--test-threads=1`. Both fixed; the emulator job deliberately
  does not pass that flag.
  **Not verifiable from here:** GitHub Actions cannot be run locally, so
  the workflow's syntax is checked and its logic reviewed but no run has
  happened. The sibling-crate clone in particular assumes the standard
  runner layout.
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
- [x] **P1.6a** OTLP exporter, and with it the real trace and span ids on
  every record. `TraceExporter` is §6.3's seam, `Otlp` the implementation,
  and `configure_with` is how §6.2's failure path is tested without a
  collector. `current_ids` returns the recording span's ids and
  `EventLogger::emit` puts them on every record; the all-zero constants now
  mean "no recording span" rather than "not implemented".
  [`spec/04`](spec/04-observability.md) §4.5a, §4.5b, §6.3.1–§6.3.4

  Two traps found while wiring it, both recorded: **setting a global tracer
  provider exports nothing by itself** — `tracing` spans become OTel spans
  only through `tracing-opentelemetry`'s layer, so without it
  `tracing_installed` would have reported success over a dead path (§6.3.2);
  and building the OTLP exporter needs a **Tokio runtime**, which is why
  `configure` must be called from inside one (§6.3.3).
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
- [x] **P2.18** ~~`xsd-schema` 0.1.3 pins `quick-xml` 0.31.0~~ — **closed
  2026-08-22.** `xsd-schema` 0.1.4 depends on `quick-xml` 0.41.0, which
  carries the fixes for RUSTSEC-2026-0194 and -0195. Verified: one
  `quick-xml` version by one path, all 100 `ihb-validation` tests pass on
  the new engine, and `cargo deny check` is clean **with both ignores
  deleted** rather than left in place — an ignore that no longer describes
  anything would hide a regression.
  This clears **one of `spec/06` §9.6's five preconditions**, not §9.6.
  `xsd::validate` is still a cross-check: §9.4's `uppsala` defect is open
  (P2.17), the XSDs are still not files in the image, the differential has
  not been re-run against the XSD route, and item 4's decision — three
  behaviours whose change alters which messages reach MPI — is the Hub
  team's to make.
  [`spec/06`](spec/06-validation.md) §9.7, §9.6.5
- [~] **P2.17** Report the `uppsala` type-resolution defect upstream, and
  promote the cross-check when it is fixed.
  [`spec/06`](spec/06-validation.md) §9.4.1

  **Filed as <https://github.com/kushaldas/uppsala/issues/39>.** The
  reproduction was run against 0.9.0, still the latest published version —
  fifteen lines, no HL7 involved, and it reproduces exactly. Checked the
  seven existing issues first; none covered it. The working copy is
  `docs/upstream/uppsala-id-type-resolution.md`.

  What remains of this task is the second half: **promote the cross-check
  when the defect is fixed.** The pinning test of
  [`spec/06`](spec/06-validation.md) §9.4.1 is the trigger — when it starts
  failing, `uppsala` agrees and can move onto the ingest path.

  Worth noting from the reproduction: `<root>plain</root>` passes while
  `<root>252</root>` fails, because `plain` happens to be a valid `NCName`
  too. That coincidence is how the defect hides — a corpus of well-behaved
  identifiers looks healthy until one starts with a digit.

  While writing it, found that [`spec/06`](spec/06-validation.md) §9.4 said
  the defect had been **"Reported"** while this task said it had not. It had
  not; the spec is corrected.

---

## Phase 3 — Ingress

- [x] **P3.1** Adopt `hl7_mock_receiver_rust` as
  `crates/hl7-mock-receiver`: move it, re-point it at the workspace crates,
  rename `hl7-v2-mllp` → `hl7-2-mllp`, keep its tests passing. **All eight
  of its integration tests pass unchanged in behaviour against the
  workspace bus. Its own `MessageSink`, `azservicebus` client and config
  reader are gone in favour of `ihb-bus`/`ihb-config`, and the `MSH-7`
  timestamp moved to `ihb-fields` so `hl7-server` and the mock share one
  implementation of `spec/07` §4's clause rather than two.**
  [`spec/11`](spec/11-mock-receivers.md) §2
- [x] **P3.2** `hl7-server`: configuration, MLLP listener, framing, size
  limit, the partial-message event.
  [`spec/07`](spec/07-hl7-server.md) §2, [`spec/02`](spec/02-configuration.md) §3
- [x] **P3.3** `hl7-server`: the three validation layers and every error
  string. [`spec/07`](spec/07-hl7-server.md) §3
- [x] **P3.4** `hl7-server`: ACK construction, every field.
  [`spec/07`](spec/07-hl7-server.md) §4
- [x] **P3.5** `hl7-server`: the pipeline of §5 with its exact ordering and
  its blocking/non-blocking split; tracking metadata and MPI routing
  properties. [`spec/07`](spec/07-hl7-server.md) §5, §6
- [x] **P3.6** `hl7-server`: the accepted message-type allow-list and the
  two rejection paths. [`spec/07`](spec/07-hl7-server.md) §7
- [x] **P3.7** `hl7-server`: graceful shutdown in the specified order.
  [`spec/07`](spec/07-hl7-server.md) §8
- [x] **P3.8** `hl7-server`: golden tests for every flow's sample message.
  **All six samples through the whole pipeline. Forwarded bytes are
  asserted byte-for-byte; the `XmlPayload` is compared against the XML the
  Python actually produced, and four of six agree element for element. The
  two that do not are real content differences, now recorded as
  [`spec/06`](spec/06-validation.md) §8.4.1 and §8.4.2, and the test
  asserts the difference set is exactly those two.**
  [`spec/07`](spec/07-hl7-server.md) §9.8
- [x] **P3.9** `hl7-soap-server`: admission checks, envelope parsing, the
  fault table, XML hardening. **Served through `hyper` rather than a
  hand-written HTTP parser; the billion-laughs and XXE tests of §8.7 both
  pass. One narrow deviation on an unparseable `Content-Length`, recorded
  at `spec/08` §2.2.**
  [`spec/08`](spec/08-hl7-soap-server.md) §2, §3
- [x] **P3.10** `hl7-soap-server`: assigning-authority extraction, responses
  and escaping. **The payload is sliced out of the request rather than
  re-rendered from the parsed tree, so `XmlPayload` is the bytes the sender
  sent.** [`spec/08`](spec/08-hl7-soap-server.md) §4, §5
- [x] **P3.11** `hl7-soap-server`: WSDL generation and the absolute-form
  request target. **Both target forms tested, and `X-Forwarded-Proto`
  honoured — without it the WSDL advertises an address nothing can reach
  from behind Container Apps ingress.**
  [`spec/08`](spec/08-hl7-soap-server.md) §1.2, §6
- [x] **P3.12** `hl7-soap-server`: TLS, requiring both files. **`Tls` is a
  pair, so "cert without key" cannot be represented after configuration is
  read; the Python ran plaintext in that case.**
  [`spec/08`](spec/08-hl7-soap-server.md) §7
- [x] **P3.13** End-to-end: a message over MLLP reaches the fake bus and is
  acknowledged; the same over SOAP. **Both against the real sample messages
  over real sockets: `phw-to-mpi.sample.hl7` through the MLLP listener, and
  `lims-to-mpi.sample.xml` through the HTTP endpoint.**

---

## Phase 4 — Transformation

The largest body of business rules in the estate.

- [x] **P4.1** `ihb-transformer`: the trait, the runtime, property
  forwarding, the audit texts, the failure table.
  [`spec/09`](spec/09-transformers.md) §1
- [x] **P4.2** `hl7-phw-transformer`: `MSH`, `EVN`, `PID`, the datetime and
  date-of-death transforms, the transformation-detail audit text.
  [`spec/09`](spec/09-transformers.md) §3
- [x] **P4.3** `hl7-chemo-transformer`: `MSH`, `EVN`, `PID` with the
  health-board table, `PD1`, `NK1`, the non-specific segments.
  [`spec/09`](spec/09-transformers.md) §4
- [x] **P4.4** `hl7-pims-transformer`: the flat reparse, `MSH` with trigger
  normalisation, `EVN`, `PID` with its guarded repetitions, `PD1`, `PV1`,
  `MRG`, timezone stripping.
  [`spec/09`](spec/09-transformers.md) §5
- [x] **P4.5** Golden tests for all three, against every Python fixture.
  **16 of 16 messages transform byte-for-byte identically to the Python,
  audit texts included. The corpora were captured by running each Python
  transformer in its own virtualenv over its fixtures and its flow's sample
  message; they are committed, and CI needs no Python.** Getting there
  settled `spec/09` §2.2.2 and found four defects in `ihb-fields`, all now
  primitives with their own tests: §2.2.2a structural field copying,
  §2.2.2b empty trailing repetitions, §2.2.2c trailing separator trimming,
  §2.2.2d the explicit null.
  [`spec/09`](spec/09-transformers.md) §6.6

---

## Phase 5 — Egress

At the end of this phase every flow runs end-to-end in Compose against
mocks.

- [x] **P5.1** `ihb-sender`: the common pipeline, first-attempt detection,
  batch sizing, throttling. **The throttle's §2.4.2 rule — an idle sender
  resets its target to *now* rather than bursting to catch up — is tested
  on a virtual clock.** First-attempt detection turned out to be
  `Incoming::is_first_delivery`, already specified and tested in
  `spec/03` §5.3, so it is called rather than restated.
  [`spec/10`](spec/10-senders.md) §2
- [x] **P5.2** `hl7-sender`: MLLP delivery, connection reuse, the single
  timeout retry, ACK evaluation. **8 integration tests against a real MLLP
  listener: `AA`, `AE`, `AR`, a non-ACK reply, silence, a hangup, five
  messages over one connection, and a receiver that is not there.**
  [`spec/10`](spec/10-senders.md) §3
- [x] **P5.3** `hl7-subscription-sender`: the subscription variant. **A
  second `[[bin]]` differing in one argument, because §5.1 says the
  variants differ only in where they read from — the pipeline, throttle,
  batch sizing and connection handling are the same code.**
  [`spec/10`](spec/10-senders.md) §5
- [x] **P5.4** `soap-sender`: envelope wrapping, escaping, the response
  evaluation table. **Every row of §4.4, and §6.5's "crucially": `"` and
  `'` are *not* escaped, which matters because HL7 payloads carry
  apostrophes constantly.**
  [`spec/10`](spec/10-senders.md) §4
- [x] **P5.5** `soap-subscription-sender`.
  [`spec/10`](spec/10-senders.md) §5
- [x] **P5.6** `http-mock-receiver`: forgiving parsing, version detection,
  payload extraction, ack and fault responses. **Plus the test that
  matters most for either crate: `soap-sender` against it end to end. The
  two halves were written from opposite sides of one specification — §4.4's
  response table and §3.6's response shape — and both could have been
  individually correct and not interoperated.**
  [`spec/11`](spec/11-mock-receivers.md) §3
- [x] **P5.7** End-to-end: `phw-to-mpi` carries its sample message from a
  socket, through **real Service Bus queues**, to the mock receiver —
  `crates/ihb-e2e`, behind the `emulator` feature. One correlation id
  across all three hops, and the transformer's `MSH-7` normalisation
  visible at the far end. It found two things nothing else could: a
  `tokio::time::timeout` around a receive cancels it rather than observing
  it (now `spec/03` §5.5), and the `large_futures` allow's caveat is real
  (a dozen 32 KB futures in one frame overflows a test stack).

  **In Compose:** `crates/ihb-e2e/tests/in_compose.rs`, behind the
  `compose` feature. Five MLLP flows — phw, pims, chemo (with a
  transformer) and paris, mosaiq (straight to the sender) — each brought up
  as its own profile, sent a sample, and traced to the mock receiver's
  egress queue. `lims-to-mpi` is deliberately excluded: its ingress is SOAP
  over HTTP, and a test claiming five flows while covering four would be
  worse than one that says so.

  It found what only a container test could: **`spec/03` §5.1.5**, a lost
  session leaving the receive loop retrying a dead receiver forever while
  the health check stayed green.

  Two constraints it also pinned down: the emulator's connection quota
  (`spec/14` §6.1) makes "all profiles at once" impossible — hence one
  profile at a time, which also proves each profile is self-sufficient; and
  a cold three-hop flow takes about **four minutes**, because each hop
  waits out §5.1.2's sixty-second receive window.

  `lims-to-mpi` is covered too, by its own test. It differs twice over:
  SOAP over HTTP rather than MLLP, and it **ends at a queue**:
  [`spec/01`](spec/01-architecture.md) §2.1 marks its transformer
  *(future)*, so the mock receiver is not the assertion and asserting it
  would be asserting something the architecture does not claim.

  Building it found two defects in the harness itself, both recorded as
  [`spec/18`](spec/18-deployment.md) §5.2.1–§5.2.2: `compose down` **only
  stops the profiles it is given** and reports success while leaving the
  rest running, which had quietly defeated the one-profile-at-a-time rule;
  and "nothing is `(starting)`" is not readiness, because it is true before
  the application containers exist at all.

---

## Phase 6 — Persistence

- [x] **P6.1** `ihb-sql`: `tiberius` connection, lazy open,
  discard-on-error, explicit transactions, parameter chunking, no
  credentials in logs. **Password or managed identity (P6.6).**
  [`spec/12`](spec/12-message-store.md) §5
- [x] **P6.2** `message-store-service`: the record builder and its error
  cases. **Including the §0.8 test: a known patient marker driven through
  every failure path, asserting it does not surface — and that §3.5's one
  permitted exception quotes only the timestamp.**
  [`spec/12`](spec/12-message-store.md) §3
- [x] **P6.3** `message-store-service`: the batch loop and its
  all-or-nothing disposition. **§4.2.1 has its own test: the failure event
  carries `Batch of {n} message(s)`, never the payloads — this is the one
  service that holds a hundred full patient records at a time.**
  [`spec/12`](spec/12-message-store.md) §4
- [x] **P6.4** `message-replay-job`: fetch, send, mark, and the retry-once
  policy with its failure classification. **Including §5.3's duplicate
  warning, which is the one an operator most needs: a timed-out send may
  already have reached the broker, so the retry can double-deliver a
  patient record.**
  [`spec/13`](spec/13-message-replay.md) §3–§6
- [x] **P6.5** Integration against a real SQL Server in a Podman container, for both
  crates. **18 tests against Azure SQL Edge — the commit, the rollback
  driven by a real server refusal, the transparent reconnect, every column
  of §2.1 read back, the replay fetch ordering, and §2.2's recovery: a
  failed replay re-run picks its own rows up again.** The container is
  **Edge, not SQL Server**: Microsoft ships no arm64 `mssql/server` image
  and it segfaults under emulation on Apple Silicon, now recorded at
  [`spec/18`](spec/18-deployment.md) §5.3. The schema lives in
  `local/sql/schema.sql` and is applied by the tests, because the Edge
  image ships no `sqlcmd` and a setup step a developer must remember is one
  that is sometimes skipped.
  [`spec/12`](spec/12-message-store.md) §6.5, [`spec/13`](spec/13-message-replay.md) §8.4
- [x] **P6.6** **Azure SQL: TLS and managed identity.**
  1. [x] **TLS.** `tiberius` 0.13.0 moved to `rustls` 0.23 and
     `rustls-webpki` 0.103.17, past RUSTSEC-2026-0098, -0099 and -0104. Both
     `ihb-sql` and `db-sender` honour `Encrypt=Yes` (`Required`, with the
     compiled-in Mozilla roots, since the image is distroless) and
     `TrustServerCertificate`. Proved against SQL Edge over TLS, plus a test
     that an untrusted certificate is refused. `spec/12` §5.8.
  2. [x] **Managed identity.** New crate `ihb-identity` fetches and caches an
     AAD token from the loopback identity endpoint (`azure-token-lite` for the
     request and parsing); `ihb-sql` and `db-sender` present it as
     `AuthMethod::aad_token`. `spec/12` §5.4.1–§5.4.4. **Unit-tested against a
     fake endpoint only: SQL Edge cannot validate an Entra token, so it is
     unproven against Azure SQL until a non-production deployment.**
  3. [x] **Fixed, not run in a Container App:** the dashboard's `HttpsClient` refuses plaintext
     URLs (`spec/15` §4.7), but a Container App's `IDENTITY_ENDPOINT` is
     `http://localhost:<port>/msi/token`, so its ambient-credential path
     probably cannot reach it. Not run against a real Container App; check
     before relying on it. The ambient path now goes through `ihb-identity`
     (`spec/15` §4.6.2a).

---

## Phase 7 — Operations surfaces

- [x] **P7.1** `buswatch`: configuration, queue discovery, snapshot cache,
  concurrent refresh. [`spec/14`](spec/14-buswatch.md) §2, §3, §4
- [x] **P7.2** `buswatch`: detail view, body rendering, the emulator quota
  hint. [`spec/14`](spec/14-buswatch.md) §5, §6
- [x] **P7.3** `buswatch`: templates and static assets, ported.
  [`spec/14`](spec/14-buswatch.md) §7
- [x] **P7.3a** `ihb-bus` gained what BusWatch needed and no service had:
  `Peeked` and `LiveReceiver::peek` (`spec/03` §5.4a),
  `LiveBus::next_session_receiver` (`spec/14` §4.4),
  `LiveBus::close` (`spec/14` §7.3), and the short-timeout variants
  `connect_within` / `receive_within` (`spec/03` §5.1.2a–b). The last pair
  matter: `MAX_WAIT` is a minute, an empty queue is BusWatch's normal case,
  and cancelling a receive instead **loses messages** (`spec/03` §5.5).
- [x] **P7.3b** Fixed a pre-existing flake in the `ihb-bus` emulator tests:
  they shared one session id and one non-session queue, so `cargo test`'s
  parallelism failed a *different* test on each run. Now isolated by
  construction, and twice as fast. Recorded as
  [`spec/17`](spec/17-testing-and-quality.md) §9.2.1–§9.2.2.
- [x] **P7.4** **Complete `spec/15` §4 and §5.3–§5.5** from the Python's
  alarm rules and Azure queries. Written: §4.3.1's nine query texts,
  §4.4's sanitiser, §5.1.1's Europe/London periods, §5.1.2's six-step rule
  lifecycle, and one section per alarm. Two findings raised as
  [`spec/15`](spec/15-dashboard.md) §9 — the out-of-hours guard is
  **configured and never enforced**, and two queries ignore
  `AZURE_APP_INSIGHTS_RESOURCE_ID` while every other one honours it.
- [x] **P7.5** `dashboard`: routing, the no-Azure-configuration guarantee,
  the 30-second status cache.
  [`spec/15`](spec/15-dashboard.md) §1.2, §2
  - [x] `config` — §1.2's guarantee, §5.6's three Cosmos modes, and the one
    refusal that matters (`COSMOS_DISABLE_SSL_VERIFY` against a real host).
  - [x] `period` — §5.1.1's Europe/London windows, BST included. `time-tz`
    carries the IANA snapshot; a fixed offset would be an hour wrong for
    seven months of the year.
  - [x] `kql` — §4.3.1's nine queries and §4.4's sanitiser. There is no
    parameter binding in the Log Analytics client, so that filter is the
    whole injection defence.
  - [x] `flows` — §3.1's eight flows, keyed on the `WORKFLOW_ID` the
    services actually emit.
  - [x] `health` — §3.2's table, with Unknown structurally distinct from
    zero.
  - [x] `cache` — §2.2.1's shared 30-second cache; concurrent requests make
    one call to Azure, not eight.
  - [x] `alarm` — §5.1.2's six-step lifecycle, written once instead of the
    Python's three copies, and §5.2.1's out-of-hours guard.
  - [x] `observe` — §4.2's seam. `Unconfigured` is the **default**
    implementation, so §1.2's guarantee is structural rather than a branch
    in every handler.
  - [x] `status` — §2.2's payload. A KPI total spanning an unreadable queue
    is absent, not a partial sum: §2.2.3, raised as §9.8.
  - [x] `web` — every page and `/api/*` route of §2, with §2.2.1's cache.
  - [x] Templates and the stylesheet, on the DHCW brand. `unknown` gets a
    colour of its own rather than a lighter `healthy`.
  - [x] `main` — verified running: `/healthz` answers, `/api/status`
    returns `null` totals rather than zeroes, and the overview says "Azure
    is not configured" with every flow Unknown.
- [x] **P7.6** `dashboard`: the Azure integrations, each degrading to empty.
  [`spec/15`](spec/15-dashboard.md) §4
  - [x] `azure::http` — HTTPS on the `hyper` + `tokio-rustls` stack already
    here, so no second TLS stack. Plaintext is **refused**, not downgraded:
    these requests carry a subscription-wide bearer token.
  - [x] `azure::token` — §4.6.1's two scopes, §4.6.2's Container Apps
    identity endpoint (IMDS deliberately absent — it hangs off-Azure),
    §4.6.3's cache with an expiry margin, and form-encoding, without which
    a secret containing `+` reads as `invalid_client`.
  - [x] `azure::arm` — Service Bus depths and Container Apps state. The API
    versions were the SDK's problem in the Python and are **ours** now, so
    they are pinned in [`spec/15`](spec/15-dashboard.md) §4.6 with a test.
  - [x] `azure::logs` — the `columns`/`rows` pairing, where getting it
    wrong puts a timestamp in a workflow id with no error anywhere.
  - [x] `azure::live` — the one place a failure becomes an empty answer
    **and a log line** (§4.2). Verified: with no configuration it serves
    every route, reaches no network, and reports unknown rather than zero.
- [x] **P7.7** `dashboard`: flow health.
  [`spec/15`](spec/15-dashboard.md) §3
  Delivered with P7.5: §3.1's eight flows keyed on the real `WORKFLOW_ID`,
  §3.2's table at each exact boundary, and §3.3's Unknown made structurally
  distinct from zero rather than a default.
- [x] **P7.8** `dashboard`: the three alarms, Cosmos persistence, the
  out-of-hours guard, e-mail.
  [`spec/15`](spec/15-dashboard.md) §5
  - [x] `alarm::store` — §5.6's seam. A store that is **down** and a store
    with **no rules** return different things, so a Cosmos outage cannot
    silently disable every alarm and report the estate healthy.
  - [x] `alarm::rules` — §5.3–§5.5's three trip conditions. §5.3.2's
    strictly-greater and §5.5.2's at-or-above genuinely differ, both come
    from the Python, and a test pins them against each other.
  - [x] `azure::cosmos` — §5.7's request signing, written as pure functions
    with known-answer tests because all three of its easy mistakes produce
    an indistinguishable `401`. Pinned as [`spec/15`](spec/15-dashboard.md)
    §5.7.1–§5.7.5.
  - [x] `/alarms` and `/api/alarms/status`, with a store outage **named**
    on the page rather than looking like no rules.
  - [x] `alarm::cosmos_store` — the transport, reading and writing the
    documents the Python already wrote (`config` and `state`, keyed `/pk`).
    Carries §5.6.2's in-memory layer so a **rejected write still suppresses
    the next escalation**; without it, Cosmos RU throttling turns a firing
    alarm into two e-mails a minute (§9.9). Both behaviours are tested,
    including the Python's, written out so the difference is visible.
  - [x] §4.3.1's alarm queries wired to the rules. One query per alarm for
    every workflow it has a rule for, not one per rule — the `in (...)`
    list exists for that and a workspace charges per query.
  - [x] §5.5.2's zero-versus-unknown, which the seam had to carry:
    `summarize count()` emits **no row** for a workflow with no failures,
    so a query that ran and found nothing means healthy, while a query that
    did not run means unknown. Collapsing them turns either every quiet
    flow unreadable or every broken query into a clean bill of health.
  - [x] `azure::email` — ACS escalation (§5.8), pinned as
    [`spec/15`](spec/15-dashboard.md) §5.8.1–§5.8.6. A send failure is a
    log line, never something that can stop an evaluation cycle.
- [x] **P7.8a** §8.5's goldens, captured from the Python dashboard run with
  no Azure configuration, in `crates/dashboard/tests/golden/`. Diffing them
  found **`spec/15` §2.3.1a**: `flows[]` carried a flat `active`/`dlq`
  where `dashboard.js` reads a `pre_queue`/`post_queue` pair. Its `|| {}`
  fallback meant nothing raised — every flow card rendered **zero**, on the
  page an operator reads first. Also recorded §8.5.1, where §8.5 and §2.3
  contradict each other: the goldens are a source of candidate keys, not an
  equality assertion, because §2.3's "what the script reads" wins.
- [x] **P7.9** `dashboard`: templates ported; message rendering redacted.
  [`spec/15`](spec/15-dashboard.md) §1.3, §7
  - [x] §1.3's redaction, which the Python does not do at all. The row type
    the templates see carries **no** unredacted field, so a template cannot
    show patient data by forgetting; `/api/messages` does not serialise the
    payload at all. Both pinned at the page level, not just in a unit test.
  - [x] `/messages` and `/trace/{operation_id}` (§2.1).
  - [x] i18n (§6): `/set-language` — note the name, **not** `/language`
    as an earlier draft of §2.1 said. English only, with the preference
    stored so completing a catalogue is the only remaining work.
  - [x] §6.4's redirect safety, specified rather than left as a comment in
    one implementation: the `Referer` is client-supplied, so the target is
    rebuilt from its path and query alone and `\` is normalised first —
    some browsers resolve `/\evil.example` as a network-path reference.
- [x] **P7.10** `dashboard`: golden JSON for every `/api/*` route so
  `dashboard.js` needs no change.
  [`spec/15`](spec/15-dashboard.md) §8.5
  Taken from the **scripts** rather than a captured response: the contract
  is the set of keys the browser dereferences, and a capture would need a
  live subscription and would record keys nothing reads. Recorded as
  [`spec/15`](spec/15-dashboard.md) §2.3, with the reading file and
  function named for each key.
  It found real defects: `/api/status` was missing `queues`,
  `retry_delay_kpis` and the three `alarmN_summary` keys, and **both chart
  routes had the wrong shape entirely** — `sb-chart.js` and
  `hl7-throughput-chart.js` read `data.incoming`/`data.outgoing` with
  `{time, value}` points, where this returned a flat `points` array. That
  draws an empty chart with nothing in the console, because
  `(undefined || []).reduce(...)` is `0`.

---

## Phase 8 — PROMs

- [x] **P8.1** **Complete `spec/16` §6** from the eleven Python mappers.
  Written: §6.0's alias-ordered field lookup, §6.1's date handling, and one
  subsection per resource. Goldens captured first, from all eleven fixtures
  with a deterministic UUID factory, into
  `crates/xml-fhir-proms-transformer/tests/corpus/` — the same approach
  that made the HL7 transformers exact.
  Six findings raised as [`spec/16`](spec/16-proms-transformer.md) §9,
  including that bundle entries are **omitted** when their source data is
  absent while §3 called the order positional (§9.1), and that
  `Appointment.end` is set equal to `start` because WPAS supplies no
  duration (§9.5).
- [x] **P8.2** `xml-fhir-proms-transformer`: WPAS parsing, routing, source
  systems. [`spec/16`](spec/16-proms-transformer.md) §2, §5
  - [x] `parse` — §2.5's leaf indexing, on `roxmltree` (already in the tree
    via `xsd-schema`, and read-only by construction). 23 tests, including
    all eleven real fixtures.
  - [x] Three corrections to §2 that the corpus and the source disproved:
    the root is **not** always `PromsEventRequest`
    (`EXPLICIT_EVENT_CODE_MESSAGE` uses `WpasMessage`); routing **does**
    consult the root tag as a fallback; and an element does **not** take
    precedence over an attribute — document order decides.
  - [x] §2.5.5a and §9.7: an attribute on a text-free element is
    unreachable, because the no-text skip happens first. Reproduced, and
    flagged because `resolve_message_type` names a "future attribute-based
    format" this parser could not read.
  - [x] `message_type` — §2.4.1's routing table, written into the spec
    first because the Python's nine types for six codes had to be reduced
    to the six that can actually be selected.
  - [x] `source_system` — §5's eight health boards, with §5.2.2's
    fabricated placeholder endpoint reproduced and its inertness asserted.
- [x] **P8.3** The six bundle shapes with their positional entry order and
  the injectable UUID factory.
  [`spec/16`](spec/16-proms-transformer.md) §3, §4
  - [x] `identity` — §4.4's injectable factory, with a sequential one that
    still parses as a version-4 UUID so a consumer validating the form
    accepts a golden.
  - [x] `bundle` — modelled as **slots** rather than entries. Every slot
    gets a UUID up front, in §3's order, and an absent resource is simply
    not emitted (§3.2). That is measurable in the Python: a minimal
    referral's `Location` carries the **seventh** UUID at the **fifth**
    entry, and allocating per entry instead would renumber it.
  - [x] Validated against all ten routable goldens — entry order **and**
    every `fullUrl` — before any mapper exists, so the mappers only have
    to fill slots.
- [x] **P8.4** The resource mappings and the reference-data resolver.
  [`spec/16`](spec/16-proms-transformer.md) §6, §7
  - [x] `reference_data` — §7's **two** lookup kinds, which behave
    differently and had been conflated in §7.2: the resolver seam returns
    `None` and omits the element; the two wiki-derived lookups return the
    **input unchanged**, so an unknown code becomes its own display text.
    Recorded as §7.1.1/§7.1.2 and raised as §9.10.
  - [x] `mapping` — §6.1's date formats and §6.1.3's two opposite name
    conventions, verified against every date and name in the goldens.
  - [x] `fhir` — all 52 constants, checked name-by-name against
    `fhir_constants.py`. Two of four written from memory were **wrong**,
    which is why the check is mechanical. Raised §9.11 (two identifier
    systems name a `tools.labs` host) and §9.12.
  - [x] `resources` — the nine mappers, and `transform` assembling them.
  - [x] §9.13: `map_location` has **no guard** where the two mappers
    beside it do, so an empty `Location` is emitted for every bundle. Only
    the golden comparison showed it.
- [x] **P8.5** Golden tests against every WPAS fixture.
  [`spec/16`](spec/16-proms-transformer.md) §8.1
  All ten routable fixtures match the Python's bundle **exactly**, plus the
  unroutable one failing the same way. Four invariants asserted across the
  corpus besides: no dangling reference, no reused `fullUrl`, no `null`
  anywhere, and the error naming what would have worked.

---

## Phase 9 — Delivery

- [x] **P9.1** A `Dockerfile` per binary, two-stage, distroless, UID 5678.
  [`spec/18`](spec/18-deployment.md) §1, §2
  `docker/Dockerfile` parameterised by `--build-arg BIN`, plus
  `docker/build-all.sh` reading the binary names from the manifests so a
  new service cannot be forgotten. **All sixteen build**, 36–42 MB each.
  Verified against a running container rather than inspected: UID
  `5678:5678` and `HEALTH_CHECK_HOST` on all sixteen, and for `hl7-server`
  the MLLP port bound, the §3.1 health probe reachable, a clean SIGTERM
  stop, and exit **1** on a bad configuration before anything binds.
  Three things the spec's §2.1 example did not cover, all found by
  building rather than reading — recorded as §2.6 to §2.9.
- [x] **P9.2** `local/docker-compose.yml` complete, every profile healthy,
  runnable mixed with Python containers.
  [`spec/18`](spec/18-deployment.md) §5
  - [x] 21 services across 9 profiles, mirroring the Python's. All 20
    `.env` files verified byte-identical (§5.2). `depends_on: sqlserver`
    remapped to `sqledge`, which is the only one that runs on arm64
    (§5.3.2).
  - [x] A `health-probe` binary shipped in every image: Compose runs its
    healthcheck **inside** the container and the Python's is `python -c
    "…socket…"`, which a distroless image cannot run. §5.4 needs it.
  - [x] `phw-to-mpi` verified: all six containers **healthy**, an MLLP
    message accepted with `MSA|AA`, and the chain traced through ingress →
    transformer → sender → mock receiver. Redaction visibly active in the
    container logs (`PID|||***`).
  - [x] **Every flow profile healthy**: `phw-to-mpi`, `paris-to-mpi`,
    `chemo-to-mpi`, `pims-to-mpi`, `mosaiq-to-mpi`, `lims-to-mpi`,
    `mpi-to-topic`, plus `dashboard` (with `/healthz` answering 200) and
    `replay` (a job, exiting 0). Convergence takes ~45s: `depends_on`
    waits for *started*, not *ready*, and `restart: on-failure` closes the
    gap (§5.4a).
  - [x] §5.3.6: **nothing created the `IntegrationHub` database.** The
    tests applied the schema to `master` while the `.env` files name
    `IntegrationHub`, so both persistence services reported healthy and
    failed every batch. Schema now creates it; the three SQL test files
    use it; `sqledge` aliased to `sqlserver` (§5.3.7) so the env files
    stay unchanged.
  - [x] §5.5's **mixed run**, verified: a message crossed Rust ingress →
    **Python** transformer → Rust sender → Rust mock receiver, with only
    the `image:` line differing from the all-Rust run.
    `local/docker-compose.mixed.yml` is the overlay.
- [x] **P9.2a** **`spec/03` §5.1.3's lock renewal was never wired up.**
  `ihb-transformer` and `ihb-sender` both opened session receivers and
  neither called `renew_session_lock`, so after one lock expiry every
  receive failed with `IllegalSessionState` **forever**, while the health
  check stayed green. That is every transformer and every sender in the
  estate. `ihb-bus` had the capability and its emulator tests exercised
  it; the main loops just never used it.
  Found by running the stack — the fake bus of §7.1 cannot expire a lock
  and the e2e test finishes in seconds. Fixed in both loops with §5.1.4's
  stop-before-close ordering, verified against the live emulator, and
  recorded as [`spec/03`](spec/03-message-bus.md) §5.1.3a.
- [x] **P9.3** ADO build definitions, one per service. 16 in
  `pipeline-ado/`, plus `build-apps.yml`, `pr-validation.yml` and two
  templates. 11 inherit the Python's exact ACR image name so releases are
  unchanged (spec/18 §6.4); 5 have no Python counterpart (§6.5).
  `cargo xtask verify-pipelines` holds them to the workspace (§6.6), and
  both gates run it.
- [x] **P9.3a** **`gate.yml` still cloned the sibling crate repositories.**
  P9.1 moved the eight first-party dependencies to crates.io, which made
  those three steps dead — and worse than dead: the `test -d` guard fails
  the build if the layout differs, for crates nothing depends on any more.
  Removed from all three jobs.
  [`spec/18`](spec/18-deployment.md) §6
- [x] **P9.4** Certificate handling and its startup validation. New crate
  `ihb-tls`: trust roots as the compiled-in Mozilla set **plus** an
  optional mounted bundle, and `startup_check` wired into all ten service
  mains. 10 unit tests, 6 scenarios verified in a real container.
- [x] **P9.4a** **The distroless base presets `SSL_CERT_FILE`**, which made
  §4.3 unfireable — a forgotten CA mount would have started cleanly and
  failed at handshake time. `docker/Dockerfile` now clears it (spec/18
  §4.7). Found by running the image; unreadable from the source.
  [`spec/18`](spec/18-deployment.md) §4
- [x] **P9.5** The cutover runbook — `docs/cutover-runbook.md`, with the
  per-container procedure, real `az` commands, the dashboard's own KQL for
  verification, and rollback. Two gaps between §7 and the tooling found and
  recorded as spec/18 §7.5–§7.7: the release pipeline cannot deploy
  "alongside taking no traffic", and half the containers have no ingress to
  weight at all.
- [~] **P9.6** Confirm every deviation in
  [`plan.md` §9](plan.md#9-deliberate-deviations-from-the-python) with the
  Hub team, and record the outcome next to each one.

  **Prepared, not confirmed — and it cannot be confirmed here.** Signing
  off is the Hub team's, not the author's, so this stays open until they
  do it. `docs/deviation-sign-off.md` is the pack that makes it possible:
  all thirteen deviations, each with what the Python does (every file and
  line checked against the source), what the Rust does instead, which
  containers it touches, what is actually being decided, and an empty
  Decision row.

  **Deviation 8 needs the Hub team regardless of the rewrite.** The
  Python's XML → ER7 converter misfiles HL7 components, in production, on
  the LIMS SOAP flow now. It must not ride along as a cutover detail.

---

## Phase 10 — Local CI

Specified in [`spec/21`](spec/21-local-ci.md); the evidence and the options
weighed are in [`docs/local-ci-assessment.md`](docs/local-ci-assessment.md).
Not started. Independent of the other phases.

- [x] **P10.1** **Repair the hosted `emulator-backed` job.** Done, proved by
  hosted run `43984183339` (commit `c8a956b`+`deny.toml`, all three jobs
  green on the re-run). The causes, in the order found: Podman socket (fixed
  earlier); a second consumer, `message-store-service`, competing for the
  queues (Compose now starts only `sb-emulator sqledge`); teardown failing
  with "rootless netns: kill network process: permission denied" after every
  test passed (teardown is now best-effort). **Known flake:** the first
  attempt of that run failed three `buswatch` emulator tests on
  `Connect ... Timeout elapsed` and passed on re-run with no change; if it
  recurs, give the test connection a retry. Also fixed on the way: a new
  advisory (`RUSTSEC-2026-0441`, `pin-utils`) failing `cargo deny`.
  [`spec/21`](spec/21-local-ci.md) §8.1, §8.3
- [x] **P10.2** **`cargo xtask gate`**, tiered (`fast`, `gate`, `stack`,
  `flows`, plus `msrv`), `--keep-going`; tier 1 needs no Podman; the MSRV
  build runs only when a toolchain input changed since the merge base, and a
  skip is printed, never silent. `gate.yml`, `AGENTS.md` and `spec/17` §1
  call or point at it; `the_workflow_only_calls_the_task` is the §9.1 test.
  Not done here: the tier-2/3 stack helpers (start Compose, detect
  `podman compose` vs `podman-compose`, wait on a connection; §6), which
  stay in the workflow's setup and the developer's hands. Tier 2 refuses up
  front, with the command to run, when the broker or database is not
  listening. [`spec/21`](spec/21-local-ci.md) §1, §2, §8.2, §9.1
- [x] **P10.3** **`cargo xtask install-hooks`**: sets this repository's
  local `core.hooksPath` to the committed `.githooks/` and prints what it
  changed (old value, how to undo); `pre-commit` runs tier 0, `pre-push`
  tier 1; the global config is never read or written. Tests: the hooks only
  call the task, are executable, and the source never names `--global`.
  Run once to prove it, then **undone** here: it makes every push run the
  ~5-minute gate, which is the developer's choice to opt into.
  [`spec/21`](spec/21-local-ci.md) §7
- [x] **P10.4** **The evidence record** `target/gate/<sha>.json`, and
  `gate --post` / `--post-only` for a `local/<tier>` commit status. Every
  run of tier 1 or above writes the record (failures and skips included;
  tiers accumulate per commit); the record holds names, results, durations,
  host and toolchain, never a command line or environment. Posting refuses
  a dirty tree (now or when the tier ran), another commit, a failed tier or
  no record. Tests per §9.2 (7 in `xtask/src/gate/record.rs`). **Not run:**
  an actual `gh api` post (it would publish a status on the real
  repository); the refusal path was run live. [`spec/21`](spec/21-local-ci.md) §3
- [x] **P10.5** **A container-runtime helper**: `cargo xtask stack
  up|down|status`; detects `podman compose` vs `podman-compose` and says
  which; waits on a protocol answer (TDS pre-login, AMQP header), never a
  port. Run live (up in 31 s) and tier 2 passed against it. The hosted job
  now calls it. **Not verified:** the `podman-compose` fallback on a machine
  that needs it. [`spec/21`](spec/21-local-ci.md) §6, §6.4
- [x] **P10.6** **Scheduled local runs** of tiers 2 and 3 (launchd), one
  Compose profile at a time, writing the record of P10.4. `cargo xtask
  schedule print|install|remove|status|run`; installed on this machine at
  02:30 daily. Skips a dirty tree, a running job and an already-green commit.
  **The first nightly log has not been seen yet.**
  [`spec/21`](spec/21-local-ci.md) §2.2, §10
- [x] **P10.7** **Trial `cargo-nextest`** (0.9.149): **not adopted.**
  Tier 1 (no emulator): `cargo test --workspace --lib --tests` 61.5 s and
  59.1 s against `cargo nextest run --workspace` 35.7 s and 36.5 s, 1965
  tests all passing, so a repeatable gain of about 40%. The emulator tier is
  the problem: nextest runs every test in its own process, so the in-binary
  locks that `spec/17` §9.2.1 relies on no longer serialise anything, and
  `buswatch::emulator clearing_really_empties_a_queue` failed on the first
  nextest run (the same suite passes under `cargo test`). Making the
  emulator tests safe under nextest means per-test queues or nextest test
  groups, which is a separate piece of work for a 25 s saving. Revisit if
  tier 1 becomes the bottleneck; `cargo test --doc` would stay either way.
- [ ] **P10.8** **DEFERRED (2026-11-17, by the user): the self-hosted Azure
  DevOps agent.** The ephemeral image exists (`docker/ado-agent/`, `spec/21`
  §11) and is not registered, has run no job, and cannot run the Docker build
  stages. Picked up again only if the Hub team says ADO minutes are a real
  cost: the pipelines already use a managed pool
  (`UKS-DHCW-IH-DEV-ADOManagedPool`), the plan and the `amd64`/`arm64` match
  are unchecked, and `spec/21` §5.3 says not to rely on self-hosted minutes
  staying free.

---

## Upstream sync — October 2026

Python changes after #413, ported in `docs/upstream-sync-2026-10.md`. Done
and gate-checked except where noted.

- [x] LIMS WRRS classifier (`hl7-lims-wrrs-transformer`) —
  [`spec/09`](spec/09-transformers.md) §10
- [x] WRRS database sender (`db-sender`) — [`spec/10`](spec/10-senders.md) §7
- [x] `hl7-server` November changes: `MLLP_TIMEOUT_SECONDS`, control-id NACK
  for a malformed frame, the WDS trigger check —
  [`spec/07`](spec/07-hl7-server.md) §2.6, §3.3.3a
- [x] FHIR sender (`fhir-sender`) — [`spec/10`](spec/10-senders.md) §8.
  Compose profile `wpas-to-proms`: `wpas-rest-server`,
  `proms-fhir-transformer`, `fhir-sender`, `promptly-mock-receiver`; the
  compose test `the_wpas_flow_reaches_the_promptly_mock` passes (run, 188 s).
- [x] Promptly mock in `http-mock-receiver` —
  [`spec/11`](spec/11-mock-receivers.md) §3.10
- [x] PROMS referral rework (`#423` second half, `#435`, `#437`–`#440`,
  `#447`) — [`spec/16`](spec/16-proms-transformer.md) §10.6, verified against
  a golden regenerated from the Python.
- [x] `#452` (no Rust counterpart), `#457` (release pipeline only, none here).
- [ ] **Upstream, 3–13 Nov, not yet ported:** `mock_db_receiver` (`#446`), a
  Python-only SQL mock for the WRRS database; the tester's four `TEMP` PROMs
  samples.
- [x] Lookup service auth and trial switch —
  [`spec/20`](spec/20-lookup-service.md) §2.7–§2.9, §12
- [x] PIMS reference-data enrichment and topic flow —
  [`spec/09`](spec/09-transformers.md) §5.10–§5.12
- [x] PROMS FHIR mapping revision (also #346, #370) —
  [`spec/16`](spec/16-proms-transformer.md) §10
- [x] LIMS ingress is a topic locally (`01-architecture.md` §2.1.3a): emulator
  topic with its three subscriptions, `hl7-soap-server` and `rest-server`
  publish to it, `lims-mpi-hl7-sender` added, and the WRRS transformer and
  TRRR sender have a feed. `ihb-e2e` `the_lims_soap_flow_reaches_its_topic`.
- [ ] **Open, upstream:** the PIMS client sends `X-API-Key`, which the
  lookup service does not accept; the Python has the same gap. Written up in
  [`docs/upstream/pims-lookup-auth-mismatch.md`](docs/upstream/pims-lookup-auth-mismatch.md)
  for the Hub team to decide; not filed.
- [x] `cargo deny` advisories cleared by a lock-file bump of rustls,
  rustls-webpki, tokio and webpki-roots.
- [x] Replay job sends a page as one batch (`spec/13` §4.2) and rejects
  `REPLAY_BATCH_SIZE=0` (`spec/02` §7.2); `wrds-service`'s non-200 handling
  confirmed against the Python (`spec/09` §9.3.1).
- [x] `http-mock-receiver` vs `spec/11` §3.2/§3.4 recorded as a
  deliberate deviation (§3.8.1) after checking the Python.
- [ ] **Open:** nothing here has run against a real Service Bus or SQL Server.

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
