//! The `hl7-lims-wrrs-transformer` binary.
//!
//! Implements `spec/09-transformers.md` §10. Everything is
//! [`hl7_lims_wrrs_transformer::main_loop::run`].

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

/// Start the runtime and hand over to `main_loop::run`. Exits non-zero only
/// if the async runtime cannot start.
fn main() -> ExitCode {
    // Before Tokio spawns any thread; see `ihb_fields::timestamp`.
    ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-lims-wrrs-transformer: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(hl7_lims_wrrs_transformer::main_loop::run(
        "hl7-lims-wrrs-transformer",
    ))
}
