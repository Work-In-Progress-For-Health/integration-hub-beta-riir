//! The `hl7-sender` binary.
//!
//! Implements `spec/10-senders.md` §3; this variant reads a queue and writes to the message store.
//!
//! The two MLLP sender binaries differ in one argument. Everything else —
//! the pipeline, the throttle, the batch sizing, the connection handling —
//! is shared, because `spec/10-senders.md` §5.1 says the variants differ
//! only in where they read from.

use std::process::ExitCode;
use std::time::Duration;

use hl7_sender::{Destination, MllpClient};
use ihb_sender::config::Variant;

fn main() -> ExitCode {
    // Before Tokio spawns any thread; see `ihb_fields::timestamp`.
    ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-sender: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_sender::main_loop::run(
        Variant::Queue,
        "hl7-sender",
        |_config| {
            // The MLLP destination is this service's own configuration
            // rather than the shared sender's, so it is read here.
            let host = read("RECEIVER_MLLP_HOST")
                .ok_or_else(|| "missing required configuration: RECEIVER_MLLP_HOST".to_string())?;
            let port = read("RECEIVER_MLLP_PORT")
                .ok_or_else(|| "missing required configuration: RECEIVER_MLLP_PORT".to_string())?
                .parse::<u16>()
                .map_err(|error| format!("RECEIVER_MLLP_PORT is not a port: {error}"))?;
            let ack_timeout =
                match read("ACK_TIMEOUT_SECONDS") {
                    None => ihb_sender::config::DEFAULT_ACK_TIMEOUT,
                    Some(value) => Duration::from_secs(value.parse().map_err(|error| {
                        format!("ACK_TIMEOUT_SECONDS is not a number: {error}")
                    })?),
                };

            Ok(MllpClient::new(Destination {
                host,
                port,
                ack_timeout,
            }))
        },
    ))
}

/// An environment variable, treating blank as absent
/// (`spec/02-configuration.md` §1.1).
fn read(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
