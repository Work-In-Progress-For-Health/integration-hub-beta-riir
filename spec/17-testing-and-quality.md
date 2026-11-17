# 17 — Testing and quality

The gate every crate passes, and the evidence that the Rust behaves as the
Python did.

---

## §1 The gate

A change is acceptable when `cargo xtask gate` is clean
([`21-local-ci.md`](21-local-ci.md) §1: the task is the one definition of
the gate, and `gate.yml` only calls it). It runs, cheapest first:

```sh
cargo xtask fmt-check
cargo clippy --workspace --all-targets -- -D warnings
cargo test  --workspace
cargo doc   --workspace --no-deps           # RUSTDOCFLAGS="-D warnings"
cargo xtask verify-dictionaries
cargo xtask verify-pipelines
cargo deny check                            # licences and advisories
cargo +$(cargo xtask msrv) check --workspace --all-targets   # §1.4, when a
                                            # toolchain input changed
```

The list is shown for reading only; the task is what runs, and a test in
`xtask` fails if the workflow lists a step of its own. Clippy and the tests
run **without** `--all-features`: the `emulator` and `sqlserver` features
need services and are tier 2 (`21-local-ci.md` §2.1, §9.2 below).

§1.4 **The MSRV is checked, not intended.** The workspace declares
`rust-version` under `[workspace.package]`, and the gate builds against that
exact toolchain — because a dependency bump or a newly stabilised API raises
the real floor silently, and an MSRV nobody compiles against is a comment.
`cargo xtask msrv` prints the declared version so the check cannot drift from
the declaration. See [`rust-msrv-n-minus-2.md`](rust-msrv-n-minus-2.md).

§1.5 Two things sit outside the gate, deliberately, because neither gives a
yes-or-no answer on a single run:

```sh
cargo bench                                 # §11
cd fuzz && cargo +nightly fuzz run <target> # §10
```

Fuzzing needs a time budget and a nightly toolchain; a benchmark needs a
baseline to compare against. Both belong in CI on a schedule rather than on
every push, and a crash found by either becomes a unit test in the gate.

§1.0 The first line is `cargo xtask fmt-check`, **not** `cargo fmt --all`.
`--all` reaches beyond this workspace. When the HL7 crates were path
dependencies it rewrote files in the `hl7-rust` and `er7-rust` checkouts
beside it — separate repositories with their own formatting, which this
workspace has no business editing. They now come from crates.io, so that
particular damage is no longer possible, and the task is kept for the
second reason: it runs `cargo fmt -p <member>` once per member and **fails
if its member list has drifted from the root manifest's**. That is how a
newly added crate gets noticed instead of sitting silently unformatted.

§1.1 `cargo test` includes doc tests. A rustdoc example that does not
compile is a failing test, which is the point of writing them.

§1.2 There is no `--no-default-features` build to keep green; features in
this workspace only *add* (the `testing` fakes, the emulator tests), never
switch behaviour.

---

## §2 Lints

§2.1 Workspace `Cargo.toml`:

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

§2.2 Every crate declares `[lints] workspace = true`. A crate that opts out
of a lint does so in its own manifest with a comment saying why.

§2.3 `unwrap`, `expect` and `panic` are warnings rather than denials so
tests can use them freely. In non-test code every `#[allow]` MUST carry a
comment justifying it; CI's `-D warnings` means an unjustified one fails.

§2.3.1 Because CI promotes warnings to errors, test code needs the allow
stated explicitly, in two places:

```rust
// crates/<crate>/src/lib.rs — covers every #[cfg(test)] mod in the crate
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// crates/<crate>/tests/<name>.rs — an integration test is its own crate,
// so the line above does not reach it
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
```

Each MUST carry a comment saying it is for test code, so it is never
mistaken for a blanket exemption.

§2.4 `#![forbid(unsafe_code)]` at the top of every crate. There is no FFI in
this workspace and there is not going to be.

---

## §3 Documentation

§3.1 `#![warn(missing_docs)]` and
`#![warn(clippy::missing_docs_in_private_items)]`: everything is documented,
public or not.

§3.2 Every module opens with `//!` docs saying what it is for and which spec
section it implements.

