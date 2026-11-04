#![forbid(unsafe_code)]
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
