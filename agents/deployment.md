# Deployment

`spec/18-deployment.md` is normative. This is the working guide.

---

## Images

**One `Dockerfile`** for every service, parameterised by the binary:

```sh
podman build --build-arg BIN=hl7-server -t ihb-rs/hl7-server:dev -f docker/Dockerfile .
./docker/build-all.sh                     # all of them
```

Two-stage, distroless (`gcr.io/distroless/cc-debian12:nonroot`), UID 5678 to
match the Python images so a mounted volume's ownership does not change on
cutover. Each image also carries `health-probe`, because a distroless image
has no shell for Compose's healthcheck to use.

**Podman, not Docker** (§5.0). On macOS the Podman VM shares only `$HOME`,
so a bind mount from `/tmp` fails with `statfs: no such file or directory`.

## Certificates

Trust is the compiled-in Mozilla root set **plus** an optional bundle named
by `SSL_CERT_FILE` — *additional*, not a replacement, which is the opposite
of OpenSSL's meaning for that variable (§4.4).

A configured bundle that cannot be read, or holds no usable certificate, is
a **startup failure** (§4.3). `ihb_tls::startup_check` enforces it in every
service.

The image clears `SSL_CERT_FILE`, because the distroless base presets it and
that would make §4.3 unfireable — a forgotten CA mount would start cleanly
and fail at handshake time instead (§4.7).

## The local stack

```sh
podman compose -f local/docker-compose.yml --profile phw-to-mpi up -d
```

21 services, 9 profiles. Two things to know:

**One profile at a time.** The emulator has a hard connection quota
(`spec/14-buswatch.md` §6.1); several profiles at once exhaust it and the
broker then refuses new connections with an error that explains none of
this.

**`local/*.env` quotes its values.** Compose strips the quotes;
`podman run --env-file` does not, so `PORT="2576"` arrives with the quotes
and fails to parse.

The stack also runs **mixed** — some containers Rust, some Python — which is
what a cutover looks like:

```sh
podman compose -f local/docker-compose.yml -f local/docker-compose.mixed.yml \
  --profile phw-to-mpi up -d
```

## Pipelines

`pipeline-ado/` holds one build definition per service, plus `build-apps.yml`,
`pr-validation.yml` and two templates.

The contract that keeps release definitions unchanged (§6.3, §6.4): the ACR
repository name, the `imageTag` artefact, and the **ADO pipeline name** all
stay identical. Cutover is repointing the named pipeline at this repository —
which is why the `rust` suffix is on the *file*, never on the image.

`cargo xtask verify-pipelines` holds the definitions to the workspace, and
catches the silent failure: a deployable binary with no build definition
never ships and nothing else notices.

## Cutover

Two documents, both written to be followed rather than read:

- [`../docs/cutover-runbook.md`](../docs/cutover-runbook.md) — per-container
  procedure, real `az` commands, verification queries, rollback.
- [`../docs/deviation-sign-off.md`](../docs/deviation-sign-off.md) — the 13
  deviations, each with a Decision row the Hub team fills in.

Three things the runbook records that the specification did not anticipate:
the release pipeline cannot deploy "alongside taking no traffic" without a
mode change (§7.5); half the containers have no ingress to weight at all
(§7.6); and §7.2.3's `messages_sent` counter is emitted by nothing, in
either implementation (§7.9).
