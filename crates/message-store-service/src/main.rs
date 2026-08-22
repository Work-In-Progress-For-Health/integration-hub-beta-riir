//! The `message-store-service` binary.
//!
//! Implements `spec/12-message-store.md` §4.

#![allow(clippy::large_futures)]

use std::process::ExitCode;

use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_bus::message::Outcome;
use ihb_config::Reader;
use ihb_runtime::backoff::Backoff;
use ihb_runtime::shutdown::Shutdown;
use ihb_sql::{SqlConfig, SqlConnection};
use ihb_telemetry::events::EventLogger;
use message_store_service::batch::{
    BatchOutcome, WORKFLOW_ID, build_all, report_failure, stored_at_now,
};
use message_store_service::insert::insert_all;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("message-store-service: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run())
}

/// This service's configuration.
struct Config {
    /// `INGRESS_QUEUE_NAME` — the message-store queue.
    ingress_queue_name: String,
    /// `MAX_BATCH_SIZE`.
    max_batch_size: usize,
    /// `MICROSERVICE_ID`.
    microservice_id: String,
    /// `HEALTH_BOARD`, a metric dimension.
    health_board: String,
    /// `PEER_SERVICE`, a metric dimension.
    peer_service: String,
    /// Where the health check binds.
    health_check: std::net::SocketAddr,
    /// `SERVICE_BUS_CONNECTION_STRING`.
    connection_string: Option<ihb_config::Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    namespace: Option<String>,
    /// `HL7_LOG_REDACTION_ENABLED`.
    redaction_enabled: bool,
    /// The database settings of §5.
    sql: SqlConfig,
}

/// Read everything, reporting every problem at once
/// (`spec/02-configuration.md` §1.4).
fn config() -> Result<Config, ihb_config::ConfigErrors> {
    let mut reader = Reader::from_env();

    let ingress_queue_name = reader.required_string("INGRESS_QUEUE_NAME");
    let microservice_id = reader.string_or("MICROSERVICE_ID", "message_store_service");
    let health_board = reader.string_or("HEALTH_BOARD", "UNKNOWN");
    let peer_service = reader.string_or("PEER_SERVICE", "SQL");
    let max_batch_size = reader.integer_or("MAX_BATCH_SIZE", 100_usize);

    let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
    let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

    let sql = SqlConfig::read(&mut reader)?;

    let config = Config {
        ingress_queue_name,
        max_batch_size,
        microservice_id,
        health_board,
        peer_service,
        health_check: std::net::SocketAddr::new(
            health_host
                .parse()
                .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            health_port.unwrap_or(ihb_health::DEFAULT_PORT),
        ),
        connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
        namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
        redaction_enabled: reader.bool_or("HL7_LOG_REDACTION_ENABLED", true),
        sql,
    };

    reader.finish()?;
    Ok(config)
}

/// Everything after the runtime exists.
async fn run() -> ExitCode {
    let config = match config() {
        Ok(config) => config,
        Err(errors) => {
            eprintln!("message-store-service: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    let telemetry = ihb_telemetry::otel::configure(&telemetry_config(&config.microservice_id));
    tracing::info!(
        queue = %config.ingress_queue_name,
        batch = config.max_batch_size,
        server = %config.sql.server,
        database = %config.sql.database,
        auth = config.sql.auth.describe(),
        export_enabled = telemetry.export_enabled,
        "message-store-service starting"
    );

    let Some(receiver) = open_receiver(&config).await else {
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

    let shutdown = Shutdown::new();
    shutdown.listen_for_signals();
    let watcher = shutdown.watcher();

    // §4.4 — the literal workflow id, not an environment variable.
    let events = EventLogger::new(WORKFLOW_ID, &config.microservice_id)
        .with_redaction(config.redaction_enabled);
    let metrics = ihb_telemetry::metrics::Metrics::new(
        WORKFLOW_ID,
        &config.microservice_id,
        &config.health_board,
        &config.peer_service,
    );

    // §5.1 — lazily opened, so a database that is briefly unreachable does
    // not stop this service starting and reporting healthy.
    let mut sql = SqlConnection::new(config.sql);
    let mut backoff = Backoff::new();

    while watcher.is_running() {
        let batch = match receiver
            .receive(u32::try_from(config.max_batch_size).unwrap_or(u32::MAX))
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

        for _ in &batch {
            metrics.message_received();
        }

        let outcome = process(&mut sql, &events, &batch).await;

        // §4.1 and §4.2 — all or nothing, for the whole batch.
        let disposition = if outcome.is_committed() {
            Outcome::Accepted
        } else {
            Outcome::Deferred
        };
        for message in &batch {
            if let Err(error) = receiver.settle(message, disposition).await {
                tracing::error!(%error, "could not settle a message");
            }
        }

        if let BatchOutcome::Committed { count } = outcome {
            for _ in 0..count {
                metrics.message_sent();
            }
        }
    }

    if let Err(error) = receiver.close().await {
        tracing::warn!(%error, "the receiver did not close cleanly");
    }
    health.stop().await;

    tracing::info!("message-store-service stopped");
    ExitCode::SUCCESS
}

/// Connect and open the ingress receiver, reporting why not.
///
/// `spec/01-architecture.md` §2.3.3 — the message-store queue is **not**
/// session-enabled, so this is a plain receiver rather than a session one.
async fn open_receiver(config: &Config) -> Option<ihb_bus::live::LiveReceiver> {
    let endpoint = Endpoint::from_config(
        config
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.namespace.as_deref(),
    )
    .inspect_err(|error| tracing::error!(%error, "Service Bus is not configured"))
    .ok()?;

    let bus = LiveBus::connect(&endpoint)
        .await
        .inspect_err(|error| tracing::error!(%error, "could not reach Service Bus"))
        .ok()?;

    bus.queue_receiver(&config.ingress_queue_name)
        .await
        .inspect_err(|error| {
            tracing::error!(%error, queue = %config.ingress_queue_name, "could not open the receiver");
        })
        .ok()
}

/// Build, insert and commit one batch.
async fn process(
    sql: &mut SqlConnection,
    events: &EventLogger,
    batch: &[ihb_bus::message::Incoming],
) -> BatchOutcome {
    let records = match build_all(batch) {
        Ok(records) => records,
        Err(reason) => return report_failure(events, batch.len(), &reason),
    };

    // §2.2 — one timestamp for the whole batch.
    let stored_at = stored_at_now();
    let count = records.len();

    let inserted = sql
        .transaction(async |client| insert_all(client, &records, stored_at).await)
        .await;

    match inserted {
        Ok(()) => {
            tracing::info!(count, "stored a batch");
            BatchOutcome::Committed { count }
        }
        Err(error) => report_failure(events, batch.len(), &error.to_string()),
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
        service_name: read("OTEL_SERVICE_NAME").unwrap_or_else(|| default_service.to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
