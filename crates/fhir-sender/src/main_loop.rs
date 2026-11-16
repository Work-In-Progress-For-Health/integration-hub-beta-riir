//! Assembling and running the FHIR sender.
//!
//! Implements `spec/10-senders.md` §8.1 and §8.7, with the session handling
//! of `spec/03-message-bus.md` §5.1 (lock renewal, reopening a lost session).
//!
//! Not `ihb_sender::main_loop::run`: that is the §2 pipeline (throttle,
//! message store, acknowledgement) and this service has none of it (`§8.1`).
//! The session lock matters more here than anywhere: one status poll can
//! outlast the queue's lock duration, and only a renewed session survives it.

// Startup calls `azservicebus`, whose futures are ~32 KB. `ihb_bus::live`
// records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

use ihb_bus::connection::Endpoint;
use ihb_bus::live::{LiveBus, LiveReceiver};
use ihb_bus::message::{Incoming, Outcome as Settle};
use ihb_runtime::backoff::Backoff;
use ihb_runtime::shutdown::{Shutdown, Watcher};
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::client::{FhirClient, TokioClock};
use crate::config::AppConfig;
use crate::http::HyperTransport;
use crate::processor::{Disposition, process_batch};

/// The client this service runs.
type Client = FhirClient<HyperTransport, TokioClock>;

/// Read the configuration, connect, and run until shutdown.
///
/// The exit code is the report, because this is what `main` returns.
#[allow(
    clippy::too_many_lines,
    reason = "one linear start-up sequence; splitting it would scatter the ordering that spec/03 §5.1 depends on"
)]
pub async fn run(default_service: &str) -> ExitCode {
    let config = match AppConfig::from_env() {
        Ok(config) => config,
        Err(errors) => {
            // Printed rather than logged: telemetry is not up yet.
            eprintln!("{default_service}: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let read = |name: &str, default: &str| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let telemetry = ihb_telemetry::otel::configure(&ihb_telemetry::otel::Config {
        service_name: read("OTEL_SERVICE_NAME", &config.microservice_id),
        // ERROR, as upstream `fhir_sender` does: `LOG_LEVEL` defaults to
        // `ERROR` there, and the local env file turns it up.
        log_level: read("LOG_LEVEL", "ERROR"),
        azure_log_level: read("AZURE_LOG_LEVEL", "WARN"),
        rust_log: std::env::var("RUST_LOG")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        connection_string: std::env::var("APPLICATIONINSIGHTS_CONNECTION_STRING")
            .ok()
            .filter(|v| !v.trim().is_empty()),
    });

    // `spec/18-deployment.md` §4.3.
    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }
    tracing::info!(
        queue = %config.ingress_queue,
        endpoint = %config.endpoint_url.split('?').next().unwrap_or(""),
        auth = %match config.auth {
            crate::config::AuthConfig::ApiKey(_) => "api_key",
            crate::config::AuthConfig::OAuth2 { .. } => "oauth2",
        },
        batch = config.max_batch_size,
        export_enabled = telemetry.export_enabled,
        "fhir-sender starting"
    );

    let Ok(endpoint) = Endpoint::from_config(
        config
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.namespace.as_deref(),
    )
    .inspect_err(|error| tracing::error!(%error, "Service Bus is not configured")) else {
        return ExitCode::FAILURE;
    };
    let Ok(bus) = LiveBus::connect(&endpoint)
        .await
        .inspect_err(|error| tracing::error!(%error, "could not reach Service Bus"))
    else {
        return ExitCode::FAILURE;
    };
    let Ok(mut receiver) = open_receiver(&bus, &config)
        .await
        .inspect_err(|error| tracing::error!(%error, "could not open the receiver"))
    else {
        return ExitCode::FAILURE;
    };

    let address = config.health_check;
    let Ok(health) = ihb_health::HealthCheck::start(&address.ip().to_string(), address.port())
        .await
        .inspect_err(|error| tracing::error!(%error, %address, "could not start the health check"))
    else {
        return ExitCode::FAILURE;
    };

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();
    let watcher = shutdown.watcher();
    let events = EventLogger::new(&config.workflow_id, &config.microservice_id);
    let metrics = Metrics::new(
        &config.workflow_id,
        &config.microservice_id,
        &config.health_board,
        &config.peer_service,
    );
    let mut client = match build_client(&config) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(%error, "could not build the FHIR client");
            health.stop().await;
            return ExitCode::FAILURE;
        }
    };

    loop {
        // `spec/03-message-bus.md` §5.1.3: a session receiver MUST renew its
        // lock, or it expires while idle and every later receive fails with
        // the health check still green.
        let renewer = receiver.renew_session_lock();
        let ended =
            receive_loop(&receiver, &mut client, &config, &events, &metrics, &watcher).await;

        // §5.1.4: the renewer stops before the receiver closes.
        if let Some(renewer) = renewer {
            let result = renewer.stop().await;
            if !ihb_bus::live::renewal_was_clean(result) {
                tracing::warn!(?result, "the session lock renewer ended badly");
            }
        }
        if let Err(error) = receiver.close().await {
            tracing::warn!(%error, "the receiver did not close cleanly");
        }
        if ended == LoopEnd::Stopped || !watcher.is_running() {
            break;
        }

        // §5.1.5: reopen on a lost session.
        match open_receiver(&bus, &config).await {
            Ok(fresh) => receiver = fresh,
            Err(error) => {
                tracing::error!(%error, "could not reopen the receiver after a lost session");
                health.stop().await;
                return ExitCode::FAILURE;
            }
        }
    }

    health.stop().await;
    tracing::info!("fhir-sender stopped");
    ExitCode::SUCCESS
}

