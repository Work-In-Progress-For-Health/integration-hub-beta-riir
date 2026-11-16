# 18 — Deployment

The container contract, the images, the local stack, and how a flow is cut
over from Python to Rust.

Reference implementation: `Integration-Hub-Beta/*/Dockerfile`,
`Integration-Hub-Beta/local/docker-compose.yml`,
`Integration-Hub-Beta/pipeline-ado/`.

---

## §1 The container contract

A Rust image MUST be a drop-in replacement for the Python image it
supersedes. That means:

§1.1 **The same environment variables**, with the same names and the same
defaults ([`02-configuration.md`](02-configuration.md)).

§1.2 **The same ports.** MLLP servers on their flow's port, web services on
8080, health checks on `HEALTH_CHECK_PORT`.

§1.3 **A non-root user.** UID **5678**, matching the Python images, so a
mounted volume's ownership does not change.

§1.4 **`SIGTERM` means shut down gracefully**, within the platform's
termination grace period.

§1.5 **Exit non-zero on a configuration error**, before binding anything, so
a misconfigured revision fails fast rather than crash-looping on the first
message.

§1.6 Nothing is written to the image's filesystem at run time. The container
is read-only apart from `/tmp`.

---

## §2 Images

§2.1 Two-stage build, built with `podman build`:

```dockerfile
FROM rust:1-bookworm AS build
WORKDIR /src
# Dependencies first, so a source change does not rebuild the world.
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin <service> && \
    cp target/release/<service> /usr/local/bin/<service>

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /usr/local/bin/<service> /usr/local/bin/<service>
USER 5678:5678
ENTRYPOINT ["/usr/local/bin/<service>"]
```

§2.2 `--locked`. A build that would change `Cargo.lock` fails.

§2.3 `distroless` is possible because §8.2 of
[`17-testing-and-quality.md`](17-testing-and-quality.md) forbids `openssl`:
there is no system TLS library to link against.

§2.4 One image per `[[bin]]`. An image name matches the Python service it
replaces, so the Azure Container App, the ADO pipeline and the `.env` file
all keep their names.

§2.5 Images are reproducible: the same commit produces the same binary. No
build-time timestamps, no build-time hostnames.

§2.6 **Every dependency resolves from crates.io.** The workspace previously
reached the `hl7-rust` and `er7-rust` crates through `path`, at
`../../../git/…` — a different tree from this repository, three levels up.
No build context can contain both, so §2.1's Dockerfile could not build any
binary at all.

All eight are published, so the `path` keys were removed and each is pinned
to the **exact** published version rather than a caret range: a container
build that resolved a different patch release than the gate ran against
would be a different binary from the one that was tested.

§2.6.1 The cost is that a local edit to `hl7-rust` or `er7-rust` no longer
appears here without a publish, or a `[patch.crates-io]` section added for
the duration of the work. That is a deliberate trade for buildable images.

§2.7 **One Dockerfile, parameterised by `--build-arg BIN`.** §2.4 wants one
image per `[[bin]]` and there are sixteen; sixteen near-identical files
would drift. `docker/build-all.sh` reads the binary names from the
manifests rather than a list, so a new service cannot be forgotten.

§2.9 **`crates/` is not the whole build.** Two binaries compile a file
from `local/` into themselves with `include_str!` —
`buswatch` (`ServiceBusEmulatorConfig.json`,
[`14-buswatch.md`](14-buswatch.md) §2.2) and `hl7-soap-server` (a sample
message). §2.1's example copies only `Cargo.toml`, `Cargo.lock` and
`crates/`, which builds fourteen of the sixteen and fails on those two.
Found by building all sixteen, not by reading the manifests.

§2.8 The image sets `HEALTH_CHECK_HOST=0.0.0.0`
([`05-health-check.md`](05-health-check.md) §2.2). Leaving it to each
container definition is a footgun: a probe bound to loopback inside a
container is unreachable from the orchestrator, so the service reports
unhealthy while working perfectly and the platform restarts it. An
environment variable still overrides the image default.

---

## §3 Health probes

