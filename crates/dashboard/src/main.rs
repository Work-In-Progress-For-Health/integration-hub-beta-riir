//! The `dashboard` binary.
//!
//! Implements `spec/15-dashboard.md`.
//!
//! The operations dashboard: flow health, queue depths, exceptions, message
//! throughput, and three alarm rules with e-mail escalation.
//!
//! # It starts with nothing configured
//!
//! §1.2. There is no Azure configuration this binary requires, and no page
//! that fails without it. That is what makes it developable on a laptop —
//! `cargo run -p dashboard` and the whole thing is there, reporting
//! everything as unknown.

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

use dashboard::alarm::cosmos_store::{CosmosStore, Credential};
use dashboard::azure::http::HttpsClient;
use dashboard::azure::live::LiveObservatory;
use dashboard::azure::token::TokenSource;
use dashboard::config::{Config, CosmosMode};
use dashboard::web::{AppState, router};
use ihb_runtime::shutdown::Shutdown;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("dashboard: could not start the async runtime: {error}");
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
            // level says. Note what reaching here means — §1.2 makes the
            // *absence* of Azure configuration valid, so this is a value
            // that is present and wrong.
            eprintln!("dashboard: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    if config.session_key.is_none() {
        // `spec/02-configuration.md` §9.2.3. Said out loud, because the
        // consequence — everyone signed out on restart — is otherwise a
        // mystery, and because the alternative the Python chose is a
        // hardcoded key in a public repository.
        tracing::info!(
            "FLASK_SECRET_KEY is not set, so a session key was generated for this \
             process. Sessions will not survive a restart. Set the variable to keep them."
        );
    }

    if !config.redact {
        // §1.3 — allowed, and worth a line in the log so it is never a
        // surprise that patient data is on the screen.
        tracing::warn!(
            "DASHBOARD_REDACT_MESSAGES is off: message content will be displayed \
             unredacted (spec/15-dashboard.md §1.3)"
        );
    }

    tracing::info!(
        listen = %config.listen,
        cosmos = config.cosmos.describe(),
        auth = config.auth.describe(),
        log_analytics = config.azure.has_log_analytics(),
        service_bus = config.azure.has_service_bus(),
        cache_ttl_seconds = config.api_cache_ttl.as_secs(),
        export_enabled = telemetry.export_enabled,
        "dashboard starting"
    );

    // §1.2's whole point: with nothing configured the dashboard still
    // serves, reporting unknown rather than zero. `LiveObservatory` is
    // built either way — it is the *configuration* that decides what it can
    // answer, not a branch here, so there is no second code path that only
    // runs in production.
    if !config.azure.has_log_analytics() && !config.azure.has_service_bus() {
        tracing::info!(
            "no Azure configuration found; every page will render with unknown values \
             (spec/15-dashboard.md §1.2)"
        );
    }
    if config.cosmos == CosmosMode::Disabled {
        tracing::info!(
            "COSMOS_ENDPOINT is not set, so alarm configuration is not persisted \
             (spec/15-dashboard.md §5.6)"
        );
    }

    // `spec/18-deployment.md` §4.3 — before binding a port. A service
    // that answers its health check while unable to complete a TLS
    // handshake is worse than one that refused to start, because the
    // failure then surfaces as somebody else's timeout.
    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }

    let listen = config.listen;
    let azure = match LiveObservatory::new(&config) {
        Ok(azure) => azure,
        Err(error) => {
            tracing::error!(%error, "could not build the HTTPS client");
            return ExitCode::FAILURE;
        }
    };
    // §10.1: the store holds alarm state, pauses, contacts, sources and
    // network test history. Until this was wired, every one of them was
    // `Disabled` in a real deployment whatever `COSMOS_ENDPOINT` said.
    match config.cosmos.clone() {
        CosmosMode::Disabled => serve(Arc::new(AppState::new(azure, config)), listen).await,
        mode => {
            let client = match HttpsClient::new() {
                Ok(client) => client,
                Err(error) => {
                    tracing::error!(%error, "could not build the HTTPS client for Cosmos");
                    return ExitCode::FAILURE;
                }
            };
            let (endpoint, credential) = match mode {
                CosmosMode::Key { endpoint, key } => (endpoint, Credential::Key(key)),
                CosmosMode::Rbac { endpoint } => (
                    endpoint,
                    Credential::Rbac(TokenSource::new(config.auth.clone(), client.clone())),
                ),
                CosmosMode::Disabled => return ExitCode::FAILURE,
            };
            let store = CosmosStore::new(
                endpoint,
                config.cosmos_database.clone(),
                config.cosmos_container.clone(),
                credential,
                client,
            );
            serve(
                Arc::new(AppState::with_alarms(azure, store, config)),
                listen,
            )
            .await
        }
    }
}

/// Bind, serve and shut down.
async fn serve<O, S>(state: Arc<AppState<O, S>>, listen: std::net::SocketAddr) -> ExitCode
where
    O: dashboard::observe::Observatory,
    S: dashboard::alarm::store::AlarmStore,
{
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, %listen, "could not bind");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(%listen, "dashboard listening");

    let watcher = shutdown.watcher();
    let served = axum::serve(listener, router(state))
        .with_graceful_shutdown(async move { watcher.wait().await })
        .await;

    match served {
        Ok(()) => {
            tracing::info!("dashboard stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "dashboard stopped with an error");
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
        service_name: read("OTEL_SERVICE_NAME").unwrap_or_else(|| "dashboard".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
