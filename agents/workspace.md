# The workspace

26 crates plus `xtask`. Shared libraries first, then runtimes, then the
binaries that assemble them.

---

## Shared libraries

| Crate | What it owns |
| --- | --- |
| `ihb-config` | Reading configuration out of the environment |
| `ihb-health` | The TCP health-check listener, and `health-probe` |
| `ihb-telemetry` | Audit events, redaction, metrics, tracing |
| `ihb-fields` | HL7 field navigation, copying, timestamps |
| `ihb-validation` | HL7 validation, ER7 ↔ v2.xml |
| `ihb-runtime` | Signals, graceful shutdown, backoff |
| `ihb-bus` | Azure Service Bus: sessions, peek-lock, renewal |
| `ihb-sql` | Azure SQL over TDS — see `spec/12` §5.8 |
| `ihb-tls` | Client trust roots, and the CA-bundle startup check |

## Runtimes

| Crate | What it owns |
| --- | --- |
| `ihb-transformer` | The transformer trait, runtime and receive loop |
| `ihb-sender` | The sender runtime, throttle and delivery pipeline |

A transformer binary is eight lines around a `PayloadTransformer`. A sender
binary is similar around a `Delivery`. If a change to one transformer looks
like it belongs in all three, it belongs in the runtime.

## Binaries

| Crate | Role |
| --- | --- |
| `hl7-server` | MLLP ingress, one deployment per flow |
| `hl7-soap-server` | SOAP/HTTP ingress, for the LIMS flow |
| `hl7-rest-server` | REST ingress: JSON carrying ER7 or v2.xml, answered with an HL7 ACK; RISP fan-out. Also the library `rest-server` runs for `PIPELINE=hl7` |
| `rest-server` | One image, `PIPELINE=generic` (SOAP or plain XML) or `hl7` |
| `hl7-phw-transformer`, `hl7-chemo-transformer`, `hl7-pims-transformer` | PHW, ChemoCare, PIMS → MPI |
| `xml-fhir-proms-transformer` | WPAS XML → FHIR R4B PROMs bundles |
| `hl7-sender` | MLLP egress; also builds `hl7-subscription-sender` |
| `soap-sender` | SOAP egress; also builds `soap-subscription-sender` |
| `message-store-service` | Drains the store queue into SQL |
| `message-replay-job` | Replays stored messages onto the bus |
| `hl7-mock-receiver`, `http-mock-receiver` | Test doubles |
| `buswatch` | Developer tool: browse queues and messages |
| `ultra7` | Developer tool: send synthetic HL7/XML/JSON test messages over MLLP, REST or SOAP, repeating with one field changing. Terminal port of the Tk GUI (`spec/ultra7-test-message-sender.md`); in no image |
| `integration-hub-tester` | Developer tool: run one message through one component's real library code (transformers, servers, senders) with no Service Bus, and show the result. Terminal port of the Tk "Component Tester" (`spec/integration-hub-tester-component-preview.md`); in no image |
| `dashboard` | Operations dashboard: flow health, alarms, e-mail |
| `lookup-service` | Reference-data lookup service for transformers: keyed tables over HTTP, CSV upload, GUI. Phase 1, not production ready; read-only seed store only (`spec/20-lookup-service.md`) |
| `ihb-e2e` | Tests only — the whole chain, in process and in Compose |

---

## Adding a crate

Four places, and the gate catches three of them:

1. `crates/<name>/Cargo.toml` — inherit `version`, `edition`,
   `rust-version`, `license`, `repository` and `[lints] workspace = true`.
2. Root `Cargo.toml` — add to `[workspace] members`, and add a
   `[workspace.dependencies]` entry if anything depends on it.
3. `xtask/src/main.rs` — add to `MEMBERS`, or `cargo xtask fmt-check`
   fails.
4. `pipeline-ado/<name>rust-build.yml` if it ships as a container, or
   `cargo xtask verify-pipelines` fails (`spec/18-deployment.md` §6.6).

## Dependencies

Declared once in `[workspace.dependencies]`; members write
`foo.workspace = true`. Adding one is a decision — this workspace is
deliberately thin, and `cargo deny` enforces licence and advisory policy.

The eight first-party HL7 and ER7 crates (`hl7-2`, `er7`, and their
companions) come from crates.io by exact version. They were path
dependencies until the container builds needed a build context that could
reach them; local edits now need a publish, or a `[patch.crates-io]` entry
that is not committed.

## MSRV

Current stable minus two, declared **once** as `rust-version` under
`[workspace.package]`. `cargo xtask msrv` prints it so nothing else has to
repeat it. See [`../spec/rust-msrv-n-minus-2.md`](../spec/rust-msrv-n-minus-2.md).
