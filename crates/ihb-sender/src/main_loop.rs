//! Assembling and running a sender service.
//!
//! Implements `spec/10-senders.md` §1.1 and §5.

// Startup calls `azservicebus`, whose futures are about 32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

use ihb_bus::connection::Endpoint;
use ihb_bus::live::{LiveBus, LiveReceiver, LiveSender};
use ihb_bus::message::Outcome as Disposition;
use ihb_runtime::backoff::Backoff;
use ihb_runtime::shutdown::{Shutdown, Watcher};
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::config::{Config, Ingress, Variant};
use crate::pipeline::{Context, Delivery, Settlement, process_one};
use crate::throttle::{Throttle, batch_size};

/// Read the configuration, connect, and run until shutdown.
///
/// `build` is handed the configuration and returns the transport — MLLP or
/// SOAP — so this function does not need to know which service it is
/// running.
pub async fn run<D, F>(variant: Variant, default_service: &str, build: F) -> ExitCode
where
    D: Delivery,
    F: FnOnce(&Config) -> Result<D, String>,
{
    let config = match Config::from_env(variant) {
        Ok(config) => config,
        Err(errors) => {
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
    if let Some(queue) = config.message_store_unused_queue.as_deref() {
        // spec/02-configuration.md §2.1.2c — the default changed upstream, so
        // a queue name with the store off is the silent failure to avoid.
        tracing::warn!(
            queue,
            "MESSAGE_STORE_QUEUE_NAME is set but MESSAGE_STORE_ENABLED is not true; \
             no message will be stored"
        );
    }
    tracing::info!(
        ingress = %config.ingress.entity(),
        store = ?config.message_store_queue,
        batch = config.max_batch_size,
        rate = ?config.max_messages_per_minute,
        export_enabled = telemetry.export_enabled,
        "sender starting"
    );

    // §2.4.3 — a configured zero fails here rather than at the first
    // message.
    let mut throttle = match Throttle::per_minute(config.max_messages_per_minute) {
        Ok(throttle) => throttle,
        Err(error) => {
            tracing::error!(%error, "the throttle is not configured usably");
            return ExitCode::FAILURE;
        }
    };

    // §2.3 — reduce the batch so a whole one fits inside the lock-renewal
    // window, and say so when it is reduced.
    let batch = batch_size(config.max_batch_size, throttle.interval());
    if batch < config.max_batch_size {
        tracing::warn!(
            configured = config.max_batch_size,
            reduced_to = batch,
            window_seconds = crate::throttle::BATCH_WINDOW.as_secs(),
            interval = ?throttle.interval(),
            "the batch size was reduced so a whole batch fits inside the lock-renewal window"
        );
    }

    let delivery = match build(&config) {
        Ok(delivery) => delivery,
        Err(reason) => {
            tracing::error!(%reason, "the delivery client could not be built");
            return ExitCode::FAILURE;
        }
    };

    let Some(bus) = connect(&config).await else {
        return ExitCode::FAILURE;
    };

    // Before the receiver is opened, because accepting any session waits for
    // one to exist and must be interruptible.
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let Some((receiver, store)) = open(&bus, &config, &shutdown.watcher()).await else {
        return ExitCode::FAILURE;
    };

    let address = config.health_check;
    let health =
        match ihb_health::HealthCheck::start(&address.ip().to_string(), address.port()).await {
            Ok(check) => check,
            Err(error) => {
                tracing::error!(%error, %address, "could not start the health check");
                return ExitCode::FAILURE;
            }
        };

    let events = EventLogger::new(&config.workflow_id, &config.microservice_id)
        .with_redaction(config.redaction_enabled);
    let metrics = Metrics::new(
        &config.workflow_id,
        &config.microservice_id,
        &config.health_board,
        &config.peer_service,
    );

    // `spec/03-message-bus.md` §5.1.3 — a session receiver MUST have
    // automatic lock renewal. Without it the lock expires while the sender
    // is idle, every later receive fails with `IllegalSessionState`, and
    // the service delivers nothing for the rest of its life while its
    // health check stays green.
    // §5.1.5 — the session loop: reopen on a lost session rather than
    // retrying a dead receiver.
    if !session_loop(SessionLoop {
        bus: &bus,
        config: &config,
        receiver,
        delivery,
        store,
        batch,
        throttle: &mut throttle,
        events: &events,
        metrics: &metrics,
        watcher: &shutdown.watcher(),
    })
    .await
    {
        health.stop().await;
        return ExitCode::FAILURE;
    }
    health.stop().await;

    tracing::info!("sender stopped");
    ExitCode::SUCCESS
}

/// Open the ingress receiver, whatever shape §6.1 and §6.2 gave it.
///
/// Its own function because §5.1.5's reopen path needs exactly this and
/// nothing else — reopening the message-store sender as well would be
/// wrong, and having two copies of the `Ingress` match would be worse.
///
/// With `INGRESS_ACCEPT_ANY_SESSION`, "no session available" is the normal
/// state of an idle queue, not a fault: this waits, with backoff, until a
/// session appears or shutdown is requested. A fixed session or none fails
/// at once, as before.
async fn open_receiver(bus: &LiveBus, config: &Config, watcher: &Watcher) -> Option<LiveReceiver> {
    let mut backoff = Backoff::new();

    loop {
        match try_open_receiver(bus, config).await {
            Ok(receiver) => return Some(receiver),
            Err(error) if config.accept_any_session && watcher.is_running() => {
                tracing::debug!(%error, entity = %config.ingress.entity(), "no session available yet");
                backoff.failed();
                if !watcher.sleep(backoff.delay()).await {
                    return None;
                }
            }
            Err(error) => {
                tracing::error!(%error, entity = %config.ingress.entity(), "could not open the receiver");
                return None;
            }
        }
    }
}

/// One attempt at [`open_receiver`]: pick the receiver kind from the
/// ingress shape and `INGRESS_ACCEPT_ANY_SESSION`. Accept-any wins over a
/// fixed session id so a replay deployment can drain whichever session has
/// work; failure is returned, not logged, so the caller can back off and
/// retry while the shutdown watcher is still honoured.
async fn try_open_receiver(
    bus: &LiveBus,
    config: &Config,
) -> Result<LiveReceiver, ihb_bus::BusError> {
    let any = config.accept_any_session;
    match &config.ingress {
        Ingress::Queue { name, session_id } => match (session_id, any) {
            (_, true) => bus.next_session_receiver(name).await,
            (Some(session), false) => bus.session_receiver(name, session).await,
            (None, false) => bus.queue_receiver(name).await,
        },
        Ingress::Subscription {
            topic,
            subscription,
            session_id,
        } => match (session_id, any) {
            (_, true) => {
                bus.next_subscription_session_receiver(topic, subscription)
                    .await
            }
            (Some(session), false) => {
                bus.subscription_receiver(topic, subscription, session)
                    .await
            }
            // A subscription without a session is a plain receiver; §6.2
            // makes the session optional here.
            (None, false) => bus.plain_subscription_receiver(topic, subscription).await,
        },
    }
}

/// Everything the session loop needs. Borrowed for the lifetime of one
/// process run; only `receiver` is swapped, when a session lock is lost.
struct SessionLoop<'a, D> {
    /// The connection, for reopening a receiver.
    bus: &'a LiveBus,
    /// The configuration, which names the ingress.
    config: &'a Config,
    /// The current receiver, replaced whenever its session is lost.
    receiver: LiveReceiver,
    /// How a message reaches its destination.
    delivery: D,
    /// The message-store sender, if one is configured.
    store: Option<LiveSender>,
    /// Messages per receive.
    batch: usize,
    /// The delivery throttle.
    throttle: &'a mut Throttle,
    /// Audit events.
    events: &'a EventLogger,
    /// Metrics.
    metrics: &'a Metrics,
    /// Shutdown.
    watcher: &'a ihb_runtime::shutdown::Watcher,
}

/// Run the receive loop, reopening the receiver whenever its session is
/// lost (§5.1.5).
///
/// Returns `false` when the receiver could not be reopened, which is the
/// one case the caller must exit non-zero on: a container that cannot
/// reach its own ingress should be restarted, not left healthy and idle.
async fn session_loop<D: Delivery>(state: SessionLoop<'_, D>) -> bool {
    let SessionLoop {
        bus,
        config,
        mut receiver,
        mut delivery,
        store,
        batch,
        throttle,
        events,
        metrics,
        watcher,
    } = state;

    loop {
        // §5.1.3 — a session receiver MUST have automatic lock renewal.
        let renewer = receiver.renew_session_lock();

        let ended_because = receive_loop(Loop {
            delivery: &mut delivery,
            receiver: &receiver,
            store: store.as_ref(),
            config,
            batch,
            throttle,
            events,
            metrics,
            watcher,
        })
        .await;

        // §5.1.4 — the renewer stops **before** the receiver closes, so no
        // renewal is in flight against a receiver being torn down.
        if let Some(renewer) = renewer {
            let ended = renewer.stop().await;
            if !ihb_bus::live::renewal_was_clean(ended) {
                tracing::warn!(?ended, "the session lock renewer ended badly");
            }
        }

        if let Err(error) = receiver.close().await {
            tracing::warn!(%error, "the receiver did not close cleanly");
        }

        if ended_because == LoopEnd::Stopped || !watcher.is_running() {
            return true;
        }

        let Some(fresh) = open_receiver(bus, config, watcher).await else {
            tracing::error!(
                entity = %config.ingress.entity(),
                "could not reopen the receiver after a lost session"
            );
            return false;
        };
        tracing::info!(
            entity = %config.ingress.entity(),
            "reopened the receiver after a lost session"
        );
        receiver = fresh;
    }
}

/// Everything the receive loop needs.
///
/// A struct because the loop takes nine things and an argument list that
/// long is one nobody reads.
struct Loop<'a, D> {
    /// How a message reaches its destination.
    ///
    /// Borrowed rather than owned: §5.1.5's reopen loop runs the receive
    /// loop more than once, and the delivery outlives any one session.
    delivery: &'a mut D,
    /// Where messages come from.
    receiver: &'a LiveReceiver,
    /// The audit copy, when this variant writes one.
    store: Option<&'a LiveSender>,
    /// This deployment.
    config: &'a Config,
    /// Messages per receive, after §2.3's reduction.
    batch: usize,
    /// The rate limit.
    throttle: &'a mut Throttle,
    /// Events.
    events: &'a EventLogger,
    /// Counters.
    metrics: &'a Metrics,
    /// Shutdown.
    watcher: &'a Watcher,
}

