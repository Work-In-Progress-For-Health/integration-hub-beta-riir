//! The `lookup-service` binary.
//!
//! Implements `spec/20-lookup-service.md`. **Phase 1: not production ready**
//! (§1.1).

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

use std::process::ExitCode;
use std::sync::Arc;

use ihb_runtime::shutdown::Shutdown;
use lookup_service::config::{Config, SeedLocation};
use lookup_service::file_store::{FileTableStore, SeedSource};
use lookup_service::web::{AppState, router};

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("lookup-service: could not start the async runtime: {error}");
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
            eprintln!("lookup-service: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    tracing::warn!(
        "the lookup service is Phase 1 and not production ready \
         (spec/20-lookup-service.md §1.1)"
    );

    if config.cosmos_endpoint.is_some() {
        // §2.5. Serving the seed files instead would answer from a table the
        // operator did not configure.
        tracing::error!(
            "COSMOS_ENDPOINT is set but the Cosmos store is not ported \
             (spec/20-lookup-service.md §1.4); refusing to start"
        );
        return ExitCode::FAILURE;
    }

    // §7.2: the file store is the only store, and a bad seed stops startup.
    let seed = match &config.seed {
        // Unset in every deployed environment: serve no tables rather than
        // some default ones (§7.1).
        SeedLocation::None => Ok(SeedSource::default()),
        SeedLocation::Dir(dir) => SeedSource::from_dir(dir),
    };
    let store = match seed.and_then(|seed| FileTableStore::load(&seed)) {
        Ok(store) => store,
        Err(error) => {
            tracing::error!(%error, "the seed data is not usable");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        listen = %config.listen,
        environment = %config.environment,
        export_enabled = telemetry.export_enabled,
        "lookup-service starting"
    );

    let listen = config.listen;
    let state = Arc::new(AppState::new(config, store));
    if let Err(error) = state.start().await {
        tracing::error!(%error, "startup failed");
        return ExitCode::FAILURE;
    }

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, %listen, "could not bind");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(%listen, "lookup-service listening");

    let watcher = shutdown.watcher();
    let served = axum::serve(listener, router(Arc::clone(&state)))
        .with_graceful_shutdown(async move { watcher.wait().await })
        .await;
    state.stop().await;

    match served {
        Ok(()) => {
            tracing::info!("lookup-service stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "lookup-service stopped with an error");
            ExitCode::FAILURE
        }
    }
}

/// Telemetry settings, read from the environment.
fn telemetry_config() -> ihb_telemetry::otel::Config {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    ihb_telemetry::otel::Config {
        service_name: read("OTEL_SERVICE_NAME").unwrap_or_else(|| "lookup-service".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        // The Azure SDK's HTTP logging includes request URLs, and Cosmos row
        // URLs contain lookup keys (§1.3); keep it at WARN or above.
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
