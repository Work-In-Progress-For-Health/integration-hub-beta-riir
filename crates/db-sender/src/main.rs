//! The `db-sender` binary.
//!
//! Implements `spec/10-senders.md` §7. Everything is
//! [`db_sender::main_loop::run`].

// See `main_loop.rs`: `azservicebus` futures are large and one-per-process.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("db-sender: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(db_sender::main_loop::run("db-sender"))
}
