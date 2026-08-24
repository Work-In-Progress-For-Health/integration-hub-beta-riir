# AGENTS.md

Instructions for any AI agent working in this repository. Short by design
(`spec/agents-directory-name-is-lowercase.md` §3.1); the longer per-area
guides are in [`agents/`](agents/).

---

## The one thing to understand first

**[`spec/`](spec/) is the source of truth.** Code implements a section
there; a section there does not describe code. Where the two disagree, the
specification is right and the code is a defect.

This is not a style preference. This workspace is a rewrite of a live NHS
Wales clinical system, and the specification is what records the behaviour
the Python already has and something outside this workspace depends on —
an exact string, a default, an ordering. Change code to match the spec.
Change the spec only deliberately, and record why.

Every public item's rustdoc names the section it implements, as a **code
span** (`` `§5.4` ``), not an intra-doc link.

---

## Two rules that must never be broken

**Patient data does not go in a log.** Not in a log line, an error message,
a span attribute, a metric dimension, or an HTTP response body. Errors carry
a structural path and a reason, never a value. The message store is the one
place a full payload is kept, because replay needs it.

**Never acknowledge a message that was not delivered.** A failure to persist
is logged and swallowed; a failure to forward stops everything and returns
no acknowledgement, so the source system retries. Backwards, this loses a
clinical message silently — the one failure this system must not produce.

---

## The gate

Every change passes all of this
([`spec/17-testing-and-quality.md`](spec/17-testing-and-quality.md) §1):

```sh
cargo xtask fmt-check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo xtask verify-dictionaries
cargo xtask verify-pipelines
cargo deny check
cargo +"$(cargo xtask msrv)" check --workspace --all-targets
```

`cargo xtask fmt-check`, **not** `cargo fmt --all`. See
[`agents/conventions.md`](agents/conventions.md).

---

## Before you claim something works

This repository has a history of things that compiled, started, reported
healthy, and did nothing. A container whose `main` was `fn main() {}`. A
transformer that never renewed its session lock. Two services connected to
a database that was never created. Each was marked done first.

So: **run it, then say it works.** Prefer evidence over inference, and say
plainly what you did not check. A test that passes for the wrong reason is
worse than a failing one — see
[`spec/17-testing-and-quality.md`](spec/17-testing-and-quality.md) §9.2.1.

---

## Where to look

| Task | Read |
| --- | --- |
| Anything at all | [`spec/index.md`](spec/index.md) |
| Conventions, lints, formatting | [`agents/conventions.md`](agents/conventions.md) |
| Adding or changing a crate | [`agents/workspace.md`](agents/workspace.md) |
| Tests, fixtures, the emulator | [`agents/testing.md`](agents/testing.md) |
| Containers, Compose, pipelines | [`agents/deployment.md`](agents/deployment.md) |
| What is done and what is next | [`tasks.md`](tasks.md) |
| Why the Rust differs from the Python | [`plan.md` §9](plan.md#9-deliberate-deviations-from-the-python) |
