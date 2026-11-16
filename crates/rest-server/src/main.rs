//! The `rest-server` binary.
//!
//! Implements `spec/19-rest-servers.md` §9 and `spec/02-configuration.md` §12.
//!
//! One image, configured by environment for exactly one job; `PIPELINE`
//! selects `generic` or `hl7`. Both are served by the same listener, with the
//! same TLS, the same TCP health check and the same shutdown.
//!
//! # Shutdown
//!
//! As `hl7-server`: stop accepting, drain, then close
//! (`spec/07-hl7-server.md` §8.3). A request answered `200` or `201` must
//! have been forwarded.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;
use std::sync::Arc;

use hl7_rest_server::config::Egress;
use hl7_rest_server::http::Hl7Http;
use hl7_rest_server::process::Processor;
use hl7_rest_server::serve::{serve, tls_config};
use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_runtime::shutdown::Shutdown;
use rest_server::config::{Config, Pipeline};
use rest_server::http::{GenericHttp, RestHandler};
use rest_server::process::GenericProcessor;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rest-server: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run())
}

/// Everything after the runtime exists.
// A start-up sequence whose order is the contract (config, then telemetry,
// then bind, then serve); splitting it would scatter that order.
#[allow(clippy::too_many_lines)]
async fn run() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(errors) => {
            // Printed rather than logged: telemetry is not up yet, and a
            // configuration failure has to be visible whatever the log
            // level says.
            eprintln!("rest-server: configuration is not usable:\n{errors}");
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
        pipeline = match config.pipeline { Pipeline::Generic(_) => "generic", Pipeline::Hl7(_) => "hl7" },
        max_request_size = config.max_request_size,
        export_enabled = telemetry.export_enabled,
        "rest-server starting"
    );

    let Ok(tls) = tls_setup(config.tls.as_ref()) else {
        return ExitCode::FAILURE;
    };

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

    let handler = match config.pipeline {
        Pipeline::Generic(generic) => {
            let Ok(wrrs) = open_wrrs(&bus, generic.wrrs.as_ref()).await else {
                return ExitCode::FAILURE;
            };
            let processor = GenericProcessor::new(config.identity, generic, egress, store);
            let processor = match wrrs {
                Some(sender) => processor.with_wrrs(sender),
                None => processor,
            };
            RestHandler::Generic(GenericHttp::new(processor, config.max_request_size))
        }
        Pipeline::Hl7(settings) => {
            let Ok(wrrs) = open_wrrs(&bus, settings.wrrs.as_ref()).await else {
                return ExitCode::FAILURE;
            };
            RestHandler::Hl7(Hl7Http::new(
                Processor::new(config.identity, settings, egress, wrrs, store),
                config.environment,
                config.max_request_size,
            ))
        }
    };

    let Ok(health) = start_health(config.health_check).await else {
        return ExitCode::FAILURE;
    };

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listen = config.listen;
    let served = serve(listen, Arc::new(handler), tls, shutdown.watcher()).await;

    health.stop().await;

    match served {
        Ok(()) => {
            tracing::info!("rest-server stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, %listen, "the REST listener failed");
            ExitCode::FAILURE
        }
    }
}

/// The TCP health check (`spec/05-health-check.md`).
///
/// `Err` has already been logged; the caller only has to stop.
async fn start_health(address: std::net::SocketAddr) -> Result<ihb_health::HealthCheck, ()> {
    ihb_health::HealthCheck::start(&address.ip().to_string(), address.port())
        .await
        .map_err(|error| tracing::error!(%error, %address, "could not start the health check"))
}

/// The WRRS sender: the `risp` flow's, or the `generic` pipeline's optional
/// secondary destination (`spec/19-rest-servers.md` §9.7).
///
/// `Err` has already been logged; the caller only has to stop.
async fn open_wrrs(
    bus: &LiveBus,
    wrrs: Option<&hl7_rest_server::config::Wrrs>,
) -> Result<Option<ihb_bus::live::LiveSender>, ()> {
    let Some(Egress::Queue(name) | Egress::Topic(name)) = wrrs.map(|wrrs| &wrrs.egress) else {
        return Ok(None);
    };
    match bus.sender(name).await {
        Ok(sender) => Ok(Some(sender)),
        Err(error) => {
            tracing::error!(%error, entity = %name, "could not open the WRRS sender");
            Err(())
        }
    }
}

/// The TLS configuration, if any (`spec/02-configuration.md` §4.1).
///
/// `Err` has already been logged; the caller only has to stop.
fn tls_setup(
    tls: Option<&hl7_rest_server::config::Tls>,
) -> Result<Option<tokio_rustls::rustls::ServerConfig>, ()> {
    match tls.map(tls_config) {
        None => {
            tracing::warn!(
                "TLS is not configured; this endpoint will serve plaintext HTTP. \
                 That is correct only behind a TLS-terminating ingress."
            );
            Ok(None)
        }
        Some(Ok(server_config)) => Ok(Some(server_config)),
        Some(Err(error)) => {
            tracing::error!(%error, "TLS is configured but could not be set up");
            Err(())
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
            .unwrap_or_else(|| "rest-server".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
