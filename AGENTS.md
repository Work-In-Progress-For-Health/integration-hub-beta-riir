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

**Arithmetic on an external figure never panics, wraps, saturates or invents
a number, and invalid input is an error, never repaired.** A result that does
not fit is `None` or an error, handled where it happens; a fallback is used
only where a `spec/` clause declares it. A value that is out of range or
malformed is refused, naming the figure, not clamped, defaulted or replaced.
Use `checked_*`, `get`, `split_at_checked` and `try_from`, not `+`, `[..]`
or `as`. The gate enforces it (`spec/17` §2.4a); the exceptions and the open
items are in `docs/checked-arithmetic-audit.md`.

---

## The gate

Every change passes `cargo xtask gate`
([`spec/21-local-ci.md`](spec/21-local-ci.md) §1, the one definition; the
steps are in `xtask/src/gate.rs`, and the hosted workflow calls the same
task):

```sh
cargo xtask gate                  # tier 1: fmt-check, clippy,
                                  # checked-arithmetic, test (nextest), doctest, doc,
                                  # verify-dictionaries, verify-pipelines, deny;
                                  # the MSRV build too when Cargo.toml,
                                  # Cargo.lock or rust-toolchain.toml changed
cargo xtask gate --keep-going     # report every failing step, not the first
cargo xtask gate --tier fast      # formatting only, seconds
cargo xtask gate --tier stack     # needs the bus stack running
cargo xtask gate --tier flows     # Compose profiles, one at a time
```

Run the **exact** command, not a filtered copy of it: a clippy run whose
output was cut with `head` once hid an error the hosted gate then found.

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
