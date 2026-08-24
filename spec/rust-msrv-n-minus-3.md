# The minimum supported Rust version

**Normative.** Referenced by
[`17-testing-and-quality.md`](17-testing-and-quality.md) §1.4 and
implemented by `cargo xtask msrv`.

---

## §1 The policy

§1.1 The minimum supported Rust version is **current stable minus three**.

At the time of writing that is **1.95**. It moves when stable moves, which
is every six weeks — the policy is a *rule*, not a version, and the number
below it is a consequence.

§1.2 Three releases is about eighteen weeks. That is chosen to be long
enough that nobody is forced to upgrade a toolchain to build this workspace
in a hurry, and short enough that the code may use language features that
have been stable for a season. `let`-chains and `let`-else both arrived
under this policy and are used freely.

§1.3 **The MSRV MUST be declared once**, as `rust-version` under
`[workspace.package]` in the root `Cargo.toml`:

```toml
[workspace.package]
rust-version = "1.95"
```

Once, and in that table. A version written a second time — in a CI file, in
a README, in a `rust-toolchain.toml` — is a copy, and a copy is a thing
that goes stale the first time the floor moves. `cargo xtask msrv` reads
the declaration and prints it, so the check runs against what is declared
rather than beside it:

```sh
rustup toolchain install "$(cargo xtask msrv)"
cargo +"$(cargo xtask msrv)" check --workspace --all-targets
```

§1.3.1 `cargo xtask msrv` fails if the key is absent, rather than guessing
a version. A missing floor is a configuration error and is reported as one.

§1.4 **The floor is checked, not intended.** The gate compiles the whole
workspace against the declared toolchain
([`17-testing-and-quality.md`](17-testing-and-quality.md) §1.4). A
dependency that raises its own floor raises this one silently, and an MSRV
nobody compiles against is a comment rather than a guarantee.

§1.5 The check is `cargo check`, not `cargo test`. §1.4 is about the floor
**compiling**; running the suite twice buys nothing and doubles the wall
clock. The suite runs on stable, once.

---

## §2 Raising it

§2.1 Raising the floor is a change to `rust-version` and nothing else. No
CI file, README or workflow needs editing, which is the point of §1.3.

§2.2 It is raised for a reason that is written down — a dependency that
now requires it, or a language feature worth the cost — and not merely
because stable moved. The policy sets a *ceiling* on how old the floor may
be, not a schedule for raising it.

§2.3 A dependency raising its floor past current-stable-minus-three is a
conflict between this policy and that dependency. Pin the dependency, or
move the floor and record why. Do not silently accept a floor newer than
the policy allows.
