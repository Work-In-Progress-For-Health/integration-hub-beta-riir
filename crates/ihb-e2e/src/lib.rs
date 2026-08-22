#![forbid(unsafe_code)]
//! End-to-end tests; see `tests/`.
//!
//! Implements `tasks.md` P5.7. The crate is a library with no items: it
//! exists so the tests have a home that depends on every service at once,
//! which no single service crate should.
