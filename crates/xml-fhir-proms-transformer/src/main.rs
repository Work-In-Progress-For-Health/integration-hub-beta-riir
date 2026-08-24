//! The `xml-fhir-proms-transformer` binary.
//!
//! Implements `spec/16-proms-transformer.md` §1.
//!
//! Everything here is [`ihb_transformer::main_loop::run`]. §1.1 puts this
//! on the same runtime, the same configuration and the same receive loop
//! as the ER7 transformers — only the mapping differs, and that is in this
//! crate's library.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

use xml_fhir_proms_transformer::Proms;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("xml-fhir-proms-transformer: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_transformer::main_loop::run(
        Proms::from_env(),
        "xml-fhir-proms-transformer",
    ))
}
