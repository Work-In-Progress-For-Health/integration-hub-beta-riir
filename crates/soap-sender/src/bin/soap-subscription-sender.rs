//! The `soap-subscription-sender` binary.
//!
//! Implements `spec/10-senders.md` §4; this variant reads a topic subscription and does not write to the message store (§5.2).

use std::process::ExitCode;
use std::time::Duration;

use ihb_config::Secret;
use ihb_sender::config::Variant;
use soap_sender::{Endpoint, SoapClient};

fn main() -> ExitCode {
    // Before Tokio spawns any thread: the local UTC offset can only be read
    // reliably while the process is single-threaded (`ihb_fields::timestamp`).
    ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("soap-subscription-sender: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(ihb_sender::main_loop::run(
        Variant::Subscription,
        "soap-subscription-sender",
        |_config| {
            // §4.5 — reserved, and a `true` value is a startup failure
            // until this section specifies the header. Accepting it
            // silently would mean posting without the security header the
            // operator believes is there.
            if read("WS_SECURITY_ENABLED").is_some_and(|value| {
                !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "false" | "0" | "no" | "off"
                )
            }) {
                return Err(
                    "WS_SECURITY_ENABLED is reserved and must be false; spec/10-senders.md §4.5"
                        .to_string(),
                );
            }

            if read("SOAP_CLIENT_CERT_PATH").is_some() {
                return Err(
                    "SOAP_CLIENT_CERT_PATH is configured, but mTLS is not yet implemented by this sender"
                        .to_string(),
                );
            }

            let url = read("SOAP_ENDPOINT_URL")
                .ok_or_else(|| "missing required configuration: SOAP_ENDPOINT_URL".to_string())?;
            let timeout = match read("SOAP_TIMEOUT_SECONDS") {
                None => Duration::from_secs(30),
                Some(value) => Duration::from_secs(
                    value
                        .parse()
                        .map_err(|error| format!("SOAP_TIMEOUT_SECONDS is not a number: {error}"))?,
                ),
            };

            Ok(SoapClient::new(Endpoint {
                url,
                timeout,
                api_key: read("SOAP_API_KEY").map(Secret::new),
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
