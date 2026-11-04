# Local CI/CD — assessment and advice

Written 2026-11-04. Normative requirements that follow from this are in
[`spec/21-local-ci.md`](../spec/21-local-ci.md); the work is
[`tasks.md`](../tasks.md) Phase 10.

The question was how to do more of the checking locally rather than relying
on GitHub, because it is felt to be unreliable and to cost money. This
document says what the evidence supports, what it does not, and what to do.

---

## 1. What the evidence says about the premise

**Cost.** This repository is **public**
(`gh repo view` → `PUBLIC`, owner `Work-In-Progress-For-Health`). Standard
GitHub-hosted runners are free for public repositories, so for *this*
repository the cost premise does not hold today. (That a public repo's
hosted minutes are free is GitHub's long-standing policy; I did not
re-verify it against the current billing page this session.) Cost does
apply where the work is private: the upstream Python estate and its Azure
DevOps pipelines.

GitHub announced a $0.002/minute platform charge on **self-hosted** runners
for 1 March 2026 and then postponed it. GitHub's own page says: "We're
postponing the announced billing change for self-hosted GitHub Actions to
take time to re-evaluate our approach", with no new date
([GitHub](https://github.com/resources/insights/2026-pricing-changes-for-github-actions)).
Several articles still describe it as in force; those are wrong on the
primary source. It may return, so a design that *depends* on free
self-hosted runners is exposed to it.

**Reliability.** The repository has ten workflow runs (eight failed, two
cancelled). I inspected five of them in detail (the three August runs, the first run of
this branch, and the latest), and the evidence is more specific
than "unreliable":

| Job | Result in the runs inspected | Time |
| --- | --- | --- |
| `gate` (spec/17 §1) | succeeded on the latest head and on the first branch run | 374 s |
| `msrv` (spec/17 §1.4) | succeeded on both, and on the third August run | 79–89 s |
| `emulator-backed` (spec/17 §9.2) | **failed in every run inspected (all five)** | 10–20 s |

Two of the August runs failed every job at a "check out the sibling crate
repositories" step that no longer exists, and one `gate` run (24 August) ran
for 21,616 s, about six hours, before being cancelled: a genuine hosted-runner
hang, and the only reliability fault in the record that is not our own
configuration. Every run since fails in exactly one place: **`Start the local stack`**, where `podman compose` finds no
Podman socket on the runner (`failed to connect to the docker API at
unix:///run/user/1001/podman/podman.sock`). That is a defect in our workflow
file, not GitHub being unreliable, and it means the emulator tier has had **no
hosted coverage in any of the five runs inspected** (the other five were
not opened; two were cancelled). Recorded as
[`spec/21`](../spec/21-local-ci.md) §8.1.

So the honest summary is: GitHub is not the problem here; the checks that
matter most are the ones GitHub is not running, and they are the ones a local
run is best placed to cover.

## 2. What running locally buys, with measurements

Measured on an Apple M4 Max (16 cores, 128 GB) on 2026-11-04, **while
several agents were building in the same target directory**, so the
cargo-bound figures are pessimistic.

| Step | Local | Hosted |
| --- | --- | --- |
| `cargo clippy --workspace --all-targets` | ~1 m 25 s – 3 m 45 s | within the 374 s `gate` |
| `cargo test --workspace` (≈2,070 tests) | ~9–10 min (contended) | within the 374 s `gate` |
| MSRV `check` | ~6 min (contended, cold toolchain) | 79–89 s |
| `ihb-bus` emulator suite (8 tests) | 506 s | never ran |
| One cold end-to-end flow (`lims-to-mpi`) | 222 s | never ran |

Two conclusions. First, **the laptop is not meaningfully faster than a hosted
runner for the cargo gate** (and is slower for MSRV, which a fresh runner
caches well); speed is not the reason to go local. Second, the emulator and
end-to-end tiers take 4–9 minutes each and need containers, a Service Bus
emulator connection quota (one Compose profile at a time,
`spec/14-buswatch.md` §6.1), and a warm machine. Those are the tiers where a
machine you control wins on **availability and cost per run**, and they are
the ones with no coverage today.

## 3. Options

| Option | Fit | Verdict |
| --- | --- | --- |
| **`cargo xtask gate` as the single definition**, called by git hooks, by humans, and by every CI | Already how this repo works (`xtask fmt-check`, `verify-*`, `msrv`). Removes the drift below. No new dependency | **Do first** |
| **Git hooks** (`core.hooksPath`, calling xtask) | Plain git, zero dependencies; catches failures before a push is spent | **Do** |
| **Local run posts a commit status** (`gh api`) that branch protection can require | Merge gated on evidence from a machine you control, no hosted minutes | **Do, with the caveats in §4** |
| Scheduled local runs of the heavy tiers (launchd) | Gives the emulator and flow tiers a nightly record | **Do** |
| **`cargo-nextest`** (installed here) | Per-test processes remove the process-global-state coupling `spec/17` §9.2.1a describes; JUnit output. **Does not run doctests**, so `cargo test --doc` stays | **Trial**, measure on this workspace before adopting; a vendor benchmark's gain was measured with a warm `sccache`, so it is not attributable to nextest alone ([Depot](https://depot.dev/blog/guide-to-faster-rust-builds-in-ci)) |
| GitHub self-hosted runner on a local machine | Keeps `gate.yml` unchanged | **Only on a private repo.** On a public repo that accepts fork pull requests it runs strangers' code on your machine. Exposed to the postponed fee |
| `act` | Runs workflow files locally in containers | **No.** The emulator job needs Podman inside the job; `act` runs Docker-API containers with images lacking much of the hosted toolset ([act](https://forge.yumechi.jp/yume/act/raw/branch/main/README.md)). Low value over xtask |
| Dagger | One pipeline definition runs locally and in CI; Rust SDK exists (`dagger-sdk` 0.21.x, [docs.rs](https://docs.rs/crate/dagger-sdk/latest/source/examples/)) | **Not now.** A large engine dependency to re-express what `xtask` already does; I did not verify its Podman support. Revisit if `xtask` becomes unwieldy |
| Forgejo / Gitea Actions + a runner | GitHub-compatible workflows, act-derived ([summary](https://nodatools.pages.dev/blog/self-hosted-ci-alternative-to-github-actions)); a forge to run and patch | **No**, unless the repository leaves GitHub. Operating a forge is a cost in its own right |
| Woodpecker | Standalone CI server | **No.** Heavier than Forgejo's runner for no gain here |
| Azure DevOps self-hosted agent | Free tier is 1 Microsoft-hosted job (1,800 min/month) plus 1 self-hosted job with unlimited minutes; extra jobs are $40 hosted / $15 self-hosted per month ([Microsoft](https://azure.microsoft.com/pricing/details/devops/)) | **The real answer to cost on the private estate**, if ADO minutes are the pain. Not verified against this organisation's plan |

`just` is installed but not needed: the upstream Python used a `justfile`;
this repository already has `cargo xtask`, and a second task runner would
create a second place for the gate to drift.

## 4. Where local checking must stop

This is a rewrite of a live clinical system, so two limits are firm.

1. **Release artifacts do not come from a developer machine.** A container
   image that reaches the Azure Container Apps estate must be built in a
   clean, attributable environment, which is what the ADO pipelines are for
   (`spec/18-deployment.md` §6). Local CI replaces the *gate*, never the
   *release*. It also avoids an architecture question I have not answered:
   whether the estate runs `amd64` while this machine is `arm64`.
2. **A local status is self-attested.** Anyone with a token can post
   `local/gate = success`. That is acceptable for a small trusted team if the
   record carries the commit, a clean-tree flag, tool versions and timings
   (so a reader can tell a real run from a claim), and if the hosted `gate`
   job stays as an independent check until the local record has earned trust.
   It is not acceptable as the only control on a release branch.

## 5. Defects found while researching

1. **The `emulator-backed` job has never passed.** `podman compose` on
   `ubuntu-latest` cannot reach a Podman socket. Fix: start the user socket
   (`systemctl --user enable --now podman.socket`), or use the Docker
   Compose that ships on the runner. Not yet fixed; a hosted run is needed to
   prove it.
2. **Three lists of "the gate" disagree.** `spec/17` §1 runs clippy and tests
   with `--all-features` and omits `verify-pipelines`; `gate.yml` omits
   `--all-features` (deliberately, with a comment) and includes
   `verify-pipelines`; `AGENTS.md` has a third form. Each is defensible; that
   they differ is the argument for one definition in code.
3. **`podman compose` is unusable on this machine** (`podman machine list` →
   "unknown machine state"); the standalone `podman-compose` works but turns
   a single-argument `CMD` healthcheck into `CMD-SHELL`, which the
   distroless images cannot run, so every service reports unhealthy.
   Both are local-environment facts the helper has to account for
   ([`spec/21`](../spec/21-local-ci.md) §6).

## 6. Recommendation, in order

1. Fix the hosted emulator job (§5.1). Cheapest, and it immediately gives the
   most important tier a record.
2. Build `cargo xtask gate` and make `gate.yml` and the hooks call it (§5.2).
3. Add `cargo xtask install-hooks` (pre-commit: `fmt-check`; pre-push: the
   gate).
4. Add the local run's evidence file and commit status; require it alongside,
   not instead of, the hosted `gate`.
5. Add a scheduled local run of the stack and flow tiers.
6. Trial `cargo-nextest`.
7. Only if hosted cost on the private estate is the real pain: a self-hosted
   ADO agent, after confirming the organisation's plan and the `amd64`
   question.

Not recommended: `act`, Dagger, Forgejo, Woodpecker, or a self-hosted GitHub
runner on this public repository.

## 7. What I did not verify

- GitHub's current per-minute rates and included minutes for private
  repositories (the page I fetched did not state them).
- That the hosted free-for-public-repos policy is unchanged.
- Dagger's and `act`'s behaviour under Podman; both are judged from
  documentation, not run.
- This organisation's Azure DevOps plan and agent architecture.
- Whether a local commit status can satisfy this repository's branch
  protection (the rule may not exist yet; `gate.yml` says protection is "not
  this file's job").
