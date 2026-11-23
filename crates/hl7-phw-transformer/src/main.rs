//! The `hl7-phw-transformer` binary.
//!
//! Implements `spec/09-transformers.md` §1 for the PHW flow.
//!
//! Everything here is [`ihb_transformer::main_loop::run`]; the mapping is
//! in this crate's library, and the loop it runs inside is shared with the
//! other transformers so there is one implementation of §1.3's ordering
//! rather than three.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
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

use hl7_phw_transformer::Phw;

/// Start the shared receive loop around [`Phw`]. Exits non-zero only if the
/// async runtime cannot start; configuration problems are reported by the
/// loop itself (`spec/09-transformers.md` §1.1).
fn main() -> ExitCode {
    // Before Tokio spawns any thread; see `ihb_fields::timestamp`.
    ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-phw-transformer: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_transformer::main_loop::run(
        Phw::default(),
        "hl7-phw-transformer",
    ))
}
