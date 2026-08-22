# serde_json float_roundtrip arbitrary_precision

When using Rust crate serde_json, use serde_json crate features:

- "float_roundtrip" makes f64 -> JSON -> f64 produce output identical to the input.
- "arbitrary_precision" makes JSON -> serde_json::Number -> JSON produce output identical to the input.

Example in file `Cargo.toml` dependencies:

```toml
serde_json = { version = "…", features = ["float_roundtrip", "arbitrary_precision"] }
```

## Measured caveat: exponent spelling

`arbitrary_precision` keeps every **significant digit** — nothing is
rounded, and a value beyond `f64`'s range survives instead of becoming
`inf` and then `null`. It does normalise how an exponent is *spelled*:

| Input | Output | |
| --- | --- | --- |
| `1e400` | `1e+400` | normalised |
| `1E400` | `1e+400` | normalised |
| `1.5e3` | `1.5e+3` | normalised |
| `1e+400` | `1e+400` | identical |
| `1.50` | `1.50` | identical |
| `-0.0` | `-0.0` | identical |
| `12345678901234567890123456789` | *(unchanged)* | identical |

So `E` becomes `e` and a bare exponent gains an explicit `+`. No value
changes. This is recorded because a reader taking "identical to the input"
at its word would expect `1e400` back, get `1e+400`, and go looking for a
bug that is not there.

Pinned by `only_the_spelling_of_an_exponent_is_normalised` in
`crates/buswatch/src/render.rs`.

## Where this applies in this workspace

The feature flags are set once, on the workspace `serde_json` dependency in
the root `Cargo.toml`, and inherited by every member with
`serde_json.workspace = true`.

Nothing else in the workspace names them, so the tests in
`crates/buswatch/src/render.rs` are what notice if they are dropped:

- `a_number_survives_the_json_round_trip_unchanged` — the
  `arbitrary_precision` half, through the store-document rendering of
  [`14-buswatch.md`](../14-buswatch.md) §5.4.
- `a_float_survives_a_rust_round_trip_unchanged` — the `float_roundtrip`
  half, `f64` → JSON → `f64` compared on the bits.