/// Receive and deliver until shutdown.
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

/// Receive and deliver until shutdown, or until the session is lost.
///
/// Returns [`LoopEnd::SessionLost`] so the caller reopens the receiver:
/// retrying on a receiver whose session is gone processes nothing forever
/// while the health check stays green (`spec/03-message-bus.md` §5.1.5).
async fn receive_loop<D: Delivery>(state: Loop<'_, D>) -> LoopEnd {
    let mut backoff = Backoff::new();

    // Everything that does not change between messages, assembled once.
    let context = Context {
        store: state.store,
        // §2.5 — the fallback for the store event's session, used only when
        // the message carries none. Replay puts the message back on the flow
        // it came from, which is the session it arrived on when the
        // receiver accepts any.
        store_session_id: state
            .config
            .ingress
            .session_id()
            .or(state.config.replay_session_id.as_deref())
            .unwrap_or_default()
            .to_string(),
        microservice_id: state.config.microservice_id.clone(),
        peer_service: state.config.peer_service.clone(),
        events: state.events,
        metrics: state.metrics,
    };

    while state.watcher.is_running() {
        let received = match state
            .receiver
            .receive(u32::try_from(state.batch).unwrap_or(u32::MAX))
            .await
        {
            Ok(received) => received,
            Err(error) if error.is_session_lost() => {
                // See `spec/03-message-bus.md` §5.1.5: this receiver is
                // dead and retrying it processes nothing forever while the
                // health check stays green.
                tracing::warn!(%error, "the session was lost; reopening the receiver");
                return LoopEnd::SessionLost;
            }
            Err(error) => {
                tracing::error!(%error, "receive failed");
                backoff.failed();
                if !state.watcher.sleep(backoff.delay()).await {
                    break;
                }
                continue;
            }
        };
        backoff.succeeded();

        for message in &received {
            state.metrics.message_received();

            let settlement = process_one(state.delivery, message, state.throttle, &context).await;

            // §1.1 — a message that was not positively acknowledged is
            // abandoned, never completed. The one exception is an `AR`,
            // which is dead-lettered (`spec/10-senders.md` §3.7.1): it is
            // moved aside for escalation, not discarded.
            let result = match &settlement {
                Settlement::Delivered => {
                    state.receiver.settle(message, Disposition::Accepted).await
                }
                Settlement::Retry => state.receiver.settle(message, Disposition::Deferred).await,
                Settlement::DeadLetter {
                    reason,
                    description,
                } => {
                    state
                        .receiver
                        .dead_letter(message, reason, description)
                        .await
                }
            };
            if let Err(error) = result {
                tracing::error!(%error, "could not settle a message");
            }

            if !settlement.is_delivered() {
                // Order within a session is the contract
                // (`spec/01-architecture.md` §6.2), so the rest of the
                // batch waits for the redelivery of this one.
                tracing::warn!("abandoning the rest of the batch behind an undelivered message");
                break;
            }
        }
    }

    LoopEnd::Stopped
}

