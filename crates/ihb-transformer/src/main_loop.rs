//! Assembling and running a transformer service.
//!
//! Implements `spec/09-transformers.md` §1.1 and §1.2.
//!
//! Every transformer's binary is the same eight lines around a different
//! [`crate::Transformer`]; this is those eight lines.

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

use crate::config::{Config, Ingress};
use crate::runtime::Runtime;
use crate::{Outcome, PayloadTransformer};

/// Read the configuration, connect, and run until shutdown.
///
/// # Errors
///
/// Never returns an error type; the exit code is the report, because this
/// is what `main` returns.
pub async fn run<T: PayloadTransformer + 'static>(
    transformer: T,
    default_service: &str,
) -> ExitCode {
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

    // `spec/18-deployment.md` §4.3 — a configured CA bundle that cannot be
    // used stops the service here, rather than becoming a TLS failure in
    // whichever component happens to need it first.
    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }
    tracing::info!(
        transformer = transformer.name(),
        ingress = %config.ingress.entity(),
        egress = %config.egress.entity(),
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

    // `spec/03-message-bus.md` §5.1.3 — a session receiver MUST have
    // automatic lock renewal. Without it the lock expires while the
    // transformer is idle, every later receive fails with
    // `IllegalSessionState`, and the service processes nothing for the
    // rest of its life while its health check stays green.
    let batch_size = config.max_batch_size;
    let ingress = config.ingress.clone();
    let session = config.ingress_session_id.clone();
    let mut runtime = Runtime::new(config, transformer, egress);

    // §5.1.5 — reopen on a lost session rather than retrying a dead
    // receiver. Renewal is bounded, and when it gives up the broker drops
    // the session; without this loop every later receive fails forever
    // while the health check stays green.
    let mut receiver = receiver;
    loop {
        let renewer = receiver.renew_session_lock();
        let ended_because = receive_loop(&mut runtime, &receiver, batch_size, &watcher).await;

        // §5.1.4 — the renewer stops **before** the receiver closes, so no
        // renewal is in flight against a receiver being torn down. `close`
        // refuses while the renewer holds it, which makes the ordering
        // enforced rather than remembered.
        if let Some(renewer) = renewer {
            let ended = renewer.stop().await;
            if !ihb_bus::live::renewal_was_clean(ended) {
                tracing::warn!(?ended, "the session lock renewer ended badly");
            }
        }

        // The receiver closes before the sender, so nothing can be received
        // that cannot then be forwarded — the same ordering argument as
        // `spec/07-hl7-server.md` §8.2a.
        if let Err(error) = receiver.close().await {
            tracing::warn!(%error, "the receiver did not close cleanly");
        }

        if ended_because == LoopEnd::Stopped || !watcher.is_running() {
            break;
        }

        let Some(fresh) = reopen(&bus, &ingress, session.as_deref()).await else {
            // Nothing else can be done: the queue is unreachable. Exit
            // non-zero so the orchestrator restarts the container, rather
            // than sitting here healthy and idle.
            tracing::error!(entity = %ingress.entity(), "could not reopen the receiver after a lost session");
            health.stop().await;
            return ExitCode::FAILURE;
        };
        tracing::info!(entity = %ingress.entity(), "reopened the receiver after a lost session");
        receiver = fresh;
    }
    health.stop().await;

    tracing::info!("transformer stopped");
    ExitCode::SUCCESS
}

/// Connect to Service Bus. `None` means the cause was already logged
/// (the connection string is a `Secret` and never appears in it).
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

/// Open the egress sender and the ingress receiver, egress first.
///
/// `None` if either fails, having logged why. Egress is opened first, so a
/// bad egress name is reported before any ingress session lock is taken.
async fn open(bus: &LiveBus, config: &Config) -> Option<(LiveSender, LiveReceiver)> {
    // `sender` opens a queue or a topic alike, by name.
    let egress = bus
        .sender(config.egress.entity())
        .await
        .inspect_err(|error| {
            tracing::error!(%error, entity = %config.egress.entity(), "could not open the egress sender");
        })
        .ok()?;

    let receiver = receiver_for(bus, &config.ingress, config.ingress_session_id.as_deref())
        .await
        .inspect_err(|error| {
            tracing::error!(%error, entity = %config.ingress.entity(), "could not open the receiver");
        })
        .ok()?;

    Some((egress, receiver))
}

/// A session receiver when a session is configured, which every pipeline
/// queue requires (`spec/01-architecture.md` §2.3.1); a plain one otherwise.
async fn receiver_for(
    bus: &LiveBus,
    ingress: &Ingress,
    session: Option<&str>,
) -> Result<LiveReceiver, ihb_bus::BusError> {
    match (ingress, session) {
        (Ingress::Queue(queue), Some(session)) => bus.session_receiver(queue, session).await,
        (Ingress::Queue(queue), None) => bus.queue_receiver(queue).await,
        (
            Ingress::Subscription {
                topic,
                subscription,
            },
            Some(session),
        ) => {
            bus.subscription_receiver(topic, subscription, session)
                .await
        }
        (
            Ingress::Subscription {
                topic,
                subscription,
            },
            None,
        ) => bus.plain_subscription_receiver(topic, subscription).await,
    }
}

/// Why the receive loop returned.
///
/// `spec/03-message-bus.md` §5.1.5. A lost session is not a failure to
/// report and stop on — the service is healthy and the queue is fine, and
/// the only thing wrong is that this particular receiver's session lock
/// expired. The caller accepts the session again and carries on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopEnd {
    /// The shutdown watcher said stop.
    Stopped,
    /// The session lock is gone; the receiver must be reopened.
    SessionLost,
}

/// Accept the ingress session again after it was lost (§5.1.5).
///
/// Only the receiver is rebuilt. The egress sender is unaffected by a
/// receive-side session lock and reopening it would drop messages already
/// accepted into its batch.
async fn reopen(bus: &LiveBus, ingress: &Ingress, session: Option<&str>) -> Option<LiveReceiver> {
    receiver_for(bus, ingress, session)
        .await
        .inspect_err(|error| {
            tracing::error!(%error, entity = %ingress.entity(), "could not reopen the receiver");
        })
        .ok()
}

/// Receive and process until shutdown (`spec/09-transformers.md` §1.2), or
/// until the session is lost.
///
/// [`LoopEnd::SessionLost`] sends the caller back to reopen the receiver;
/// retrying a dead session processes nothing while health stays green
/// (`spec/03-message-bus.md` §5.1.5).
async fn receive_loop<T: PayloadTransformer, E: ihb_bus::sink::MessageSink>(
    runtime: &mut Runtime<T, E>,
    receiver: &LiveReceiver,
    batch_size: usize,
    watcher: &ihb_runtime::shutdown::Watcher,
) -> LoopEnd {
    let mut backoff = Backoff::new();

    while watcher.is_running() {
        let batch = match receiver
            .receive(u32::try_from(batch_size).unwrap_or(u32::MAX))
            .await
        {
            Ok(batch) => batch,
            Err(error) if error.is_session_lost() => {
                // The session lock is gone and this receiver will never
                // work again. Retrying it is an infinite loop that
                // processes nothing while the health check stays green,
                // which is exactly what happened before
                // `spec/03-message-bus.md` §5.1.5 existed. Hand back to
                // the caller, which accepts the session afresh.
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

    LoopEnd::Stopped
}

/// Telemetry configuration, read straight from the environment, so that
/// telemetry set-up does not depend on the rest of configuration parsing.
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
        // `spec/02-configuration.md` §5 — the Python's transformers
        // defaulted to ERROR, unlike the servers' INFO.
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "ERROR".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
