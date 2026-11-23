//! The `http-mock-receiver` binary.
//!
//! Implements `spec/11-mock-receivers.md` §3.
//!
//! A stand-in for a downstream SOAP system: it answers anything vaguely
//! SOAP-shaped, forwards the payload if a queue is configured, and answers
//! a SOAP fault to anything containing the word `fail` (§1.1). Unlike
//! `hl7-mock-receiver` there is no `reject` trigger: SOAP has no
//! reject/error distinction to drive.

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

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;

use http_mock_receiver::server::{Receiver, serve};
use ihb_bus::connection::Endpoint;
use ihb_bus::live::{LiveBus, LiveSender};
use ihb_runtime::shutdown::Shutdown;

fn main() -> ExitCode {
    ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("http-mock-receiver: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run())
}

/// Everything after the runtime exists.
async fn run() -> ExitCode {
    let telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    // `spec/18-deployment.md` §4.3 — a configured CA bundle that cannot be
    // used stops the service here, rather than becoming a TLS failure in
    // whichever component happens to need it first.
    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }

    // Configuration is read inline rather than through `ihb_config`: this
    // mock has four optional variables and no cross-field rules, so the
    // all-problems-at-once reader (`spec/02-configuration.md` §1.4) would
    // add nothing.
    let host = read("HOST").unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = match read("PORT").map(|value| value.parse::<u16>()) {
        None => 8080,
        Some(Ok(port)) => port,
        Some(Err(error)) => {
            eprintln!("http-mock-receiver: PORT is not a port: {error}");
            return ExitCode::FAILURE;
        }
    };
    let listen = SocketAddr::new(
        host.parse::<IpAddr>()
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port,
    );

    let queue = read("EGRESS_QUEUE_NAME");
    tracing::info!(
        %listen,
        queue = ?queue,
        export_enabled = telemetry.export_enabled,
        "http-mock-receiver starting"
    );

    // §3.9 — forwarding is optional; absent means log-only, which is how
    // this runs on a laptop.
    let sink: Option<LiveSender> = match queue.as_deref() {
        None => {
            tracing::info!("no EGRESS_QUEUE_NAME; payloads will be acknowledged and logged");
            None
        }
        Some(queue) => match connect(queue).await {
            Ok(sender) => Some(sender),
            Err(code) => return code,
        },
    };

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let receiver = Arc::new(Receiver {
        sink,
        session_id: read("EGRESS_SESSION_ID"),
        fhir: http_mock_receiver::fhir::FhirMock::new(),
    });

    match serve(listen, receiver, shutdown.watcher()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, %listen, "the listener failed");
            ExitCode::FAILURE
        }
    }
}

/// Open the egress sender, or say why not.
async fn connect(queue: &str) -> Result<LiveSender, ExitCode> {
    let endpoint = Endpoint::from_config(
        read("SERVICE_BUS_CONNECTION_STRING").as_deref(),
        read("SERVICE_BUS_NAMESPACE").as_deref(),
    )
    .map_err(|error| {
        tracing::error!(%error, "Service Bus is not configured");
        ExitCode::FAILURE
    })?;

    let bus = LiveBus::connect(&endpoint).await.map_err(|error| {
        tracing::error!(%error, "could not reach Service Bus");
        ExitCode::FAILURE
    })?;

    bus.sender(queue).await.map_err(|error| {
        tracing::error!(%error, queue, "could not open the sender");
        ExitCode::FAILURE
    })
}

/// An environment variable, treating blank as absent.
fn read(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Telemetry configuration, read straight from the environment.
fn telemetry_config() -> ihb_telemetry::otel::Config {
    ihb_telemetry::otel::Config {
        service_name: read("OTEL_SERVICE_NAME")
            .or_else(|| read("MICROSERVICE_ID"))
            .unwrap_or_else(|| "http-mock-receiver".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
