# Integration Hub Beta — Rust

A rewrite of the NHS Wales [Integration Hub](../Integration-Hub-Beta) in
Rust: the cloud-native platform that receives HL7 v2 clinical messages from
NHS Wales systems, validates and transforms them, and delivers them to the
Master Patient Index and other destinations.

This is a **work in progress**. The Python it replaces is still in place and
is still what runs; nothing here is deployed. See [`tasks.md`](tasks.md) for
where the work has reached.

---

## Where to start

| If you want to | Read |
| --- | --- |
| Know what is being built and why | [`plan.md`](plan.md) |
| Know exactly how something must behave | [`spec/`](spec/) |
| Know what is done and what is next | [`tasks.md`](tasks.md) |
| Understand the flows and queues | [`spec/01-architecture.md`](spec/01-architecture.md) |
| Configure a service | [`spec/02-configuration.md`](spec/02-configuration.md) |

**[`spec/`](spec/) is the source of truth.** Code implements a section
there; a section there does not describe code. Where the two disagree, the
specification is right and the code is a defect. Every public item's rustdoc
names the section it implements.

---

## Build

```sh
cargo build --workspace
cargo test  --workspace
```

Development happens on current stable. The **minimum supported Rust version
is current stable minus three** — 1.95 at the time of writing — recorded in
the root `Cargo.toml` and checked by the gate. See
[`spec/rust-msrv-n-minus-3.md`](spec/rust-msrv-n-minus-3.md).

```sh
rustup toolchain install $(cargo xtask msrv)
cargo +$(cargo xtask msrv) check --workspace --all-targets
```

The local container runtime is **Podman**
([`spec/18-deployment.md`](spec/18-deployment.md) §5.0):

```sh
# The Service Bus emulator and its backing store, for the bus tests.
podman compose -f local/docker-compose.yml up -d
```

The workspace depends on the `hl7-rust` and `er7-rust` crates by path, so
both must be checked out beside this repository:

```
~/git/hl7-rust/hl7-rust/
~/git/er7-rust/er7-rust/
```

They are what makes this tractable — MLLP framing, SOAP envelopes, the HL7
v2 dictionary, and ER7 ↔ v2.xml conversion are solved there rather than
reimplemented here. See [`plan.md` §6](plan.md#6-dependencies).

## The gate

Every change passes all of this before it is acceptable
([`spec/17-testing-and-quality.md`](spec/17-testing-and-quality.md) §1):

```sh
cargo xtask fmt-check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo xtask verify-dictionaries
cargo deny check
cargo +$(cargo xtask msrv) check --workspace --all-targets
```

Beyond the gate:

```sh
cargo bench                            # criterion benchmarks
cd fuzz && cargo +nightly fuzz run redact -- -max_total_time=60
```

Note `cargo xtask fmt-check` rather than `cargo fmt --all`: `--all` reaches
through the path dependencies and rewrites files in the `hl7-rust` and
`er7-rust` checkouts, which are separate repositories.

## Automation

```sh
cargo xtask fmt                   # format this workspace only
cargo xtask fmt-check             # check formatting, changing nothing
cargo xtask build-dictionaries    # regenerate the HL7 schema dictionaries
cargo xtask verify-dictionaries   # fail if the committed ones drifted
cargo xtask msrv                  # print the declared MSRV
```

---

## Layout

```
Cargo.toml          workspace: members, shared dependencies, lints
plan.md             what is being built, and why
tasks.md            the ordered work list
spec/               the normative specification
xtask/              workspace automation
fuzz/               fuzz targets (own workspace; needs nightly)
crates/
  ihb-config        environment configuration                     ✅
  ihb-health        the TCP health-check listener                 ✅
  ihb-telemetry     events, redaction, metrics, tracing           ✅
  ihb-fields        HL7 field navigation and copying              ✅
  ihb-runtime       signals, shutdown, backoff                    ✅
  ihb-bus           Azure Service Bus clients                    ✅
  ihb-validation    HL7 validation and ER7 ↔ v2.xml               ✅
  ihb-transformer   the transformer trait and its runtime
  ihb-sql           Azure SQL access
  hl7-server        MLLP ingress                            [bin]
  hl7-soap-server   SOAP ingress                            [bin]
  hl7-*-transformer PHW, ChemoCare and PIMS                 [bin]
  hl7-sender, soap-sender, and their subscription variants  [bin]
  *-mock-receiver   test doubles                            [bin]
  message-store-service, message-replay-job                 [bin]
  buswatch, dashboard                                       [bin]
  xml-fhir-proms-transformer                                [bin]
```

Crates appear as their phase begins; [`tasks.md`](tasks.md) is the order.

---

## The rules that matter most

Two, from [`spec/index.md`](spec/index.md), worth knowing before reading any
code.

**Patient data does not go in a log.** A log line, an error message, a span
attribute, a metric dimension or an HTTP response body must never carry an
unredacted payload or a field value from one. Errors carry a structural path
and a reason, never a value. The message store is the one place a full
payload is kept, because replay needs it.

**Never acknowledge a message that was not delivered.** A failure to
persist is logged and swallowed; a failure to forward stops everything and
returns no acknowledgement, so the source system retries. Getting this
backwards loses a clinical message silently, which is the one failure this
system must not produce.

---

## Relationship to the Python

The Python at [`../Integration-Hub-Beta`](../Integration-Hub-Beta) is the
reference implementation and the oracle. Where this specification records an
exact string, default or ordering, it is because the Python produces it and
something outside this workspace depends on it.

Where the Rust knowingly differs, it is listed in one place:
[`plan.md` §9](plan.md#9-deliberate-deviations-from-the-python). Nothing is
changed silently.

Cutover is per-container and reversible, and the local Compose stack runs
mixed — some containers Rust, some Python — because that is what a cutover
looks like. See [`spec/18-deployment.md`](spec/18-deployment.md) §7.

## Licence

MIT.
