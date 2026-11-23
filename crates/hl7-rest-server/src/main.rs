//! The `hl7-rest-server` binary.
//!
//! Implements `spec/19-rest-servers.md` §1 to §3 and `spec/02-configuration.md`
//! §11.
//!
//! # Shutdown
//!
//! As `hl7-server`: stop accepting, drain, then exit. A request answered
//! `201` must have been forwarded (`spec/07-hl7-server.md` §8.3).

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

use hl7_rest_server::config::{Config, Egress};
use hl7_rest_server::http::Hl7Http;
use hl7_rest_server::process::Processor;
use hl7_rest_server::serve::serve;
use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_runtime::shutdown::Shutdown;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-rest-server: could not start the async runtime: {error}");
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
            // Printed rather than logged: telemetry is not up yet.
            eprintln!("hl7-rest-server: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }
    tracing::info!(
        workflow_id = %config.identity.workflow_id,
        microservice_id = %config.identity.microservice_id,
        listen = %config.listen,
        flow = config.settings.flow_name().unwrap_or("none"),
        export_enabled = telemetry.export_enabled,
        "hl7-rest-server starting"
    );
    tracing::warn!(
        "TLS is not offered by the standalone service; it is correct only behind a \
         TLS-terminating ingress (spec/19-rest-servers.md §8)."
    );

    let endpoint = match Endpoint::from_config(
        config
            .identity
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.identity.namespace.as_deref(),
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

    let egress = match bus.sender(config.identity.egress.entity()).await {
        Ok(sender) => sender,
        Err(error) => {
            tracing::error!(%error, "could not open the egress sender");
            return ExitCode::FAILURE;
        }
    };

    let wrrs = match config.settings.wrrs.as_ref().map(|wrrs| &wrrs.egress) {
        Some(Egress::Queue(name) | Egress::Topic(name)) => match bus.sender(name).await {
            Ok(sender) => Some(sender),
            Err(error) => {
                tracing::error!(%error, entity = %name, "could not open the WRRS sender");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    if let Some(queue) = config.identity.message_store_unused_queue.as_deref() {
        // spec/02-configuration.md §2.1.2c — the default changed upstream, so
        // a queue name with the store off is the silent failure to avoid.
        tracing::warn!(
            queue,
            "MESSAGE_STORE_QUEUE_NAME is set but MESSAGE_STORE_ENABLED is not true; \
             no message will be stored"
        );
    }

    let store = match config.identity.message_store_queue.as_deref() {
        Some(queue) => match bus.sender(queue).await {
            Ok(sender) => Some(sender),
            Err(error) => {
                tracing::error!(%error, queue, "could not open the message-store sender");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listen = config.listen;
    let handler = Arc::new(Hl7Http::new(
        Processor::new(config.identity, config.settings, egress, wrrs, store),
        config.environment,
        config.max_message_size,
    ));

    match serve(listen, handler, None, shutdown.watcher()).await {
        Ok(()) => {
            tracing::info!("hl7-rest-server stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, %listen, "the REST listener failed");
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
            .unwrap_or_else(|| "hl7-rest-server".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATION_INSIGHTS_CONNECTION_STRING")
            .or_else(|| read("APPLICATIONINSIGHTS_CONNECTION_STRING")),
    }
}
