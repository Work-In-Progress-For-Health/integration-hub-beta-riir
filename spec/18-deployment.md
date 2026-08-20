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

§5.2 The `.env` files are copied from `Integration-Hub-Beta/local/`
**unchanged**. If one needs changing, §1.1 has been violated.

§5.3 `local/ServiceBusEmulatorConfig.json` and
`local/sample_messages/` are copied across unchanged and are the fixtures of
[`17-testing-and-quality.md`](17-testing-and-quality.md) §4.4.1.

§5.4 A profile is started with `podman compose --profile <name> up` and MUST
reach a healthy state with no manual step.

§5.5 The stack MUST be runnable **mixed** — some containers Rust, some
Python — because that is what a cutover looks like. Nothing in either
implementation may assume its neighbour is the same language.

---

## §6 Pipelines

§6.1 `pipeline-ado/` gains a build definition per Rust service, mirroring
the Python's naming (`hl7serverrust-build.yml` beside `hl7server-build.yml`).

§6.2 A build runs the full gate of
[`17-testing-and-quality.md`](17-testing-and-quality.md) §1 before it builds
an image.

§6.3 Release definitions are unchanged: they deploy an image to a container
app, and the image is now built from a different repository.

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