/// The client for the configured endpoint and credentials.
fn build_client(config: &AppConfig) -> Result<Client, String> {
    let transport = HyperTransport::new().map_err(|error| error.to_string())?;
    FhirClient::new(
        &config.endpoint_url,
        config.build_auth()?,
        config.settings,
        transport,
        TokioClock::new(),
    )
}

/// The session receiver on the ingress queue. A session is required, for the
/// lock renewal that a long status poll depends on.
async fn open_receiver(
    bus: &LiveBus,
    config: &AppConfig,
) -> Result<LiveReceiver, ihb_bus::BusError> {
    bus.session_receiver(&config.ingress_queue, &config.ingress_session_id)
        .await
}

/// Why the receive loop returned (`spec/03-message-bus.md` §5.1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopEnd {
    /// Shutdown was requested.
    Stopped,
    /// The session lock is gone; reopen.
    SessionLost,
}

/// Receive and process until shutdown or a lost session.
async fn receive_loop(
    receiver: &LiveReceiver,
    client: &mut Client,
    config: &AppConfig,
    events: &EventLogger,
    metrics: &Metrics,
    watcher: &Watcher,
) -> LoopEnd {
    let mut backoff = Backoff::new();
    while watcher.is_running() {
        let batch = match receiver
            .receive(u32::try_from(config.max_batch_size).unwrap_or(u32::MAX))
            .await
        {
            Ok(batch) => batch,
            Err(error) if error.is_session_lost() => {
                tracing::warn!(%error, "the session was lost; reopening the receiver");
                return LoopEnd::SessionLost;
            }
            Err(error) => {
                tracing::error!(%error, "receive failed");
                backoff.failed();
                if !watcher.sleep(backoff.delay()).await {
                    break;
                }
                continue;
            }
        };
        backoff.succeeded();
        if batch.is_empty() {
            continue;
        }

        let outcomes = process_batch(&batch, client, events, metrics).await;
        settle_all(receiver, &batch, &outcomes).await;
    }
    LoopEnd::Stopped
}

/// Settle a batch: complete, dead-letter or abandon.
///
/// Whatever follows the last outcome was never attempted and is abandoned.
/// A settlement that fails is logged and the lock is left to expire, which
/// redelivers the message: the safe direction. (For a *completed* message
/// that means a possible resubmission to Promptly; the IG defines no
/// idempotency mechanism, `spec/10-senders.md` §8.8.)
async fn settle_all(receiver: &LiveReceiver, batch: &[Incoming], outcomes: &[Disposition]) {
    for (message, outcome) in batch.iter().zip(outcomes) {
        let result = match outcome {
            Disposition::Complete => receiver.settle(message, Settle::Accepted).await,
            Disposition::Abandon => receiver.settle(message, Settle::Deferred).await,
            Disposition::DeadLetter {
                reason,
                description,
            } => receiver.dead_letter(message, reason, description).await,
        };
        if let Err(error) = result {
            tracing::error!(%error, "could not settle a message");
        }
    }
    for message in batch.iter().skip(outcomes.len()) {
        if let Err(error) = receiver.settle(message, Settle::Deferred).await {
            tracing::error!(%error, "could not abandon a message behind a failure");
        }
    }
}
