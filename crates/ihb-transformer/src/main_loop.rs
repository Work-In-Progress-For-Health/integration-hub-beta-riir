//! Assembling and running a transformer service.
//!
//! Implements `spec/09-transformers.md` §1.1 and §1.2.
//!
//! Every transformer's binary is the same eight lines around a different
//! [`Transformer`]; this is those eight lines.

// Startup calls `azservicebus`, whose futures are about 32 KB. `ihb_bus::live`
// records why that is allowed rather than boxed away; here they are all
// one-per-process or one-per-batch, so even the allocation argument does
// not arise.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

use ihb_bus::connection::Endpoint;
use ihb_bus::live::{LiveBus, LiveReceiver, LiveSender};
use ihb_bus::message::Outcome as Disposition;
use ihb_runtime::backoff::Backoff;
use ihb_runtime::shutdown::Shutdown;

use crate::config::Config;
use crate::runtime::Runtime;
use crate::{Outcome, Transformer};

/// Read the configuration, connect, and run until shutdown.
///
/// # Errors
///
/// Never returns an error type; the exit code is the report, because this
/// is what `main` returns.
pub async fn run<T: Transformer + 'static>(transformer: T, default_service: &str) -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(errors) => {
            // Printed rather than logged: telemetry is not up yet, and a
            // configuration failure has to be visible whatever the log
            // level says.
            eprintln!("{default_service}: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config(default_service));
    tracing::info!(
        transformer = transformer.name(),
        ingress = %config.ingress_queue_name,
        egress = %config.egress_queue_name,
        batch = config.max_batch_size,
        export_enabled = telemetry.export_enabled,
        "transformer starting"
    );

    let Some(bus) = connect(&config).await else {
        return ExitCode::FAILURE;
    };

    let Some(pieces) = open(&bus, &config).await else {
        return ExitCode::FAILURE;
    };
    let (egress, receiver) = pieces;

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
    let watcher = shutdown.watcher();

    let batch_size = config.max_batch_size;
    let mut runtime = Runtime::new(config, transformer, egress);
    receive_loop(&mut runtime, &receiver, batch_size, &watcher).await;

    // The receiver closes before the sender, so nothing can be received
    // that cannot then be forwarded — the same ordering argument as
    // `spec/07-hl7-server.md` §8.2a.
    if let Err(error) = receiver.close().await {
        tracing::warn!(%error, "the receiver did not close cleanly");
    }
    health.stop().await;

    tracing::info!("transformer stopped");
    ExitCode::SUCCESS
}

/// Connect to Service Bus, reporting why not.
async fn connect(config: &Config) -> Option<LiveBus> {
    let endpoint = Endpoint::from_config(
        config
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.namespace.as_deref(),
    )
    .inspect_err(|error| tracing::error!(%error, "Service Bus is not configured"))
    .ok()?;

    LiveBus::connect(&endpoint)
        .await
        .inspect_err(|error| tracing::error!(%error, "could not reach Service Bus"))
        .ok()
}

/// Open the egress sender and the ingress receiver.
async fn open(bus: &LiveBus, config: &Config) -> Option<(LiveSender, LiveReceiver)> {
    let egress = bus
        .sender(&config.egress_queue_name)
        .await
        .inspect_err(|error| {
            tracing::error!(%error, queue = %config.egress_queue_name, "could not open the egress sender");
        })
        .ok()?;

    // A session receiver when the queue is session-enabled, which every
    // pipeline queue is (`spec/01-architecture.md` §2.3.1).
    let receiver = match config.ingress_session_id.as_deref() {
        Some(session) => {
            bus.session_receiver(&config.ingress_queue_name, session)
                .await
        }
        None => bus.queue_receiver(&config.ingress_queue_name).await,
    }
    .inspect_err(|error| {
        tracing::error!(%error, queue = %config.ingress_queue_name, "could not open the receiver");
    })
    .ok()?;

    Some((egress, receiver))
}

/// Receive and process until shutdown (§1.2).
async fn receive_loop<T: Transformer, E: ihb_bus::sink::MessageSink>(
    runtime: &mut Runtime<T, E>,
    receiver: &LiveReceiver,
    batch_size: usize,
    watcher: &ihb_runtime::shutdown::Watcher,
) {
    let mut backoff = Backoff::new();

    while watcher.is_running() {
        let batch = match receiver
            .receive(u32::try_from(batch_size).unwrap_or(u32::MAX))
            .await
        {
            Ok(batch) => batch,
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

        let outcomes = runtime.process_batch(&batch).await;

        // §1.7 — a failure abandons the message and everything behind it.
        // `outcomes` is shorter than `batch` in that case, and the tail is
        // abandoned without having been attempted.
        for (message, outcome) in batch.iter().zip(outcomes.iter()) {
            let disposition = match outcome {
                Outcome::Handled => Disposition::Accepted,
                Outcome::NotHandled { .. } => Disposition::Deferred,
            };
            if let Err(error) = receiver.settle(message, disposition).await {
                tracing::error!(%error, "could not settle a message");
            }
        }
        for message in batch.iter().skip(outcomes.len()) {
            if let Err(error) = receiver.settle(message, Disposition::Deferred).await {
                tracing::error!(%error, "could not abandon a message behind a failure");
            }
        }
    }
}

/// Telemetry configuration, read straight from the environment.
fn telemetry_config(default_service: &str) -> ihb_telemetry::otel::Config {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    ihb_telemetry::otel::Config {
        service_name: read("OTEL_SERVICE_NAME")
            .or_else(|| read("MICROSERVICE_ID"))
            .unwrap_or_else(|| default_service.to_string()),
        // §5 of `spec/02-configuration.md` — the Python's transformers
        // defaulted to ERROR, unlike the servers' INFO.
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "ERROR".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
