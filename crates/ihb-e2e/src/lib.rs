#![forbid(unsafe_code)]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable,
        clippy::float_arithmetic,
        clippy::float_cmp,
        clippy::lossy_float_literal
    )
)]
//! End-to-end tests; see `tests/`.
//!
//! Implements `tasks.md` P5.7. The crate is a library with no items: it
//! exists so the tests have a home that depends on every service at once,
//! which no single service crate should.
//!
//! Two suites, each behind its own feature so a plain `cargo test` never
//! fails for want of a broker or a container runtime:
//!
//! - `tests/phw_to_mpi.rs` (`emulator`): the PHW flow wired in one process
//!   over the real Service Bus emulator.
//! - `tests/in_compose.rs` (`compose`): every flow through the built
//!   containers of `local/docker-compose.yml`
//!   (`spec/18-deployment.md` §5).