§3.3 Every public item's rustdoc states what it does, its arguments, what it
returns, what it errors on, and carries a `# Examples` block that compiles.

§3.4 Every public item's rustdoc names its spec section, per
[`index.md`](index.md) §0.3.

§3.5 Comments explain **why**, not what. Where the Python carries a comment
explaining a workaround — why the lock renewer closes before the receiver,
why `xmlschema`'s error string is never logged, why `PID-3[2].1` is left
unset for an unknown health board — that reasoning is carried across in
substance with a pointer to the Python file it came from.

---

## §4 Test layers

Four, all required.

### §4.1 Unit

In `#[cfg(test)] mod tests` beside the code. One test per normative clause,
named for it or carrying it in a first-line comment:

```rust
#[test]
fn blank_is_the_same_as_absent() {
    // spec/02-configuration.md §1.1
```

### §4.2 Doc

The `# Examples` blocks of §3.3, executed by `cargo test`. They are the
documentation's own regression suite.

### §4.3 Integration

In `tests/`, driving only the public API: a real TCP listener, a real HTTP
server, a real SQL Server in a Podman container. Anything a service does over
a socket is tested over a socket.

§4.3.1 `ihb-bus` ships a **fake bus** behind its own traits, under a
`testing` feature, so no integration test needs a broker
([`03-message-bus.md`](03-message-bus.md) §7.1).

§4.3.2 Tests that do need the Service Bus emulator or SQL Server are behind
a feature and skip cleanly when it is absent. They MUST also run in CI,
where those services are available.

### §4.4 Golden

The equivalence evidence. This is the layer that makes the rewrite
defensible.

§4.4.1 Fixtures are copied into `crates/<crate>/tests/corpus/` from:

```
Integration-Hub-Beta/local/sample_messages/*.hl7, *.xml
Integration-Hub-Beta/hl7_chemo_transformer/tests/chemo_messages.py
Integration-Hub-Beta/hl7_pims_transformer/tests/pims_messages.py
Integration-Hub-Beta/xml_fhir_proms_transformer/tests/wpas_messages.py
Integration-Hub-Beta/shared_libs/hl7_validation/tests/test_*.py
```

§4.4.2 For each fixture, the Python's output is captured **once**, by
running the Python, and committed alongside as the golden file. CI compares
against the committed file and needs no Python runtime.

