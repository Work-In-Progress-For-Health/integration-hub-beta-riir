# Rust MSRV — current minus three

The Minimum Supported Rust Version for this workspace is **the current
stable release minus three**.

At the time of writing, stable is **1.98**, so the MSRV is **1.95**.

---

## §1 The rule

§1.1 The MSRV is `N - 3`, where `N` is the current stable Rust release.

§1.2 It is a **floor, not a target**. Development happens on current stable;
the MSRV says what the workspace is still *guaranteed* to build on.

§1.3 It MUST be recorded in exactly one place — `rust-version` under
`[workspace.package]` in the root `Cargo.toml` — and inherited by every
member with `rust-version.workspace = true`. A crate MUST NOT declare its
own.

§1.4 It MUST be enforced by CI, not by intention
([`17-testing-and-quality.md`](17-testing-and-quality.md) §1.4). An MSRV
nobody checks is a comment.

---

## §2 Why three

Three releases is about **eighteen weeks**, Rust shipping every six.

That is long enough to matter to the people who have to consume this:

- A distribution or a base image that lags a release or two still works.
- A developer who has not run `rustup update` this month is not blocked.
- An Azure DevOps agent pinned by someone else's schedule keeps building.

And short enough not to cost much: three releases back is recent enough that
`let`-chains, `let ... else`, and the 2024 edition are all available, so the
code does not have to be written in an older dialect to satisfy it.

§2.1 It is deliberately **not** "whatever happens to compile". A floor that
drifts upward silently, one convenient API at a time, is not a floor. When
raising it is genuinely worth it, §4 says how.

---

## §3 What the rule permits

§3.1 The 2024 edition, which needs 1.85.

§3.2 `let`-chains (`if let Some(x) = a && x > 1`), stable since 1.88.

§3.3 `#[expect(...)]` and `#[allow(..., reason = "...")]`, stable since
1.81.

§3.4 Anything else stabilised at or before the MSRV. The check in §1.4 is
what settles the question; nobody should be reading release notes to find
out.

---

## §4 Raising it

§4.1 The MSRV moves when the rule moves — that is, when stable advances far
enough that `N - 3` is higher than what is recorded. Following the rule is
the normal reason to raise it, and needs no argument.

§4.2 Raising it **ahead** of the rule, to reach a specific feature, is a
deliberate change and MUST be recorded here with what it bought.

§4.3 Lowering it below `N - 3` is not a thing this workspace does. A
consumer stuck further back than eighteen weeks has a problem the workspace
cannot fix by contorting itself.

---

## §5 History

| Date | MSRV | Stable at the time | Why |
| --- | --- | --- | --- |
| 2026-08-20 | 1.95 | 1.98 | The rule, applied at the point it was written down. |
