//! The `hl7-soap-server` binary.
//!
//! Implements `spec/08-hl7-soap-server.md` and `spec/01-architecture.md`
//! §1.3.
//!
//! The LIMS flow's ingress: an HTTP endpoint taking HL7 v2.xml inside a
//! SOAP envelope and forwarding it to Service Bus as ER7.
//!
//! # Shutdown
//!
//! As `hl7-server`: stop accepting, drain, then close. See that binary and
//! `spec/07-hl7-server.md` §8.2a for why that is not the Python's order.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;
use std::sync::Arc;

use hl7_soap_server::config::Config;
use hl7_soap_server::process::Processor;
use hl7_soap_server::serve::{serve, tls_config};
use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_runtime::shutdown::Shutdown;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-soap-server: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run())
}

/// Everything after the runtime exists.
async fn run() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(errors) => {
            // Printed rather than logged: telemetry is not up yet, and a
            // configuration failure has to be visible whatever the log
            // level says.
            eprintln!("hl7-soap-server: configuration is not usable:\n{errors}");
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
        workflow_id = %config.workflow_id,
        microservice_id = %config.microservice_id,
        listen = %config.listen,
        path = %config.endpoint_path,
        schema_group = %config.schema_group.as_str(),
        export_enabled = telemetry.export_enabled,
        "hl7-soap-server starting"
    );

    // §7 — TLS when both files are configured. The configuration reader has
    // already refused one-without-the-other, so reaching here with `Some`
    // means both paths were given; whether they are *usable* is decided
    // now, before the socket is bound.
    let tls = match config.tls.as_ref().map(tls_config) {
        None => {
            tracing::warn!(
                "TLS is not configured; this endpoint will serve plaintext HTTP. \
                 That is correct only behind a TLS-terminating ingress."
            );
            None
        }
        Some(Ok(server_config)) => Some(server_config),
        Some(Err(error)) => {
            tracing::error!(%error, "TLS is configured but could not be set up");
            return ExitCode::FAILURE;
        }
    };

    let endpoint = match Endpoint::from_config(
        config
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.namespace.as_deref(),
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::error!(%error, "Service Bus is not configured");
            return ExitCode::FAILURE;
        }
    };

    let bus = match LiveBus::connect(&endpoint).await {
        Ok(bus) => bus,
        Err(error) => {
            tracing::error!(%error, "could not reach Service Bus");
            return ExitCode::FAILURE;
        }
    };

    let egress = match bus.sender(config.egress.entity()).await {
        Ok(sender) => sender,
        Err(error) => {
            tracing::error!(%error, entity = config.egress.entity(), "could not open the egress sender");
            return ExitCode::FAILURE;
        }
    };

    let store = match bus.sender(&config.message_store_queue).await {
        Ok(sender) => sender,
        Err(error) => {
            tracing::error!(%error, queue = %config.message_store_queue, "could not open the message-store sender");
            return ExitCode::FAILURE;
        }
    };

    let address = config.health_check;
    let health =
        match ihb_health::HealthCheck::start(&address.ip().to_string(), address.port()).await {
            Ok(check) => check,
            Err(error) => {
                tracing::error!(%error, %address, "could not start the health check");
                return ExitCode::FAILURE;
            }
        };

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listen = config.listen;
    let processor = Arc::new(Processor::new(config, egress, store));

    let served = serve(listen, processor, tls, shutdown.watcher()).await;

    health.stop().await;

    match served {
        Ok(()) => {
            tracing::info!("hl7-soap-server stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, %listen, "the SOAP listener failed");
            ExitCode::FAILURE
        }
    }
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
            .unwrap_or_else(|| "hl7-soap-server".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
