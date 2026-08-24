# One image per binary, per spec/18-deployment.md §2.
#
# The binary is chosen with `--build-arg BIN=<name>`, because §2.4 wants one
# image per `[[bin]]` and sixteen near-identical Dockerfiles would drift.
#
#   podman build -f docker/Dockerfile --build-arg BIN=hl7-server -t hl7-server .
#
# §2.1's two stages. The build stage is a full Rust toolchain; the runtime
# stage is distroless, which §2.3 makes possible because
# spec/17-testing-and-quality.md §8.2 forbids openssl — there is no system
# TLS library to link against.

FROM docker.io/library/rust:1-bookworm AS build

ARG BIN
RUN test -n "$BIN" || (echo "build with --build-arg BIN=<binary>" >&2; exit 1)

WORKDIR /src

# Every dependency comes from crates.io. The workspace used to reach two
# sibling repositories through `path`, which no build context could see —
# see spec/18-deployment.md §2.6.
COPY Cargo.toml Cargo.lock ./
COPY xtask/ xtask/
COPY crates/ crates/

# Two binaries compile a file from `local/` into themselves with
# `include_str!`, so the build needs them even though they look like
# development-stack files:
#
#   local/ServiceBusEmulatorConfig.json      buswatch (spec/14 §2.2)
#   local/sample_messages/*.xml              hl7-soap-server
#
# spec/18-deployment.md §2.1's example copies only `crates/`, which builds
# fourteen of the sixteen binaries and fails on those two. See §2.9.
COPY local/ServiceBusEmulatorConfig.json local/
COPY local/sample_messages/ local/sample_messages/

# §2.2 — `--locked`. A build that would change `Cargo.lock` fails, so an
# image can never be built from a dependency set the gate did not run
# against.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin "$BIN" --bin health-probe \
 && cp "target/release/$BIN" /usr/local/bin/service \
 && cp target/release/health-probe /usr/local/bin/health-probe

# ---------------------------------------------------------------------------

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=build /usr/local/bin/service /usr/local/bin/service

# spec/18-deployment.md §3.1's TCP probe, runnable *inside* the container.
# Azure Container Apps probes from the platform and needs nothing here, but
# Compose runs its healthcheck as a command in the container — and a
# distroless image has no shell and no Python to run the Python stack's
# `python -c "…socket…"` equivalent. §5.4 wants a profile healthy with no
# manual step, so the probe ships alongside the service.
COPY --from=build /usr/local/bin/health-probe /usr/local/bin/health-probe

# §1.3 — UID 5678, matching the Python images, so a mounted volume's
# ownership does not change on cutover.
USER 5678:5678

# §1.6 — nothing is written to the image's filesystem at run time.
# The orchestrator mounts /tmp; this image needs no writable path of its own.

# spec/05-health-check.md §2.2 — "In a container the host is `0.0.0.0`;
# `127.0.0.1` is the default only" for a laptop. Setting it here rather than
# leaving it to sixteen container definitions: a health probe bound to
# loopback inside a container is unreachable from the orchestrator, the
# service looks unhealthy while working perfectly, and the platform
# restarts it. An environment variable still overrides this.
ENV HEALTH_CHECK_HOST=0.0.0.0

# §4.2 — there is deliberately NO `COPY ca-certs/*.crt` and no
# `update-ca-certificates` here, which is what the Python images do and
# what makes their base an OS image rather than a distroless one.
#
# The trust roots are the Mozilla set compiled into the binary
# (`webpki-roots`), plus whatever `SSL_CERT_FILE` names at run time — a PEM
# bundle the orchestrator mounts (§4.1). Nothing needs to be baked in, so
# nothing is, and the image keeps no package manager to rebuild a store
# with.
#
# If a corporate CA is needed, mount it and name it. Do not add a
# certificate to this file:
#
#     volumes:
#       - ./ca-certs/corporate.pem:/etc/ssl/corporate.pem:ro
#     environment:
#       SSL_CERT_FILE: /etc/ssl/corporate.pem
#
# A value that cannot be read stops the service at startup (§4.3), so a
# missing mount fails visibly here rather than as a handshake error
# somewhere downstream.
#
# §4.7 — and that is why this line exists. The distroless base presets
#
#     SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
#
# which quietly breaks two things. The service would add the base image's
# ~150 OS certificates to the compiled-in roots, which is the OS trust
# store §4.2 says this image does not use; and, worse, §4.3 could never
# fire, because the variable is already set to a path that reads
# perfectly. An operator who mounted a corporate CA and forgot to name it
# would get a clean startup and a handshake failure later — exactly the
# outcome §4.3 exists to prevent.
#
# Blank is absent (§4.6), so clearing it restores "unset means the
# compiled-in roots, and a set value is the operator's decision".
ENV SSL_CERT_FILE=""

# §1.4 — the binaries install their own SIGTERM handler
# (`ihb_runtime::shutdown`), so the default stop signal is right.
ENTRYPOINT ["/usr/local/bin/service"]
