//! The `hl7-lims-wrrs-transformer` binary.
//!
//! Implements `spec/09-transformers.md` §10. Everything is
//! [`hl7_lims_wrrs_transformer::main_loop::run`].

// See `main_loop.rs`: `azservicebus` futures are large and one-per-process.
#![allow(clippy::large_futures)]

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
