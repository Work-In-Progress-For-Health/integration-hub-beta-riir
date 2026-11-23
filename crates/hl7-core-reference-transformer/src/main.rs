//! The `hl7-core-reference-transformer` binary.
//!
//! Implements `spec/09-transformers.md` §8.
//!
//! [`ihb_transformer::main_loop::run`] around the mapping in this crate's
//! library, plus the WRDS client of `spec/09-transformers.md` §9, which is
//! built first so a bad endpoint or certificate stops the service at start.

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

use hl7_core_reference_transformer::CoreReference;
use hl7_core_reference_transformer::config::WrdsConfig;

/// The name this reports itself as.
const NAME: &str = "hl7-core-reference-transformer";

/// Build the WRDS client, then hand the transformer to the shared receive
/// loop. Exits non-zero before touching the bus if configuration or the
/// client certificate is unusable, so a bad deployment fails at start rather
/// than on the first message.
fn main() -> ExitCode {
    // Before Tokio spawns any thread; see `ihb_fields::timestamp`.
    ihb_fields::timestamp::capture_local_offset();

    let config = match WrdsConfig::from_env() {
        Ok(config) => config,
        Err(errors) => {
            eprintln!("{NAME}: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };
    // Built before the runtime: loading the mTLS certificate is the failure
    // we want at start (see the module doc).
    let transformer = match CoreReference::from_config(&config) {
        Ok(transformer) => transformer,
        Err(error) => {
            eprintln!("{NAME}: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Multi-thread, deliberately: the WRDS lookup blocks a worker
    // (`wrds_service::Client::get_result_set_blocking`).
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{NAME}: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_transformer::main_loop::run(transformer, NAME))
}
