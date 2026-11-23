# Testing

`spec/17-testing-and-quality.md` is normative. This is the working guide.

---

## The layers

| Layer | Needs | How to run |
| --- | --- | --- |
| Unit and integration tests | nothing | `cargo nextest run --workspace` |
| Doc tests | nothing | `cargo test --workspace --doc` |
| Emulator-backed | Service Bus emulator | `--features emulator` |
| SQL-backed | SQL Server container | `--features sqlserver` |
| In-process end to end | emulator | `-p ihb-e2e --features emulator` |
| Container end to end | images + Compose | `-p ihb-e2e --features compose` |

Anything needing a service is behind a feature, so a plain
`cargo test --workspace` never fails for want of a broker.

```sh
podman compose -f local/docker-compose.yml --profile bus up -d
cargo test -p ihb-bus --features emulator --test emulator -- --test-threads=1
```

---

## Isolation, the hard-won part

Shared queues and a shared database make tests interfere in ways that look
like defects elsewhere. Four rules, each from a real failure
(`spec/17-testing-and-quality.md` §9.2.1):

**Vary the session id per test.** A session-enabled queue hands out one lock
per session at a time, so two tests naming the same session fight over it or
read each other's messages. `session(label)`.

**Vary the message id per run.** A fixed id is poison: an interrupted run
leaves its message behind, abandoned with a raised delivery count, and the
next run finds that one. `message_id(label)`.

**Serialise consumers of a non-session queue.** There is no id to vary, so
the queue is the unit of isolation and those tests take turns.

**Beware assertions that are both polluted and vacuous.** A `LIKE` prefix
that matches previous runs, and matched one row of three. It passed for
years and proved nothing.

---

## Fail fast when a service is missing

A test that needs a container checks it is there **before** it starts, and
says so. Without that, the emulator suite once spent **eight minutes**
retrying AMQP and then blamed a healthy test.

`require_emulator` does a two-second TCP connect and names the command that
fixes it. But note the limit: TCP-open proves the port is open, not that the
broker is ready — the emulator accepts TCP about ten seconds before it will
complete a SASL exchange, because it waits on SQL first. To wait for
*readiness*, poll a real connection (`§9.2.1e`).

---

## What a test is for

**Name a test after the clause it protects.** A future reader needs to know
whether deleting it is safe.

**Test the thing that could actually be wrong.** The in-process end-to-end
test exists because queue names, session ids and message rendering are what
two services can disagree about; every service's own tests use whatever name
that test chose.

**A test double must not lie.** `Live::throughput` once returned a
hardcoded `"24h"` and ignored its argument, so every test of it passed and
the feature was broken.

**Golden corpora beat remembered constants.** Two of four FHIR constants
written from memory were wrong. Extracting all 52 into a corpus with a
name-by-name test found them; nothing else would have.

---

## Timing

A cold three-hop flow through containers takes about **four minutes** —
each hop waits out its sixty-second receive window on an idle queue. Do not
mistake a slow flow for a broken one, and do not wrap a receive in a
shorter `tokio::time::timeout`: that **cancels** it and loses messages the
broker already handed over (`spec/03-message-bus.md` §5.5).

## Beyond the gate

```sh
cargo bench                                              # criterion
cd fuzz && cargo +nightly fuzz run redact -- -max_total_time=60
```
