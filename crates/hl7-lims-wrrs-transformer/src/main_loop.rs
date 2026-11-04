//! Assembling and running the classifier service.
//!
//! Implements `spec/09-transformers.md` §10.1 and §10.7, and borrows
//! `spec/03-message-bus.md` §5.1 (session lock renewal, reopening a lost
//! session) from the shared transformer loop it cannot reuse.
//!
//! `ihb_transformer::main_loop::run` is one in, one out and private in its
//! helpers; a copy of its shape here is cheaper than bending that runtime to
//! fan out (`§10.1`).

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
use ihb_transformer::config::{Config, Egress, Ingress};

use crate::mapping::MappingTypes;
use crate::processor::{Disposition, process_batch};

/// The batch size when `MAX_BATCH_SIZE` is unset: the Python's `config.ini`
/// (`§10.7`), not the shared runtime's 100.
pub const DEFAULT_BATCH_SIZE: usize = 1;

/// Check that the shared configuration has the shape `§10.7` needs.
///
/// # Errors
///
/// A message naming what is missing: this service reads a topic
/// subscription and writes a topic, never a queue.
///
/// # Examples
///
/// ```
/// use hl7_lims_wrrs_transformer::main_loop::require_topics;
/// use ihb_transformer::config::Config;
///
/// let queue = Config::from_pairs([
///     ("WORKFLOW_ID", "w"), ("MICROSERVICE_ID", "m"), ("HEALTH_BOARD", "h"),
///     ("PEER_SERVICE", "p"), ("INGRESS_QUEUE_NAME", "q"), ("EGRESS_QUEUE_NAME", "q2"),
/// ])?;
/// assert!(require_topics(&queue).is_err());
/// # Ok::<(), ihb_config::ConfigErrors>(())
/// ```
pub fn require_topics(config: &Config) -> Result<(), String> {
    if !matches!(config.ingress, Ingress::Subscription { .. }) {
        return Err(
            "INGRESS_TOPIC_NAME and INGRESS_SUBSCRIPTION_NAME are required for this service".into(),
        );
    }
    if !matches!(config.egress, Egress::Topic(_)) {
        return Err(
            "EGRESS_TOPIC_NAME is required: classified messages are published to a topic".into(),
        );
    }
    Ok(())
}

/// Read the configuration, connect, and run until shutdown.
///
/// The exit code is the report, because this is what `main` returns:
/// failure for an unusable configuration or an unreachable bus, success after
/// a clean shutdown. `default_service` names the service in start-up
/// messages printed before telemetry exists. `MAX_BATCH_SIZE` defaults to 1
/// here rather than the shared 100 (§10.7).
#[allow(
    clippy::too_many_lines,
    reason = "one linear start-up sequence; splitting it would scatter the ordering that §10 and spec/03 §5.1 depend on"
)]
pub async fn run(default_service: &str) -> ExitCode {
    let config = match Config::from_env() {
        Ok(mut config) => {
            // Unset means the service default of 1, not the shared 100.
            if !std::env::var("MAX_BATCH_SIZE").is_ok_and(|v| !v.trim().is_empty()) {
                config.max_batch_size = DEFAULT_BATCH_SIZE;
            }
            config
        }
        Err(errors) => {
            // Printed rather than logged: telemetry is not up yet.
            eprintln!("{default_service}: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };
    let types = match MappingTypes::from_env() {
        Ok(types) => types,
        Err(errors) => {
            eprintln!("{default_service}: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(reason) = require_topics(&config) {
        eprintln!("{default_service}: configuration is not usable:\n{reason}");
        return ExitCode::FAILURE;
    }

    let service = std::env::var("OTEL_SERVICE_NAME")
        .or_else(|_| std::env::var("MICROSERVICE_ID"))
        .unwrap_or_else(|_| default_service.to_string());
    let telemetry = ihb_telemetry::otel::configure(&ihb_telemetry::otel::Config {
        service_name: service,
        // Transformers default to ERROR (`spec/02-configuration.md` §5).
        log_level: env_or("LOG_LEVEL", "ERROR"),
        azure_log_level: env_or("AZURE_LOG_LEVEL", "WARN"),
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
        ingress = %config.ingress.entity(),
        egress = %config.egress.entity(),
        nhs_id_update = %types.nhs_id_update,
        a40 = %types.a40,
        batch = config.max_batch_size,
        export_enabled = telemetry.export_enabled,
        "LIMS WRRS transformer starting"
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
    let Ok(egress) = bus
        .sender(config.egress.entity())
        .await
        .inspect_err(|error| tracing::error!(%error, "could not open the egress sender"))
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
    let events = EventLogger::new(&config.workflow_id, &config.microservice_id)
        .with_redaction(config.redaction_enabled);

    loop {
        // `spec/03-message-bus.md` §5.1.3: without renewal the session lock
        // expires while idle and every later receive fails, with the health
        // check still green.
        let renewer = receiver.renew_session_lock();
        let ended = receive_loop(&receiver, &egress, &config, &types, &events, &watcher).await;

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

        // §5.1.5: reopen on a lost session rather than retry a dead receiver.
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
    tracing::info!("LIMS WRRS transformer stopped");
    ExitCode::SUCCESS
}

/// An environment variable, or `default` when unset or blank.
fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// The subscription receiver, with a session when one is configured.
async fn open_receiver(bus: &LiveBus, config: &Config) -> Result<LiveReceiver, ihb_bus::BusError> {
    let Ingress::Subscription {
        topic,
        subscription,
    } = &config.ingress
    else {
        // Unreachable after `require_topics`, but a typed error beats a panic.
        return Err(ihb_bus::BusError::Disposition {
            action: "open",
            entity: config.ingress.entity(),
            reason: "this service reads a topic subscription".to_string(),
        });
    };
    match config.ingress_session_id.as_deref() {
        Some(session) => {
            bus.subscription_receiver(topic, subscription, session)
                .await
        }
        None => bus.plain_subscription_receiver(topic, subscription).await,
    }
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
///
/// Settles each message from its [`Disposition`](crate::processor::Disposition)
/// and returns [`LoopEnd::SessionLost`] so the caller can reopen the
/// receiver, rather than looping on a lock that has gone.
async fn receive_loop(
    receiver: &LiveReceiver,
    egress: &ihb_bus::live::LiveSender,
    config: &Config,
    types: &MappingTypes,
    events: &EventLogger,
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

        let outcomes = process_batch(
            &batch,
            egress,
            config.egress_session_id.as_deref(),
            types,
            events,
        )
        .await;
        settle_all(receiver, &batch, &outcomes).await;
    }
    LoopEnd::Stopped
}

/// Settle a batch: complete, dead-letter, or abandon (`§10.5`, `§10.6`).
///
/// Anything past the last outcome was never attempted and is abandoned. A
/// settlement that fails is logged and the message is left to its lock
/// expiring, which redelivers it: the safe direction.
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
