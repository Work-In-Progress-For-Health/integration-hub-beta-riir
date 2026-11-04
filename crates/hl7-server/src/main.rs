//! The `hl7-server` binary.
//!
//! Implements `spec/07-hl7-server.md` §8 (startup and shutdown) and
//! `spec/01-architecture.md` §1.3.
//!
//! One process, one flow. Which flow is entirely a matter of environment
//! variables; see [`hl7_server::config`].
//!
//! # Startup order
//!
//! 1. Capture the local UTC offset, **before the runtime starts any
//!    threads** — see [`hl7_server::ack::capture_local_offset`] for why that
//!    cannot wait.
//! 2. Read the configuration. Every problem at once (§1.4), then exit
//!    non-zero if there were any, **before binding a socket** (§1.3).
//! 3. Install telemetry.
//! 4. Connect to Service Bus, and fail if it cannot be reached: a server
//!    that accepts messages it cannot forward would have to refuse every
//!    one of them, which is worse than not listening.
//! 5. Start the health check, then the MLLP listener.
//!
//! # Shutdown order
//!
//! §8.2 gives it: the Service Bus sender, the message store client, the
//! health-check listener, the MLLP listener, then join the listener task.
//!
//! This implementation deliberately **inverts the first three**. The Python
//! closed its sender first and then stopped listening, which leaves a window
//! in which a message can be accepted and cannot be forwarded — and §8.3
//! forbids exactly that outcome. So here the listener stops accepting first,
//! in-flight messages drain, and only then do the sender and the health
//! check close. The observable order of the *last* two steps is unchanged.
//! Recorded as a deviation in `plan.md` §9.

// Startup calls `azservicebus` through `ihb-bus`, and every one of its
// futures is about 32 KB. `ihb_bus::live` records why that is allowed
// rather than boxed away; these three calls happen once, at startup, so
// even the allocation argument does not arise.
#![allow(clippy::large_futures)]

use std::process::ExitCode;
use std::sync::Arc;

use hl7_server::config::{Config, Egress};
use hl7_server::pipeline::Pipeline;
use ihb_bus::connection::Endpoint;
use ihb_bus::live::{LiveBus, LiveSender};
use ihb_runtime::shutdown::Shutdown;

fn main() -> ExitCode {
    // Step 1, before Tokio exists. `capture_local_offset` is unreliable once
    // the process is multi-threaded, and the runtime below makes it so.
    let local = hl7_server::ack::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hl7-server: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run(local))
}

/// Everything after the runtime exists.
// A start-up sequence whose order is the contract (config, then telemetry,
// then bind, then serve); splitting it would scatter that order.
#[allow(clippy::too_many_lines)]
async fn run(local_offset_found: bool) -> ExitCode {
    // Step 2 — before any socket is bound (`spec/02-configuration.md` §1.3).
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(errors) => {
            // Printed rather than logged: telemetry is not up yet, and a
            // configuration failure has to be visible in `podman logs`
            // whatever the log level says.
            eprintln!("hl7-server: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    // Step 3.
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
        egress = %config.egress.entity(),
        flow = ?config.validation_flow_name,
        export_enabled = telemetry.export_enabled,
        local_offset_found,
        "hl7-server starting"
    );

    if !config.redaction_enabled {
        // Worth its own line at warn. `spec/04-observability.md` §2 exists
        // because these payloads carry patient identifiers.
        tracing::warn!(
            "HL7_LOG_REDACTION_ENABLED is off: full message payloads will be logged. \
             This must never be the case where real patient data flows."
        );
    }

    // Step 4.
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
    if let Egress::Topic(name) = &config.egress {
        tracing::info!(topic = %name, "egress is a topic");
    }

    if let Some(queue) = config.message_store_unused_queue.as_deref() {
        // spec/02-configuration.md §2.1.2c — the default changed upstream, so
        // a queue name with the store off is the silent failure to avoid.
        tracing::warn!(
            queue,
            "MESSAGE_STORE_QUEUE_NAME is set but MESSAGE_STORE_ENABLED is not true; \
             no message will be stored"
        );
    }

    let store: Option<LiveSender> = match config.message_store_queue.as_deref() {
        None => {
            // Not a warning: several flows run without one by design.
            tracing::info!("message store off; messages will not be stored");
            None
        }
        Some(queue) => match bus.sender(queue).await {
            Ok(sender) => Some(sender),
            Err(error) => {
                tracing::error!(%error, queue, "could not open the message-store sender");
                return ExitCode::FAILURE;
            }
        },
    };

    // Step 5.
    let address = config.health_check;
    let health =
        match ihb_health::HealthCheck::start(&address.ip().to_string(), address.port()).await {
            Ok(check) => check,
            Err(error) => {
                // A process nothing can probe is a process the orchestrator
                // will never route to (`spec/05-health-check.md` §2.1).
                tracing::error!(%error, %address, "could not start the health check");
                return ExitCode::FAILURE;
            }
        };

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listen = config.listen;
    let pipeline = Arc::new(Pipeline::new(config, egress, store));

    let served = hl7_server::listener::serve(listen, pipeline, shutdown.watcher()).await;

    // The listener has stopped accepting and drained. Only now is it safe
    // to close what a message in flight would have needed; see the module
    // documentation for why this is not §8.2's literal order.
    health.stop().await;

    match served {
        Ok(()) => {
            tracing::info!("hl7-server stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, %listen, "the MLLP listener failed");
            ExitCode::FAILURE
        }
    }
}

/// Telemetry configuration, read straight from the environment.
///
/// Separate from [`Config`] because `spec/02-configuration.md` §2 makes
/// these common to every service, and duplicating them into each service's
/// own struct is how they drift apart.
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
            .unwrap_or_else(|| "hl7-server".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
