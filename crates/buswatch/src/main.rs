//! The `buswatch` binary.
//!
//! Implements `spec/14-buswatch.md`.
//!
//! A queue inspector for local development: peek at what is sitting on the
//! Service Bus, look at one message in detail, and clear a queue.
//!
//! # Not for an environment with real patient data
//!
//! §1.2. BusWatch displays message bodies and applies no redaction, which is
//! the whole point of it and the reason it must not be deployed anywhere
//! real. The page says so too, so nobody has to read the specification to
//! find out.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::sync::Arc;

use buswatch::config::{Config, discover_queues, discover_session_queues};
use buswatch::live::LiveReader;
use buswatch::web::{AppState, router};
use ihb_bus::connection::Endpoint;
use ihb_runtime::shutdown::Shutdown;

/// The emulator configuration, compiled in.
///
/// §2.2 discovers queue names from it when `BUSWATCH_QUEUE_NAMES` is blank.
/// Compiled in rather than read at runtime because the container image does
/// not carry the repository — the Python searches three candidate paths and
/// gives up if none exists, which is how a containerised BusWatch ends up
/// showing no queues at all.
const EMULATOR_CONFIG: &str = include_str!("../../../local/ServiceBusEmulatorConfig.json");

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("buswatch: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run())
}

/// Everything after the runtime exists.
async fn run() -> ExitCode {
    let mut config = match Config::from_env() {
        Ok(config) => config,
        Err(errors) => {
            // Printed rather than logged: telemetry is not up yet, and a
            // configuration failure has to be visible whatever the log
            // level says.
            eprintln!("buswatch: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    // §2.2 — blank means discover.
    if config.queue_names.is_empty() {
        config.queue_names = discover_queues(EMULATOR_CONFIG);
        tracing::info!(
            discovered = config.queue_names.len(),
            "BUSWATCH_QUEUE_NAMES is blank, so queues were discovered from \
             the emulator configuration"
        );
    }

    if config.queue_names.is_empty() {
        // Not a startup failure: a BusWatch that will not start is a
        // developer wondering why, and the page says what to set.
        tracing::warn!(
            "no queues configured and none discovered; the page will say so. \
             Set BUSWATCH_QUEUE_NAMES."
        );
    }

    tracing::warn!(
        "BusWatch shows message bodies without redaction (spec/14-buswatch.md \
         §1.2). It must not be pointed at an environment holding real patient \
         data."
    );

    tracing::info!(
        listen = %config.listen,
        queues = config.queue_names.len(),
        peek_count = config.peek_count,
        detail_search_limit = config.detail_search_limit,
        parallelism = config.parallelism,
        export_enabled = telemetry.export_enabled,
        "buswatch starting"
    );

    let endpoint = match Endpoint::from_config(Some(&config.connection_string), None) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::error!(%error, "Service Bus is not configured");
            return ExitCode::FAILURE;
        }
    };

    // §4.4 — which queues need a session receiver. Discovered even when the
    // names came from the environment, because the *names* being configured
    // says nothing about whether they are sessionful.
    let session_queues: BTreeSet<String> = discover_session_queues(EMULATOR_CONFIG);

    let reader = match LiveReader::connect(&endpoint, session_queues).await {
        Ok(reader) => reader,
        Err(error) => {
            tracing::error!(%error, "could not reach Service Bus");
            return ExitCode::FAILURE;
        }
    };

    let listen = config.listen;
    let state = Arc::new(AppState::new(reader, config));

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, %listen, "could not bind");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(%listen, "buswatch listening");

    let watcher = shutdown.watcher();
    let served = axum::serve(listener, router(Arc::clone(&state)))
        .with_graceful_shutdown(async move { watcher.wait().await })
        .await;

    // §7.3 — AMQP resources are closed on shutdown. The emulator has a hard
    // connection quota (§6.1), and a developer restarting BusWatch a dozen
    // times while debugging is exactly who runs into it.
    if let Ok(state) = Arc::try_unwrap(state) {
        if let Err(error) = state.reader.close().await {
            tracing::warn!(%error, "the Service Bus connection did not close cleanly");
        }
    } else {
        // A request was still in flight when the graceful shutdown returned.
        // The connection closes on its own drop, so this costs a moment
        // rather than a leaked link.
        tracing::warn!("a request was still in flight; the connection closes on drop");
    }

    match served {
        Ok(()) => {
            tracing::info!("buswatch stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "buswatch stopped with an error");
            ExitCode::FAILURE
        }
    }
}

/// Telemetry settings, read from the environment.
///
/// A failure here is not a startup failure: BusWatch is a developer tool,
/// and refusing to start because a trace exporter is misconfigured helps
/// nobody.
fn telemetry_config() -> ihb_telemetry::otel::Config {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    ihb_telemetry::otel::Config {
        service_name: read("OTEL_SERVICE_NAME").unwrap_or_else(|| "buswatch".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
