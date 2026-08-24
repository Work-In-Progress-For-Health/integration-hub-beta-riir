# Conventions

How code in this workspace is written. The rules that surprise people are
first.

---

## Formatting

**`cargo xtask fmt`, never `cargo fmt --all`.**

`--all` reaches beyond this workspace. It also once rewrote files in
separate repositories that were then path dependencies — the dependencies
have since moved to crates.io, but the habit is kept because `xtask` does
something `cargo fmt` cannot: it fails when its member list has drifted
from the manifest's. That is how a newly added crate gets noticed instead of
sitting silently unformatted.

```sh
cargo xtask fmt          # format this workspace
cargo xtask fmt-check    # check, changing nothing
```

## Lints

Declared once in the root `Cargo.toml` and inherited by every member with
`[lints] workspace = true`. The set is deliberately aggressive:

- `clippy::pedantic`, `clippy::nursery`, `clippy::cargo`
- `missing_docs` **and** `missing_docs_in_private_items` — everything is
  documented, public or not
- `unsafe_code = "forbid"`
- `unwrap_used`, `expect_used`, `panic` — warnings, so tests may use them
  freely; CI's `-D warnings` means an unjustified one in non-test code fails
- `todo`, `dbg_macro` — denied

A test module opts out at the crate root, which is the established idiom:

```rust
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
```

Every `#[allow]` in non-test code carries a comment saying why
(`spec/17-testing-and-quality.md` §2.3).

## Documentation

**Cite the specification as a code span, not an intra-doc link.**

```rust
/// Deliver `message` and evaluate what came back (`spec/10-senders.md` §3.4).
```

Not `[`§3.4`]` — rustdoc would try to resolve it as an item and fail the
`-D warnings` doc build.

**Comments say why, not what.** The code says what. Nearly every non-obvious
line here records the reason it is that way, and often the measurement or
the failure that motivated it. That is the most valuable content in the
repository; do not strip it when refactoring.

**Module docs earn their length.** A crate or module doc that explains the
two or three rules a reader needs before reading anything else is worth
fifty lines. `ihb-config`, `ihb-tls` and `ihb-bus` are the models.

## Errors

Errors carry a **structural path and a reason, never a value**. A patient
identifier, a payload, a connection string or a password must not appear in
any error, at any level (`spec/12-message-store.md` §5.7,
`spec/04-observability.md` §2).

Name the file or entity in the message. "Invalid certificate" without a path
is the least useful thing a deployment can fail with.

`thiserror` for libraries. A variant per distinguishable cause, because the
caller usually needs to tell them apart.

## Async

`async fn` in traits (AFIT) is used, which means those traits are **not
dyn-compatible**. Seams are generic over the trait rather than boxed. Where
a seam must be dynamic, it takes an owned value and an `async move` block —
a borrowed iterator plus AFIT hits a higher-ranked lifetime error that has
no clean workaround.

## Configuration

Every service is configured by environment variables and nothing else.
Three rules hold everywhere (`spec/02-configuration.md` §1):

- **Blank is absent.** `FOO=` and `FOO="   "` mean unset, for every type.
- **Every problem is reported at once**, not the first one.
- **A secret never prints.** `Secret` renders as `<redacted>` through both
  `Debug` and `Display`.

Configuration is parsed **completely, before a socket is bound**. A
misconfigured container fails to start rather than failing later on the
first message that needed the bad value.
