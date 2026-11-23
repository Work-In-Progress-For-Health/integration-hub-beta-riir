# CLAUDE.md

Guidance for [Claude Code](https://claude.com/claude-code) in this
repository.

**Read [`AGENTS.md`](AGENTS.md) first.** It carries the rules that apply to
every task: `spec/` is the source of truth, patient data never reaches a
log, a message is never acknowledged unless it was delivered, and the gate
that every change passes. This file adds only what is specific to working
here interactively.

---

## Build and test

```sh
cargo build --workspace
cargo nextest run --workspace           # unit and integration, in parallel
cargo test  --workspace --doc            # nextest does not run doc tests
cargo test  -p ihb-bus                   # one crate
cargo test  -p dashboard alarm           # one crate, filtered
```

The full suite is over 1,200 tests and takes a few minutes. Test one crate
while iterating; run the workspace before saying anything is done.

Some suites need services and are behind features, so a plain
`cargo test --workspace` never fails for want of a broker:

```sh
podman compose -f local/docker-compose.yml --profile bus up -d

cargo test -p ihb-bus  --features emulator --test emulator -- --test-threads=1
cargo test -p ihb-sql  --features sqlserver
cargo test -p ihb-e2e  --features emulator                 # in-process chain
cargo test -p ihb-e2e  --features compose                  # the containers
```

The emulator's port opens **before** its broker will answer, and it waits on
SQL first. Poll a real connection, never the port
(`spec/17-testing-and-quality.md` §9.2.1e).

---

## Things that will bite

**`cargo fmt --all` is wrong here.** Use `cargo xtask fmt`. The `--all` form
reaches outside this workspace; `xtask` also fails when its member list has
drifted from the manifest, which is how a newly added crate gets noticed.

**`compose up` reuses an existing image.** A service whose image is already
built (`ihb-rs/<bin>:dev`) is not rebuilt when its code changes, so a compose
test can fail on a bug you have already fixed. Rebuild the one image
(`podman build -f docker/Dockerfile --build-arg BIN=<bin> -t ihb-rs/<bin>:dev .`)
or run `docker/build-all.sh`. It cost a 7-minute run when the `wpas-to-proms`
test was added.

**Podman, not Docker** (`spec/18-deployment.md` §5.0). On macOS the Podman
VM only shares `$HOME`, so a bind mount from `/tmp` fails with
`statfs: no such file or directory`.

**The Service Bus emulator has a hard connection quota**
(`spec/14-buswatch.md` §6.1). Bringing up several Compose profiles at once
exhausts it and the broker then refuses new connections with an error that
names none of this. Use one profile at a time.

**A cold three-hop flow takes about four minutes.** Each hop waits out its
own sixty-second receive window on an idle queue. Do not read a slow flow as
a broken one.

**`local/*.env` files quote their values.** Compose strips the quotes;
`podman run --env-file` does not, so a value arrives as `"2576"` and fails
to parse. That is a quirk of the tool, not a defect in the config.

---

## Working style that fits this repository

**Comments say why, not what.** The code says what. Nearly every non-obvious
line here carries the reason it is that way, often with the measurement or
the failure that motivated it. Match that; do not strip it.

**Cite the specification** in rustdoc and in tests, as a code span. A test
named after the clause it protects is how a future reader knows whether it
may be deleted.

**When you find a defect, record it.** Add the clause to the relevant
`spec/` section, with the evidence. This repository's specification has
grown by exactly this route, and the clauses that came from a real failure
are the most valuable ones in it.

**Do not mark work done that you have not run.** See AGENTS.md.

---

## Layout

```
spec/          the normative specification — start here
agents/        longer per-area guides for agents
docs/          operational documents (cutover runbook, deviation sign-off)
crates/        26 crates; see agents/workspace.md
xtask/         workspace automation
local/         Compose stack, .env files, sample messages
docker/        one parameterised Dockerfile for every service
pipeline-ado/  Azure DevOps build definitions
fuzz/          fuzz targets (own workspace; needs nightly)
```