§3.1 A TCP probe against `HEALTH_CHECK_PORT`
([`05-health-check.md`](05-health-check.md)).

§3.2 Web services MAY additionally expose an HTTP probe; the dashboard's
`/healthz` is retained.

§3.3 A readiness probe MUST NOT succeed before the service's ingress socket
is bound and its bus clients are constructed.

---

## §4 Certificates

§4.1 The corporate CA bundle is mounted as a PEM file and named by
`SSL_CERT_FILE`. `rustls` reads it at startup.

§4.2 The image does **not** rebuild an OS trust store. The Python's
`update-ca-certificates` step has no counterpart, which is what allows §2.3's
distroless base.

§4.3 A configured `SSL_CERT_FILE` that cannot be read, or that contains no
certificates, is a startup failure. Silently continuing with the default
roots is how a certificate problem becomes a TLS failure three hops away.

§4.4 **The bundle is additional, not a replacement.** OpenSSL gives
`SSL_CERT_FILE` the opposite meaning — setting it *replaces* the default
trust store — so the name is borrowed but the semantics are not. Here the
file's certificates are added to the compiled-in Mozilla roots, because
`spec/02-configuration.md` §1 calls it a bundle of "additional trusted
roots" and because a corporate CA is issued alongside the public ones
rather than instead of them.

A deployment that set the variable expecting OpenSSL's meaning would get a
*larger* trust set than intended, never a smaller one. That is the safe
direction to be wrong in, but it is a real divergence and is stated here so
it is not discovered by surprise.

§4.5 §4.3 is enforced by `ihb_tls::startup_check`, called by **every**
service after telemetry is configured and before any port is bound — not
only by the services that obviously speak TLS. A service that ignored a
broken value would be the one still running while its neighbours refused
to start, which is the least useful arrangement available.

Four things count as unusable, and the third is the one that motivates the
rule:

| The file | Why it fails |
|---|---|
| does not exist | the mount is missing |
| is empty | the mount is there and wrong |
| holds only a private key | the plausible operator mistake — key and certificate side by side, wrong one named |
| holds no certificate rustls accepts as a root | it parsed, and contributed nothing |

The third and fourth are exactly the cases a fallback would hide: the file
is present, readable and valid PEM, and the default roots would carry on
working until the one connection that needed the corporate CA failed.

§4.6 A service with no `SSL_CERT_FILE` set starts normally on the built-in
roots. Blank counts as unset (`spec/02-configuration.md` §1), because a
templated container definition whose source was empty produces
`SSL_CERT_FILE=` and that must not be read as a request to load a file
named by the empty string.

§4.7 **The image must clear `SSL_CERT_FILE`.** `gcr.io/distroless/cc-debian12`
presets it to `/etc/ssl/certs/ca-certificates.crt`, which breaks §4.2 and
§4.3 at once and is invisible from the source:

- the service adds the base image's ~150 OS certificates to the compiled-in
  roots — the OS trust store §4.2 says this image does not use;
- and §4.3 can **never fire**, because the variable is always already set
  to a path that reads perfectly. An operator who mounted a corporate CA
  and forgot to name it would get a clean startup, a healthy container,
  and a handshake failure later in whichever service needed that CA
  first — precisely the outcome §4.3 exists to prevent.

`docker/Dockerfile` therefore sets `ENV SSL_CERT_FILE=""`. Blank is absent
(§4.6), so the meaning is restored: unset means the compiled-in roots, and
a set value is the operator's decision.

This was found by running the built image, not by reading it. The unit
tests could not have caught it — they exercise the library, and the
defect lived in the base image's environment.

---

## §5 Local stack

### §5.0 Podman, not Docker

§5.0.1 The local container runtime is **Podman**. Every command in this
workspace's documentation, scripts and `justfile` MUST be written for it.

§5.0.2 The reason is the estate's, not this rewrite's: the Python repository
already carries `docs/PODMAN_COMPOSE_SETUP.md` and
`docs/PODMAN_CORPORATE_CA_FIX.md`, and its own `justfile` calling
`docker compose` is recorded there as a defect that made
`just start phw-to-mpi` fail outright. This workspace does not repeat it.