§4.4.3 Capturing a golden file is a deliberate act with a recorded reason.
Re-capturing one to make a test pass is prohibited unless the change is a
recorded deviation ([`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python))
or a specification change.

§4.4.4 Comparison is byte-for-byte for ER7, XML-canonical for v2.xml, and
JSON-canonical for FHIR bundles and store events.

---

## §5 Determinism

A golden test cannot compare output that changes per run. Three sources of
non-determinism are injected rather than called directly:

| Source | Injected as |
| --- | --- |
| Wall-clock time | A clock trait; tests use a fixed instant |
| UUID generation | A factory; tests use a counting generator |
| Random | Not used anywhere in this workspace |

§5.1 A crate MUST NOT call `SystemTime::now`, `Instant::now` or
`Uuid::new_v4` outside its composition root. The Python's transformers
already thread a UUID factory for this reason
([`16-proms-transformer.md`](16-proms-transformer.md) §4.4).

§5.2 Throttling and backoff tests use a paused tokio clock, not real
sleeps. A test suite that sleeps is a test suite that gets deleted.

---

## §6 PII in tests

§6.1 Every crate that can render an error containing message-derived text
MUST have a test that pushes a payload carrying a distinctive marker through
every failure path and asserts the marker does not appear in the rendered
error. [`index.md`](index.md) §0.8.4.

§6.2 Every crate that logs MUST have a test that captures log output for a
representative message and asserts the redaction rules of
[`04-observability.md`](04-observability.md) §2 held.

§6.3 Fixtures MUST NOT contain real patient data. The Python's fixtures are
synthetic (`Lims2val`, `TEST^FOUAZ`, `B0000010612`) and are used as-is; a new
fixture MUST be synthetic too.

---

## §7 Coverage

§7.1 There is no coverage percentage target. A percentage is satisfied by
testing what is easy, and this specification is explicit about what must be
tested instead.

§7.2 The actual requirement: **every normative clause that can be observed
has a test that cites it.** A spec section with no citing test is an
incomplete crate, and the section-to-test mapping is checked at review.

§7.3 `cargo llvm-cov` MAY be run to find code no test reaches. Uncovered
code is a prompt to ask whether it should exist, not a number to raise.

---

## §10 Fuzzing

`fuzz/`, driven by `cargo-fuzz` and libFuzzer.

§10.1 The fuzz crate is **not** a workspace member. `cargo-fuzz` builds with
its own flags on a nightly toolchain, and making it a member would make
`cargo build` at the root need nightly too.

§10.1.1 It is therefore **exempt from the MSRV**
([`rust-msrv-n-minus-2.md`](rust-msrv-n-minus-2.md)): it already requires
nightly, so a floor two releases behind stable means nothing to it. The
MSRV job of §1.4 does not build it. Nothing ships from `fuzz/`, so no
consumer is affected.

§10.2 A target exists for every surface that (a) sees input this workspace
does not control, and (b) must not panic. A panic in an ingress server is a
denial of service that anyone who can reach a port can trigger.

| Target | Surface | Properties |
| --- | --- | --- |
| `redact` | Any payload that reaches a log | returns; leaks nothing; idempotent |
| `xml_normalise` | SOAP payloads, pre-parse string surgery | returns |
| `validate_flow` | Any message at ingress | returns; never quotes the message |
| `config` | Container configuration | returns; never prints a secret |
| `fields` | Any message a transformer maps | returns; loses no populated repetition |

§10.3 A target asserts **properties**, not outputs. "It does not panic" and
"the marker does not come out" hold for all inputs; "this field reads as
empty" does not, because the fuzzer's input goes *into* the message and can
create the very field the assertion says is absent. Two of the five targets
had to be corrected on exactly that point.

§10.4 A crash MUST be fixed and its input kept as a corpus seed, so the same
bug cannot return unnoticed.

§10.5 Findings so far:

| Found | Where | Outcome |
| --- | --- | --- |
| Panic on `MSH` + any non-ASCII character | `redact_hl7` | Fixed; see [`04-observability.md`](04-observability.md) §2.5a |
| An all-empty repeating field copies as nothing | `ihb-fields::copy` | Recorded as an open question, [`09-transformers.md`](09-transformers.md) §2.2.2 |

The first was a five-byte input that would have killed an ingress server,
found within seconds of the first run, in code carrying a comment asserting
it was safe.

---

## §11 Benchmarks

`benches/`, using `criterion`, run with `cargo bench`.

§11.1 Benchmarks exist to answer design questions with numbers, not to
decorate a README. Each one MUST be tied to a decision:

| Benchmark | The question it answers |
| --- | --- |
| `ihb-validation` validation routes | Which validation route can sit on an ingress path? ([`06-validation.md`](06-validation.md) §9.5.3) |
| `ihb-validation` conversion | What does the message store's XML cost per message? |
| `ihb-telemetry` redaction | What does the most frequently executed function in the estate cost? |

§11.2 A benchmark MUST measure steady state, with caches warm. One-off costs
— schema compilation — are measured separately and asserted with a
threshold in a test, where they can fail a build; a benchmark cannot.

§11.3 A benchmark's name MUST describe what it measures. A benchmark called
"compile a schema" that measures a warm cache is worse than no benchmark,
because it will be quoted.

---

## §8 Dependencies

§8.1 Every dependency is declared once in `[workspace.dependencies]` and
referenced by members as `dep.workspace = true`.

§8.2 No `openssl`. `rustls` everywhere, so the image needs no system TLS
library and the corporate CA is a PEM file rather than a trust-store
rebuild.

§8.3 `cargo deny` gates licences and advisories. A new dependency with an
unreviewed licence fails the build.

§8.4 The HL7 crates (`er7`, `er7-redact`, `hl7-2` and its siblings) are path
dependencies with a version alongside, so publishing later is a one-line
change per crate.

---

## §9 Continuous integration

§9.1 CI runs §1's gate on every push and every pull request.

§9.2 CI additionally runs the emulator-backed tests of §4.3.2, with the
Service Bus emulator and SQL Server as services, under Podman
([`18-deployment.md`](18-deployment.md) §5.0).

§9.2.1 Emulator-backed tests run in **parallel**, as `cargo test` runs
everything else, and they MUST be isolated from each other by construction
rather than by a `--test-threads=1` nobody can enforce. Two units of
isolation exist and both are required:

- **A session id.** A session-enabled queue hands out one lock per session
  at a time, so two tests naming the same session either fight over the lock
  or read each other's messages. Session ids are arbitrary to the broker, so
  each test uses its own.
- **A queue.** A non-session queue gives any message to any receiver, and
  there is no id to vary. Tests that *consume* from the same non-session
  queue take turns on a lock in the test binary.

§9.2.1b **A shared queue also means no fixed message ids.** A test that
sends a message with a constant id and then looks for it will find the
*previous* run's message instead, if that run was interrupted before it
settled — abandoned, with a raised delivery count. The test then fails
asserting something about first delivery, with a number and no explanation.

This is not hypothetical: it is what a cancelled CI run leaves behind, and
`gate.yml` sets `cancel-in-progress` deliberately. Ids carry a per-run
suffix, as correlation ids in the SQL-backed tests already do.

§9.2.1d **A query that scopes a database assertion by prefix must scope it
to *this* run.** `message-store-service`'s `a_batch_shares_one_stored_at`
matched correlation ids with `LIKE 'store-test-stamp-0%'`, which matched
every previous run's rows as well — so it failed on the second run against
a container that had not been recreated.

The same expression was also wrong in a way that made it **vacuous**: each
id carries its own nanosecond stamp, so the prefix of `ids[0]` matched one
row of a three-row batch. It asserted "one `StoredAt` across the batch"
while looking at one row, and would have passed however the batch was
stamped. Match the exact keys, and assert the **row count** as well as the
property, so the second assertion cannot pass on an empty set.

§9.2.1e A test that needs a container MUST check the container is there
**before** it starts, and say so in the failure. The emulator suite had no
such check: with the emulator stopped, the AMQP client retried a
connection nobody was listening for and the run took **eight minutes**
before failing — naming a test that was entirely healthy rather than the
missing container.

`require_emulator` in `crates/ihb-bus/tests/emulator.rs` does a two-second
TCP connect first and fails with the `podman compose` line that fixes it.
The suite now reports the real problem in about a second.

A TCP connect proves the port is *open*, not that the broker is *ready* —
the emulator accepts TCP roughly ten seconds before it will complete a
SASL exchange, because it waits on SQL first. The guard deliberately does
not try to cover that gap: it separates "not running" from "not ready", and
those two need different messages.

§9.2.1c These three rules exist because a test binary's own lock does
**not** isolate across binaries — `cargo test` runs them concurrently, so
two crates whose emulator tests touch one queue can still collide however
carefully each is written internally. Where that is possible, the queues
are chosen so no two binaries share one.

§9.2.1a The same rule applies to **process-global** state inside one test
binary. A test whose premise is "nothing is configured" must establish that
premise, not inherit it: `ihb_telemetry::otel::configure` installs a global
`tracing` subscriber permanently, so a sibling test asserting that a span is
disabled passed or failed according to the order `cargo test` happened to
pick. Scope the state the assertion depends on.

§9.2.2 This is not a tidiness rule. Sharing one session across the
`ihb-bus` emulator tests failed a **different** test on each run — the worst
kind of flake, because it looks like a real defect somewhere else. Isolating
them also made the suite twice as fast, since the contention was resolved by
waiting.

§9.3 CI builds every container image, per
[`18-deployment.md`](18-deployment.md).

§9.4 A merge with a failing gate is not possible. There is no
"fix it in the next commit" for a workspace whose output is clinical data.

§9.5 **The gate can run locally, and one command defines it.** §1's list
and `.github/workflows/gate.yml` had drifted apart (`--all-features`,
`verify-pipelines`), and the hosted `emulator-backed` job of §9.2 failed in
every run inspected on 2026-11-04, so the emulator tier had no hosted
coverage. [`21-local-ci.md`](21-local-ci.md) specifies `cargo xtask gate`,
its tiers, and how a local run is evidenced; once it lands, §1's list is
generated from it rather than maintained beside it.
