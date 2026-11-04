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

use std::sync::Arc;

use hl7_pims_transformer::PimsTransformer;
use hl7_pims_transformer::reference_data::{LookupClient, ReferenceDataConfig};

/// Build the reference-data client from the environment (§5.10.4), then run
/// the shared receive loop around [`PimsTransformer`].
///
/// A missing `REFERENCE_DATA_API_BASE_URL` does not stop start-up: it fails
/// the first lookup instead, so a flow whose messages never carry the coded
/// fields still runs (see `ReferenceDataConfig::base_url`).
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

    // §5.10.5. Read before the loop starts so a bad setting stops the
    // service with every problem listed, like the loop's own configuration.
    let config = match ReferenceDataConfig::from_env() {
        Ok(config) => config,
        Err(errors) => {
            eprintln!("hl7-pims-transformer: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };
    // `spec/18-deployment.md` §4.3: an unusable CA bundle is fatal here too.
    let client = match LookupClient::new(config) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("hl7-pims-transformer: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_transformer::main_loop::run(
        PimsTransformer::new(Arc::new(client)),
        "hl7-pims-transformer",
    ))
}