### §5.3 SQL locally is Azure SQL Edge, not SQL Server

§5.3.1 The `sqledge` service is the database for local development, and it
publishes 1433. `ihb-sql`'s tests, and those of the two persistence
services, connect to it.

§5.3.2 **`mcr.microsoft.com/mssql/server` does not run on Apple Silicon.**
Microsoft ships no arm64 image; under emulation the container segfaults
during start-up (`launch_sqlservr.sh: Segmentation fault (core dumped)`) and
restarts forever. `mcr.microsoft.com/azure-sql-edge` is arm64-native and is
what the stack uses.

§5.3.3 The `sqlserver` service behind the `sql` profile is kept for an
amd64 host, where it is the closer match to Azure SQL. On arm64 it is not
usable and `sqledge` is the only option.

§5.3.4 Edge is a subset of SQL Server, and everything the persistence
services use — `datetime2`, `NVARCHAR(MAX)`, identities, explicit
transactions, `READPAST`, `SCOPE_IDENTITY` — behaves the same on both. The
schema in `local/sql/schema.sql` applies unmodified to either.

§5.3.5 The schema is applied by the **tests**, not by hand or by an entry
point: the Edge image ships no `sqlcmd`, and a setup step a developer has to
remember is a setup step that is sometimes skipped. Every statement guards
itself, so a run does not need a clean container.

§5.3.6 **The schema creates the `IntegrationHub` database, and the tests use
it.** `message-store-service.env` and `message-replay-job.env` set
`SQL_DATABASE=IntegrationHub`, and §5.2 keeps those files unchanged — so
that database has to exist. Nothing created it: `sqlcmd` is absent (§5.3.5)
so there is no entry-point script, and the tests connected to `master`,
which meant the schema they applied was in a different database from the one
the containers read.

The consequence was the same shape as §5.1.3a's: both persistence services
started, reported **healthy**, and failed every batch with `Cannot open
database "IntegrationHub"`. Found by bringing the `replay` profile up.

§5.3.7 `sqledge` also answers to the alias **`sqlserver`**, because that is
the host name those two `.env` files use and §5.2 forbids editing them. On
arm64 the `sqlserver` service cannot run at all (§5.3.2), so without the
alias the name resolves to nothing.

§5.3.8 The ordering this leaves is worth knowing: the database exists only
after the tests have run once. A developer who brings the stack up on a
fresh container without running them gets a persistence layer that reports
healthy and stores nothing. That is §5.3.5's design followed to its
conclusion; if it proves awkward, the fix is an init container rather than
a change here.

§5.4a **`depends_on` waits for *started*, not *ready*.** The Service Bus
emulator accepts AMQP about forty seconds after its container starts, and a
service that connects before then exits non-zero per §1.5. `restart:
on-failure` is what closes the gap — a profile converges to healthy in
roughly forty-five seconds with no manual step, which is what §5.4 asks.
The emulator image carries no shell, so it cannot have a healthcheck of its
own and `condition: service_healthy` is not available for it.

§5.0.3 Compose files stay in Compose format and keep the filename
`docker-compose.yml`, because that is what every tool looks for and renaming
it buys nothing. `podman compose` is a thin wrapper that runs whichever
compose provider is installed against the Podman socket, so the same file
works either way:

```sh
podman compose -f local/docker-compose.yml --profile phw-to-mpi up -d
```

§5.0.4 Images are built with `podman build` and run with `podman run`. The
Containerfile format is Dockerfile format; the files keep the name
`Dockerfile` for the same reason as §5.0.3.

§5.0.5 Nothing in a container image may depend on the runtime. An image
built by Podman and one built by Docker must be interchangeable, so a build
that works only under one of them is a defect in the build, not a
configuration choice.

§5.1 `local/docker-compose.yml` in this workspace mirrors the Python's,
profile for profile:

```
phw-to-mpi     paris-to-mpi     chemo-to-mpi     pims-to-mpi     dashboard
```