/// Connect to Service Bus. `None` means the error was already logged
/// (without the connection string, which is a `Secret`); the caller treats
/// that as a failed startup rather than panicking.
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

/// Open the ingress receiver and, when `MESSAGE_STORE_QUEUE_NAME` is
/// configured, the message-store sender (`spec/10-senders.md` §5.2: only
/// the queue variant writes the audit copy). `None` if either fails, having
/// logged why.
async fn open(
    bus: &LiveBus,
    config: &Config,
    watcher: &Watcher,
) -> Option<(LiveReceiver, Option<LiveSender>)> {
    let receiver = open_receiver(bus, config, watcher).await?;

    let store = match config.message_store_queue.as_deref() {
        None => None,
        Some(queue) => Some(
            bus.sender(queue)
                .await
                .inspect_err(|error| {
                    tracing::error!(%error, queue, "could not open the message-store sender");
                })
                .ok()?,
        ),
    };

    Some((receiver, store))
}

/// Telemetry configuration, read straight from the environment.
///
/// Read from the environment directly rather than from [`Config`], so
/// telemetry set-up does not depend on the rest of the configuration
/// parsing cleanly. The default log level is the senders' `ERROR`
/// (`spec/02-configuration.md` §6.1).
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
        // `spec/02-configuration.md` §6.1 — the senders' default, unlike
        // the servers' INFO.
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "ERROR".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
