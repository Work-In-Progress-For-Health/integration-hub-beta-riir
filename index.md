# Integration Hub Beta (Rust) — documentation index

Every document in this repository, and when to read it.

The NHS Wales Integration Hub receives HL7 v2 clinical messages from
hospital systems, validates and transforms them, and delivers them to the
Master Patient Index and other destinations. This repository is a rewrite of
that system in Rust. It is **work in progress**; the Python it replaces is
what runs today.

---

## Start here

| You want to | Read |
| --- | --- |
| Understand the project in five minutes | [`README.md`](README.md) |
| Know exactly how something must behave | [`spec/index.md`](spec/index.md) |
| Know what is done and what is next | [`tasks.md`](tasks.md) |
| Know why the Rust differs from the Python | [`plan.md` §9](plan.md#9-deliberate-deviations-from-the-python) |
| Work in this repository as an AI agent | [`AGENTS.md`](AGENTS.md) |

---

## The specification

[`spec/`](spec/) is the **source of truth**. Code implements a section
there; a section there does not describe code. Where the two disagree, the
specification is right and the code is a defect.

| | |
| --- | --- |
| [`spec/index.md`](spec/index.md) | Conventions, and the map of all sections |
| [`01-architecture.md`](spec/01-architecture.md) | Flows, queues, sessions, the shape of the estate |
| [`02-configuration.md`](spec/02-configuration.md) | Every environment variable |
| [`03-message-bus.md`](spec/03-message-bus.md) | Service Bus: sessions, peek-lock, renewal |
| [`04-observability.md`](spec/04-observability.md) | Audit events, redaction, metrics, tracing |
| [`05-health-check.md`](spec/05-health-check.md) | The TCP health probe |
| [`06-validation.md`](spec/06-validation.md) | HL7 validation, ER7 ↔ v2.xml |
| [`07-hl7-server.md`](spec/07-hl7-server.md) | MLLP ingress |
| [`08-hl7-soap-server.md`](spec/08-hl7-soap-server.md) | SOAP/HTTP ingress |
| [`09-transformers.md`](spec/09-transformers.md) | The three HL7 transformers |
| [`10-senders.md`](spec/10-senders.md) | MLLP and SOAP egress |
| [`11-mock-receivers.md`](spec/11-mock-receivers.md) | Test doubles |
| [`12-message-store.md`](spec/12-message-store.md) | Persistence to Azure SQL |
| [`13-message-replay.md`](spec/13-message-replay.md) | Replaying stored messages |
| [`14-buswatch.md`](spec/14-buswatch.md) | The queue browser |
| [`15-dashboard.md`](spec/15-dashboard.md) | Flow health, alarms, e-mail |
| [`16-proms-transformer.md`](spec/16-proms-transformer.md) | WPAS XML → FHIR PROMs |
| [`17-testing-and-quality.md`](spec/17-testing-and-quality.md) | Tests, fixtures, lints, the gate |
| [`18-deployment.md`](spec/18-deployment.md) | Containers, Compose, pipelines, cutover |
| [`rust-msrv-n-minus-3.md`](spec/rust-msrv-n-minus-3.md) | The minimum supported Rust version |
| [`agents-directory-name-is-lowercase.md`](spec/agents-directory-name-is-lowercase.md) | What an agents directory is called |

---

## Operations

Written to be followed with a console open, not read.

| | |
| --- | --- |
| [`docs/cutover-runbook.md`](docs/cutover-runbook.md) | Per-container cutover, verification, rollback |
| [`docs/deviation-sign-off.md`](docs/deviation-sign-off.md) | The 13 deviations, for the Hub team to confirm |
| [`docs/upstream/uppsala-id-type-resolution.md`](docs/upstream/uppsala-id-type-resolution.md) | A verified upstream bug report, ready to file |
| [`docs/upstream-sync-2026-10.md`](docs/upstream-sync-2026-10.md) | What was ported from the Python after upstream #413, and what was not |

---

## For agents

| | |
| --- | --- |
| [`AGENTS.md`](AGENTS.md) | The rules that apply to every task |
| [`CLAUDE.md`](CLAUDE.md) | The same, for Claude Code specifically |
| [`agents/conventions.md`](agents/conventions.md) | How code here is written |
| [`agents/workspace.md`](agents/workspace.md) | The 26 crates, and how to add one |
| [`agents/testing.md`](agents/testing.md) | Test layers, isolation, timing |
| [`agents/deployment.md`](agents/deployment.md) | Images, Compose, pipelines |

---

## The two rules

**Patient data does not go in a log.** Not in a log line, an error message,
a span attribute, a metric dimension, or an HTTP response body. The message
store is the one place a full payload is kept, because replay needs it.

**Never acknowledge a message that was not delivered.** A failure to persist
is logged and swallowed; a failure to forward stops everything and returns
no acknowledgement, so the source system retries. Backwards, this loses a
clinical message silently — the one failure this system must not produce.
