//! The `hl7-wds-transformer` binary.
//!
//! Implements `spec/09-transformers.md` §7.
//!
//! Everything here is [`ihb_transformer::main_loop::run`]; the filter is in
//! this crate's library.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

use hl7_wds_transformer::Wds;

fn main() -> ExitCode {
    // Before Tokio spawns any thread: reading the local UTC offset is only
    // sound while the process is single-threaded; see `ihb_fields::timestamp`.
    ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-wds-transformer: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_transformer::main_loop::run(Wds, "hl7-wds-transformer"))
}
