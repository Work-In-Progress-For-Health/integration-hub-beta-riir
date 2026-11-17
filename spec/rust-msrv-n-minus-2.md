# The minimum supported Rust version

**Normative.** Referenced by
[`17-testing-and-quality.md`](17-testing-and-quality.md) §1.4 and
implemented by `cargo xtask msrv`. This is the only document on the MSRV;
an earlier copy (`spec/rust-msrv-n-minus-3/`) disagreed with it on the
current number and has been folded in here.

---

## §1 The policy

§1.1 The minimum supported Rust version is **current stable minus two**
(`N - 2`). At the time of writing stable is **1.100**, so the MSRV is
**1.98**. The policy is a *rule*, not a version; the number is a
consequence of it and is recorded in §5.

§1.2 It is a **floor, not a target**. Development happens on current
stable; the MSRV says what the workspace is still guaranteed to build on.
Two releases is about **twelve weeks**, Rust shipping every six: long enough
that a base image or a build agent that lags a release is not broken, short
enough that the code may use whatever has been stable for a season
(`let`-chains, `let ... else`, `#[expect]`, the 2024 edition) without
writing an older dialect.

§1.3 **The MSRV MUST be declared once**, as `rust-version` under
`[workspace.package]` in the root `Cargo.toml`, and inherited by every
member with `rust-version.workspace = true`; a crate MUST NOT declare its
own:

```toml
[workspace.package]
rust-version = "1.98"
```

Once, and in that table. A version written a second time — in a CI file, in
a README, in a `rust-toolchain.toml` — is a copy, and a copy goes stale the
first time the floor moves. `cargo xtask msrv` reads the declaration and
prints it, so the check runs against what is declared rather than beside it:

```sh
rustup toolchain install "$(cargo xtask msrv)"
cargo +"$(cargo xtask msrv)" check --workspace --all-targets
```

§1.3.1 `cargo xtask msrv` fails if the key is absent, rather than guessing a
version. A missing floor is a configuration error and is reported as one.

§1.4 **The floor is checked, not intended.** The gate compiles the whole
workspace against the declared toolchain
([`17-testing-and-quality.md`](17-testing-and-quality.md) §1.4; the hosted
`msrv` job and tier 1 when a toolchain input changed,
[`21-local-ci.md`](21-local-ci.md) §2.1). A dependency that raises its own
floor raises this one silently, and an MSRV nobody compiles against is a
comment.

§1.5 The check is `cargo check`, not `cargo test`: §1.4 is about the floor
**compiling**, and running the suite twice buys nothing. The suite runs on
stable, once.

§1.6 It is deliberately **not** "whatever happens to compile". A floor that
drifts upward silently, one convenient API at a time, is not a floor.

---

## §2 Moving it

§2.1 Moving the floor is a change to `rust-version` and nothing else. No CI
file, README or workflow needs editing, which is the point of §1.3. Add a
row to §5.

§2.2 It moves when the rule moves, that is, when stable has advanced far
enough that `N - 2` is higher than what is recorded; that needs no
argument. Raising it **ahead** of the rule, to reach a specific feature, is
a deliberate change and MUST say in §5 what it bought. Lowering it below
`N - 2` is not something this workspace does.

§2.3 A dependency raising its floor past `N - 2` is a conflict between this
policy and that dependency. Pin the dependency, or move the floor and record
why. Do not silently accept a floor newer than the policy allows. (Cargo's
MSRV-aware resolver already holds a dependency back to the declared floor:
`cargo update` says "highest Rust 1.98 compatible versions".)

---

## §3 What the rule permits

The 2024 edition (1.85); `let`-chains (1.88); `#[expect(...)]` and
`#[allow(..., reason = "...")]` (1.81); anything else stabilised at or
before the MSRV. The check in §1.4 settles the question; nobody should be
reading release notes to find out.

---

## §5 History

| Date | MSRV | Stable at the time | Why |
| --- | --- | --- | --- |
| 2026-08-20 | 1.95 | 1.98 | `N - 3`, the rule as first written. |
| 2026-11-17 | 1.98 | 1.100 | Policy changed to `N - 2` on the owner's instruction; the floor had also fallen to `N - 5` because nothing moved it when stable did. |
