//! The `hl7-topic-mock-receiver` binary.
//!
//! Implements `spec/11-mock-receivers.md` §5.
//!
//! [`hl7-mock-receiver`](../main.rs) with a topic for a destination, so
//! several subscriptions can consume what it was sent. The destination is
//! optional (§5.2): with no topic, or no connection, it acknowledges and
//! logs, which is how a developer's tooling runs it standalone.

// Startup calls `azservicebus` through `ihb-bus`, whose futures are ~32 KB.
// `ihb_bus::live` records why that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;
use std::sync::Arc;

use hl7_mock_receiver::server::{Receiver, serve};
use hl7_mock_receiver::sink::LoggingSink;
use hl7_mock_receiver::topic::TopicConfig;
use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_runtime::shutdown::Shutdown;

/// The name this reports itself as.
const NAME: &str = "hl7-topic-mock-receiver";

fn main() -> ExitCode {
    // Before Tokio spawns any thread; see `ihb_fields::timestamp`.
    let local = ihb_fields::timestamp::capture_local_offset();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{NAME}: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run(local))
}

/// Everything after the runtime exists.
async fn run(local_offset_found: bool) -> ExitCode {
    let config = match TopicConfig::from_env() {
        Ok(config) => config,
        Err(errors) => {
            eprintln!("{NAME}: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    // `spec/18-deployment.md` §4.3.
    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }
    tracing::info!(
        listen = %config.listen,
        topic = ?config.egress_topic_name,
        session = ?config.egress_session_id,
        forwards = config.forwards(),
        export_enabled = telemetry.export_enabled,
        local_offset_found,
        "{NAME} starting"
    );

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();

    let listen = config.listen;
    let health_address = config.health_check;
    let health = match ihb_health::HealthCheck::start(
        &health_address.ip().to_string(),
        health_address.port(),
    )
    .await
    {
        Ok(check) => check,
        Err(error) => {
            tracing::error!(%error, %health_address, "could not start the health check");
            return ExitCode::FAILURE;
        }
    };

    // The two shapes differ only in the sink; see the same choice in
    // `hl7-mock-receiver`'s `main`.
    let forward_to = config
        .egress_topic_name
        .clone()
        .filter(|_| config.forwards());
    let served = if let Some(topic) = forward_to {
        match connect(&config, &topic).await {
            Ok(sender) => {
                let receiver = Arc::new(Receiver::new(
                    sender,
                    config.egress_session_id.clone(),
                    config.max_message_size,
                ));
                serve(listen, receiver, shutdown.watcher()).await
            }
            Err(code) => return code,
        }
    } else {
        // §5.2 — a topic with no connection warns; nothing configured is
        // the normal standalone case and says so at info.
        if config.egress_topic_name.is_some() {
            tracing::warn!(
                "EGRESS_TOPIC_NAME is set but no Service Bus connection is; \
                 messages will be acknowledged and logged, not forwarded"
            );
        } else {
            tracing::warn!(
                "Service Bus not configured (EGRESS_TOPIC_NAME unset); \
                 messages will be acknowledged and logged, not forwarded"
            );
        }
        let receiver = Arc::new(Receiver::new(
            LoggingSink::new(config.egress_topic_name.clone().unwrap_or_default()),
            config.egress_session_id.clone(),
            config.max_message_size,
        ));
        serve(listen, receiver, shutdown.watcher()).await
    };

    health.stop().await;

    match served {
        Ok(()) => {
            tracing::info!("{NAME} stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, %listen, "the MLLP listener failed");
            ExitCode::FAILURE
        }
    }
}

/// Open the topic sender, or say why not.
async fn connect(config: &TopicConfig, topic: &str) -> Result<ihb_bus::live::LiveSender, ExitCode> {
    let endpoint = Endpoint::from_config(
        config
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.namespace.as_deref(),
    )
    .map_err(|error| {
        tracing::error!(%error, "Service Bus is not configured");
        ExitCode::FAILURE
    })?;

    let bus = LiveBus::connect(&endpoint).await.map_err(|error| {
        tracing::error!(%error, "could not reach Service Bus");
        ExitCode::FAILURE
    })?;

    // A topic is sent to exactly as a queue is.
    bus.sender(topic).await.map_err(|error| {
        tracing::error!(%error, %topic, "could not open the sender");
        ExitCode::FAILURE
    })
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
            .unwrap_or_else(|| NAME.to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
