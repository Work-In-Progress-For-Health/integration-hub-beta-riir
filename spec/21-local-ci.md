# 21 — Local CI

How much of the gate runs on a developer's machine, how that run is
recorded, and where it must stop.

The evidence, the options considered and the options rejected are in
[`docs/local-ci-assessment.md`](../docs/local-ci-assessment.md). This file
holds only what the implementation MUST do. It extends
[`17-testing-and-quality.md`](17-testing-and-quality.md) §9 and does not
change what the gate *is*; it changes where it can run and how a run is
evidenced.

---

## §1 One definition of the gate

§1.1 There MUST be exactly one definition of the gate, and it MUST be code:
`cargo xtask gate`. Git hooks, a developer at a terminal, the hosted
workflow and any future runner all call it. None of them lists the steps.

§1.2 This replaces three hand-maintained lists that already disagree
(`17-testing-and-quality.md` §1, `.github/workflows/gate.yml`, `AGENTS.md`):
the first passes `--all-features` and omits `verify-pipelines`, the second
does the reverse, and the third differs from both. A list in prose or YAML
cannot be checked against the others; a task can.

§1.3 `cargo xtask gate` MUST run its steps in `17-testing-and-quality.md`
§1's order (cheapest first), MUST stop at the first failure by default, and
MUST accept `--keep-going` to report every failing step in one run.

§1.4 The task MUST use `cargo xtask fmt-check`, never `cargo fmt --all`
(`17-testing-and-quality.md` §1.0).

## §2 Tiers

§2.1 The gate has tiers, so a developer can run the cheap ones often and the
expensive ones when it matters:

| Tier | Name | Runs | Needs | Typical time |
| --- | --- | --- | --- | --- |
| 0 | `fast` | `fmt-check` | nothing | seconds |
| 1 | `gate` | §1's steps, `cargo doc`, `verify-dictionaries`, `verify-pipelines`, `cargo deny`; MSRV `check` when `Cargo.toml`, `Cargo.lock` or `rust-toolchain.toml` changed since the merge base | a toolchain | minutes |
| 2 | `stack` | the `emulator` and `sqlserver` feature tests (`17-testing-and-quality.md` §9.2) | Podman; the `bus` Compose profile | ~9 min |
| 3 | `flows` | each `ihb-e2e` flow, **one Compose profile at a time** | Podman | ~4 min per flow, cold |

Times are measurements from 2026-11-04 on an M4 Max under load, not
requirements.

§2.2 Tier 3 MUST run profiles sequentially. The Service Bus emulator has a
hard connection quota (`14-buswatch.md` §6.1); several profiles at once
exhaust it and the broker then refuses new connections with an error that
names none of this.

§2.3 Tier 1 MUST NOT depend on tier 2 or 3. A machine without Podman can
still run the gate; the record (§3) says which tiers ran, so a missing tier
is visible rather than silently green.

## §3 The evidence of a run

§3.1 Every run of tier 1 or above MUST write
`target/gate/<commit-sha>.json` containing at least: the commit sha; whether
the working tree was clean (a dirty tree MUST be recorded as dirty and MUST
NOT be posted as a status); each tier attempted with its result and duration;
the toolchain and MSRV versions; the host OS and architecture; and the time.

§3.2 The file MUST contain no message content, no connection string and no
secret. The `sqlserver` tier reads `MSSQL_SA_PASSWORD`; it MUST NOT be
echoed into the record or the log.

§3.3 `cargo xtask gate --post` MAY publish the result as a GitHub commit
status named `local/<tier>` (`gh api repos/{owner}/{repo}/statuses/{sha}`).
It MUST refuse when the tree is dirty, when the commit is not the one
checked out, or when the tier failed.

§3.4 **A local status is self-attested.** Anyone holding a token can post
one. It is therefore evidence for a small trusted team and MUST NOT be the
only control on a release branch: the hosted `gate` job stays required until
the local record has a history that justifies relaxing it, and that decision
is a deliberate change to this clause, not a drift.

## §4 What stays hosted

§4.1 **A release artifact MUST NOT be built on a developer machine.** Images
that reach the Azure Container Apps estate are built by the Azure DevOps
pipelines (`18-deployment.md` §6) in a clean, attributable environment.
Local CI replaces the *gate*, not the *release*.

§4.2 Local CI does not change `pipeline-ado/` or `cargo xtask
verify-pipelines`.

§4.3 Hosted workflows SHOULD run on pull requests and pushes to the default
branch, not on every push to every branch, once the local record exists: the
local gate has already run on the commit the developer is pushing.

## §5 Self-hosted runners

§5.1 A self-hosted runner MUST NOT be registered against a repository that
accepts pull requests from forks, which includes every public repository. A
workflow run from a fork executes the contributor's code on the runner's
machine.

§5.2 Where a self-hosted runner is used on a private repository it SHOULD be
ephemeral, and it MUST NOT hold credentials beyond the job's own.

§5.3 A design MUST NOT depend on self-hosted runner minutes being free: the
announced $0.002 per minute charge was postponed, not withdrawn, and has no
new date.

## §6 The container runtime

§6.1 The stack helpers MUST wait on a real connection, never on a port
(`17-testing-and-quality.md` §9.2.1e).

§6.2 A helper that starts Compose MUST NOT assume `podman compose` works. On
the machine this was written on it fails because `podman machine list`
reports an unknown machine state; the standalone `podman-compose` works.
The helper MUST detect which is usable, and say which it used.

§6.3 `podman-compose` rewrites a single-argument `["CMD", "<path>"]`
healthcheck to `CMD-SHELL`. The images are distroless and have no shell, so
every service then reports unhealthy. A helper that waits on container
health under `podman-compose` MUST wait on a connection instead (§6.1) or
the healthcheck MUST be written so it survives the rewrite.

## §7 Hooks

§7.1 `cargo xtask install-hooks` MUST set `core.hooksPath` to a committed
directory (`.githooks/`) whose scripts only call `cargo xtask`, and MUST
print what it changed. It MUST NOT modify global git configuration.

§7.2 The pre-commit hook runs tier 0. The pre-push hook runs tier 1.

§7.3 `git push --no-verify` remains available. The consequence is that no
`local/gate` status exists for that commit (§3), which is visible, rather than
the hook pretending to be a barrier it is not.

## §8 Defects this specification records

§8.1 **The hosted `emulator-backed` job has never passed.** In every run
inspected on 2026-11-04 (five of the repository's ten) it fails at
`Start the local stack`: `podman compose` finds no Podman socket on the
runner (`unix:///run/user/1001/podman/podman.sock`). The emulator tier has
therefore had no hosted coverage. The fix is to start the user socket
(`systemctl --user enable --now podman.socket`) or to use the Docker Compose
that ships on the runner; either MUST be proven by a hosted run before this
clause is closed.

§8.2 The three gate lists of §1.2 disagree. Closed when `gate.yml` and
`AGENTS.md` call `cargo xtask gate` and `17-testing-and-quality.md` §1 points
here.

§8.3 One hosted `gate` run (24 August 2026) ran for 21,616 seconds before it
was cancelled. A hosted job SHOULD have a `timeout-minutes` well under the
default, so a hang costs minutes rather than six hours.

## §9 Tests

§9.1 `cargo xtask gate` MUST have a test that its step list equals the list
`gate.yml` would run, so §1.2's drift cannot return. (If the workflow only
calls the task, this is the test that it does.)

§9.2 The record of §3 MUST be tested for: dirty tree refused for posting;
no secret in the file; a failing tier recorded as failing.