§5.1.1 Beyond that list, the stack carries `mosaiq-to-mpi` and `risp-to-mpi`
(upstream `7157ee3`, `2b26c7f`). `paris-to-mpi`, `mosaiq-to-mpi` and
`risp-to-mpi` each run a **dedicated `hl7-sender`** (`paris-mpi-hl7-sender`,
`mosaiq-mpi-hl7-sender`, `risp-mpi-hl7-sender`) fed by a topic subscription,
delivering to `mpi-hl7-topic-mock-receiver`; none of them starts the shared
`mpi-hl7-sender` or the queue `mpi-hl7-mock-receiver`
(`01-architecture.md` §2.1.3). The Paris server service and env file are
`cav-paris-hl7-server` (upstream `9e797ac`), and its `MICROSERVICE_ID` is
`cav_paris_hl7_server`. `risp-to-mpi` holds only its sender here: the REST
server and the core-reference transformer have no Compose service yet, so
nothing publishes to it locally.

§5.2 The `.env` files are copied from `Integration-Hub-Beta/local/`
**unchanged**. If one needs changing, §1.1 has been violated. (The queue to
topic migration is the case that proves the rule: upstream changed its env
files, and this workspace changed the same keys to the same values —
`local/*.env` here are flat, without upstream's `env/` subdirectories — and
kept PHW, ChemoCare, PIMS and WDS on the shared queue, `01-architecture.md`
§2.1.4.)

§5.2.1 **`compose down` only stops the profiles it is given.** A bare
`podman compose down` leaves every profile-scoped container running and
reports success: it prints a network-removal line, exits zero, and the
containers stay up.

Anything tearing the stack down MUST name every profile, and MUST then
check the container list rather than the exit code. `compose` also accepts
a profile name that does not exist **without complaint**, so a typo in that
list fails silently too — which is why the check is on `podman ps`, not on
the command.

This was found by `crates/ihb-e2e/tests/in_compose.rs`, whose
one-profile-at-a-time rule it had quietly defeated: each run added to the
last until the emulator's connection quota (`14-buswatch.md` §6.1) refused
the next connection.

§5.2.2 **"Nothing is `(starting)`" is not readiness.** It is true *before
the application containers exist*: only `sqledge` and `sb-emulator` are up,
neither declares a health check, and a single `podman ps` reading sees no
`(starting)` at all.

A stack is ready when something reports `(healthy)`, nothing reports
`(starting)` or `(unhealthy)`, and that has held for several consecutive
readings. One reading is a race.

§5.2.3 The `lims-to-mpi` profile starts `lims-soap-hl7-server` (the `rest-server`
binary, `rest-server-lims.env`; `01-architecture.md` §2.1.3a),
`lims-mpi-hl7-sender` and the topic mock receiver. The SOAP server publishes
to the topic `local-inthub-lims-hl7-input` with session
`lims-soapserver`; the sender reads `…-lims-mpi-sender-sub` and delivers to the topic mock
receiver (`01-architecture.md` §2.1.3a). The shared `mpi-hl7-sender` and the
queue mock receiver are **not** in this profile: they read
`mpi-sender-ingress`, which this flow never writes, and were idle there.
`lims-to-wrrs` (with `wrrs-idupd-dbsender`) and `lims-to-trrr` also start
`lims-soap-hl7-server`, so each has a
feed; run one profile at a time (the emulator's connection quota, `14-buswatch.md`
§6.1).

§5.3 `local/ServiceBusEmulatorConfig.json` and
`local/sample_messages/` are copied across unchanged and are the fixtures of
[`17-testing-and-quality.md`](17-testing-and-quality.md) §4.4.1.

§5.4 A profile is started with `podman compose --profile <name> up` and MUST
reach a healthy state with no manual step.

§5.5 The stack MUST be runnable **mixed** — some containers Rust, some
Python — because that is what a cutover looks like. Nothing in either
implementation may assume its neighbour is the same language.

§5.5.1 `local/docker-compose.mixed.yml` is the overlay that does it. It
changes **only** the `image:` and the healthcheck — the `.env` file, the
queue names, the session ids and the network alias are the same ones the
all-Rust run uses:

