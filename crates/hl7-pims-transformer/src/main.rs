//! The `hl7-pims-transformer` binary.
//!
//! Implements `spec/09-transformers.md` §1 for the PIMS flow.
//!
//! Everything here is [`ihb_transformer::main_loop::run`]; the mapping is
//! in this crate's library, and the loop it runs inside is shared with the
//! other transformers so there is one implementation of §1.3's ordering
//! rather than three.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

use hl7_pims_transformer::Pims;

fn main() -> ExitCode {
    // Before Tokio spawns any thread; see `ihb_fields::timestamp`.
    ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-pims-transformer: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_transformer::main_loop::run(
        Pims,
        "hl7-pims-transformer",
    ))
}
