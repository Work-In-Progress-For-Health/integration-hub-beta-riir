//! The `fhir-sender` binary.
//!
//! Implements `spec/10-senders.md` §8. Everything is
//! [`fhir_sender::main_loop::run`].

// See `main_loop.rs`: `azservicebus` futures are large and one-per-process.
#![allow(clippy::large_futures)]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): see `lib.rs`.
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

use std::process::ExitCode;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fhir-sender: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(fhir_sender::main_loop::run("fhir-sender"))
}