```sh
podman compose -f local/docker-compose.yml -f local/docker-compose.mixed.yml \
  --profile phw-to-mpi up -d
```

§5.5.2 The healthcheck has to change because the two images differ in what
they can run: the Python image has an interpreter and uses the Python
stack's own `python -c "…socket…"`, and the Rust image is distroless and
uses the `health-probe` of §2.1. Both answer the same question.

§5.5.3 Verified: a message crossed **Rust ingress → Python transformer →
Rust sender → Rust mock receiver**, acknowledged `MSA|AA` at the ingress
and logged `forwarded control_id=MIXED1` at the receiver.

§5.5.4 One obstacle worth recording for whoever builds the Python images
next: `hl7_phw_transformer/Dockerfile` copies `*.crt` from its `ca-certs`
build context, and `Integration-Hub-Beta/ca-certs/` ships a single `.cer`
file. The build fails on that `COPY` until a `.crt` is present. Nothing in
this workspace can fix it, and it is not a defect in this port — it is
noted so the next person does not spend the time twice.

---

## §6 Pipelines

§6.1 `pipeline-ado/` gains a build definition per Rust service, mirroring
the Python's naming (`hl7serverrust-build.yml` beside `hl7server-build.yml`).

§6.2 A build runs the full gate of
[`17-testing-and-quality.md`](17-testing-and-quality.md) §1 before it builds
an image.

§6.3 Release definitions are unchanged: they deploy an image to a container
app, and the image is now built from a different repository.

§6.4 That is only true if three things stay identical, because a release
reads exactly three things from a build:

| What | Where the release reads it |
|---|---|
| the ACR repository name | `appImageName` in `release-apps.yml` |
| the tag | `imageTag/imageTag.txt`, published as a build artefact |
| the producing pipeline | `resources.pipelines[].source`, **by name** |

All three are preserved. `hl7serverrust-build.yml` pushes to `hl7server`,
not to `hl7serverrust`, and publishes the same `imageTag` artefact in the
same format.

The third is the one that surprises people: a release refers to its build
by the ADO **pipeline name** (`'HL7 Server - Build'`), not by a file path.
So cutover is not editing any release YAML — it is repointing that named
pipeline definition at this repository and at
`pipeline-ado/hl7serverrust-build.yml`. The name stays; the repository and
the file behind it change. That is what §6.3's "built from a different
repository" means in practice, and it is why the `rust` suffix is on the
**file** and never on the image.

§6.5 Five services — `soap-sender`, `soap-subscription-sender`,
`http-mock-receiver`, `xml-fhir-proms-transformer` and `buswatch` — are
containerised in the Python repository but have **no ADO build definition
there**. There is nothing to mirror and no release consuming the image, so
their ACR repository names are chosen here rather than inherited. Each such
file says so in its header. Nothing breaks if a future release picks a
different name, because nothing depends on these yet.

§6.6 `cargo xtask verify-pipelines` checks the definitions against the
workspace, and the gate runs it (§6.2). It catches two failures that are
otherwise silent:

- a definition naming a `BIN` that does not exist — which fails the image
  build, but only after a full dependency compile on a build agent;
- **a deployable binary with no build definition at all** — which fails
  nothing. The service simply never ships, and the omission stays
  invisible until somebody looks for an image nobody ever pushed.

The second is why the task exists. `xtask`'s own binary and `health-probe`
are excluded by name: the first is the tool, and the second ships *inside*
every image rather than as one of its own (§2.1).

§6.7 The gate runs **once**, not once per service. The Python runs a
code-quality stage per app because each app is a separate Python package
with its own dependency set; this is one Cargo workspace, so
`cargo test --workspace` checks a shared crate against every crate that
uses it. Sixteen per-app stages could not do that, so this is a
strengthening rather than a saving.

---

## §7 Cutover

The order is chosen so that the first thing to move is the thing whose
failure costs least.

§7.1 **Order.**

1. `hl7-mock-receiver` — a test double; already ported and proven.
2. `http-mock-receiver` — likewise.
3. `message-store-service` — a failure delays persistence; the pipeline
   keeps running and the queue absorbs the backlog.
