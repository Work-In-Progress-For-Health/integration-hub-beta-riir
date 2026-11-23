//! The `hl7-mock-receiver` binary.
//!
//! Implements `spec/11-mock-receivers.md` §2.
//!
//! A stand-in for a downstream system: it answers MLLP, forwards what it is
//! given, and answers `AE` to anything containing `fail` and `AR` to anything
//! containing `reject`, so a test can drive either unhappy path (§1.1).
//!
//! # It runs with no broker
//!
//! Unlike every other service here, this one starts with neither
//! `SERVICE_BUS_CONNECTION_STRING` nor `SERVICE_BUS_NAMESPACE` set (§2.6,
//! and the exception noted at `spec/02-configuration.md` §8). That is not a
//! degraded mode to be warned about on every start — it is how the mock is
//! used on a laptop, and the log line says plainly which queue it would
//! have forwarded to.

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
        clippy::unreachable
    )
)]

use std::process::ExitCode;
use std::sync::Arc;

use hl7_mock_receiver::config::Config;
use hl7_mock_receiver::server::{Receiver, serve};
use hl7_mock_receiver::sink::LoggingSink;
use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_runtime::shutdown::Shutdown;

fn main() -> ExitCode {
    // Before Tokio spawns any thread; see `ihb_fields::timestamp`.
    let local = ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-mock-receiver: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run(local))
}

/// Everything after the runtime exists.
async fn run(local_offset_found: bool) -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(errors) => {
            eprintln!("hl7-mock-receiver: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    // `spec/18-deployment.md` §4.3 — a configured CA bundle that cannot be
    // used stops the service here, rather than becoming a TLS failure in
    // whichever component happens to need it first.
    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }
    tracing::info!(
        listen = %config.listen,
        queue = %config.egress_queue_name,
        session = ?config.egress_session_id,
        broker = config.has_broker(),
        export_enabled = telemetry.export_enabled,
        local_offset_found,
        "hl7-mock-receiver starting"
    );

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listen = config.listen;
    let health_address = config.health_check;

    let health = match ihb_health::HealthCheck::start(
        &health_address.ip().to_string(),
        health_address.port(),
    )
    .await
    {
        Ok(check) => check,
        Err(error) => {
            tracing::error!(%error, %health_address, "could not start the health check");
            return ExitCode::FAILURE;
        }
    };

    // §2.6 — the two shapes this runs in. They differ only in the sink, so
    // the serve call is written once per arm rather than the receiver being
    // boxed: `MessageSink` is not dyn-compatible (see `ihb_bus::sink`), and
    // the choice is made once at startup.
    let served = if config.has_broker() {
        match connect(&config).await {
            Ok(sender) => {
                let receiver = Arc::new(Receiver::new(
                    sender,
                    config.egress_session_id.clone(),
                    config.max_message_size,
                ));
                serve(listen, receiver, shutdown.watcher()).await
            }
            Err(code) => return code,
        }
    } else {
        tracing::info!(
            queue = %config.egress_queue_name,
            "no Service Bus configured; messages will be acknowledged and logged"
        );
        let receiver = Arc::new(Receiver::new(
            LoggingSink::new(config.egress_queue_name.clone()),
            config.egress_session_id.clone(),
            config.max_message_size,
        ));
        serve(listen, receiver, shutdown.watcher()).await
    };

    health.stop().await;

    match served {
        Ok(()) => {
            tracing::info!("hl7-mock-receiver stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, %listen, "the MLLP listener failed");
            ExitCode::FAILURE
        }
    }
}

/// Open the egress sender, or say why not.
async fn connect(config: &Config) -> Result<ihb_bus::live::LiveSender, ExitCode> {
    let endpoint = Endpoint::from_config(
        config
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.namespace.as_deref(),
    )
    .map_err(|error| {
        tracing::error!(%error, "Service Bus is not configured");
        ExitCode::FAILURE
    })?;

    let bus = LiveBus::connect(&endpoint).await.map_err(|error| {
        tracing::error!(%error, "could not reach Service Bus");
        ExitCode::FAILURE
    })?;

    bus.sender(&config.egress_queue_name)
        .await
        .map_err(|error| {
            tracing::error!(%error, queue = %config.egress_queue_name, "could not open the sender");
            ExitCode::FAILURE
        })
}

/// Telemetry configuration, read straight from the environment.
fn telemetry_config() -> ihb_telemetry::otel::Config {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    ihb_telemetry::otel::Config {
        service_name: read("OTEL_SERVICE_NAME")
            .or_else(|| read("MICROSERVICE_ID"))
            .unwrap_or_else(|| "hl7-mock-receiver".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