4. `hl7-sender` for one flow, then the rest.
5. One transformer, then the rest.
6. `hl7-server` for one flow, then the rest.
7. `hl7-soap-server`.
8. `message-replay-job`.
9. `buswatch`, then `dashboard`.

§7.2 **Per-container procedure.**

1. Confirm every deviation in
   [`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python) that
   touches this container, with the Hub team.
2. Deploy the Rust revision alongside, taking no traffic.
3. Shift traffic, watching the flow's queue depth, its dead-letter depth,
   and its `messages_received` / `messages_sent` counters.
4. Hold for one full business day.
5. Scale the Python revision to zero. Do not delete it.

§7.3 **Rollback** is scaling the Python revision back up and the Rust one
to zero. Nothing in the Rust changes a queue's shape, a message's shape or
the SQL schema, so a rollback needs no data migration. **This property MUST
be preserved for the whole cutover** — a change that breaks it is not a
cutover step, it is a migration, and needs its own plan.

§7.4 A flow is cut over one container at a time. Two at once means an
incident has two candidate causes.

§7.5 **§7.2 step 2 is not achievable with the release pipeline as
written.** "Deploy the Rust revision alongside, taking no traffic" needs
the container app in multiple-revision mode with explicit traffic weights.
The release runs `az containerapp update --revision-suffix --image` and
sets no weights, so in the default **single-revision** mode the new
revision takes 100% of traffic the moment the command returns.

Either the app is switched to multiple-revision mode before the deploy — a
change to the app, worth its own approval on a live one — or the step is a
replace and the runbook says so. It must not be described as a shift when
it is not.

§7.6 **Traffic weights apply to ingress, and half of these containers have
none.** The transformers, the senders, `message-store-service` and
`message-replay-job` pull from Service Bus. Two revisions of a queue
consumer running at once do not split by weight; they compete for the same
queue.

That is safe — a session-enabled queue hands each session to exactly one
receiver, so the revisions divide the sessions and no message is processed
twice — but it is not a traffic shift. What the operator controls is the
replica count of each revision. The two procedures are genuinely different
and `docs/cutover-runbook.md` gives them separately (§2a, §2b).

§7.7 **MLLP is TCP ingress.** Where weighted splitting does apply to an
HL7 server, it splits **connections**, not messages. A sending system
holding one long-lived connection sits entirely on one revision, so a 10%
weight may deliver none of that peer's messages or all of them. The shift
is judged by the counters of §8, never by the weight that was set.

§7.8 The runbook is `docs/cutover-runbook.md`. It is written to be followed
with a console open, and it carries the commands rather than describing
them.

§7.9a The deviation pack is `docs/deviation-sign-off.md`. Every Python file
and line it cites was checked against the source when it was written; a
deviation list with a citation a reviewer cannot follow is worse than none,
because it spends the reviewer's trust and returns nothing.

§7.9 **§7.2.3's `messages_sent` counter does not exist.** `MESSAGE_SENT` is
defined in both implementations — `event_logger_lib/log_event.py` and
`ihb-telemetry`'s `EventType` — and is emitted by no component in either.
`04-observability.md` already records it as "Reserved; emitted by no
current component"; §7.2.3 nonetheless asks an operator to watch it.

Read literally, the instruction has an operator judging a cutover by a
number that is zero before, during and after — which is worse than no
signal, because a flat zero looks like a working measurement. The counter
that carries the meaning is `MESSAGE_PROCESSED`: for a sender, that *is*
the delivery. The runbook says so.

---

## §8 Observability during cutover

§8.1 `MICROSERVICE_ID` is **unchanged** between the Python and Rust
revisions of a container, so a Log Analytics query spans the cutover.

§8.2 The revision is distinguishable by the OTel `service.version` resource
attribute and by the container app revision name — not by a changed
`MICROSERVICE_ID`.

§8.3 The alert rules in `Integration-Hub-Beta/docs/ALERT_STRATEGY.md` MUST
continue to fire unchanged. They are written against the names frozen in
[`04-observability.md`](04-observability.md), which is why those names are
frozen.
