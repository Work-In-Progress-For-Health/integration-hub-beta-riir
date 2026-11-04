//! The `message-replay-job` binary.
//!
//! Implements `spec/13-message-replay.md` §1 and §7.
//!
//! A job: it runs to completion and exits 0 or 1. No health check, no
//! signal handling, no backoff — a job that cannot do its work should stop
//! and let an operator look at it.
//!
//! The loop here is §1.3's fetch, send, mark; the per-step retry rules
//! (§5) and statements (§3, §6) are in the library, so this file is only
//! configuration, connections and the order of the three steps.

// Startup and the send call `azservicebus` through `ihb-bus`, whose futures
// are ~32 KB. `ihb_bus::live` records why that is allowed rather than boxed
// away.
#![allow(clippy::large_futures)]

use std::process::ExitCode;

use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_config::Reader;
use ihb_sql::{SqlConfig, SqlConnection};
use message_replay_job::job::{fetch, log_page, mark, mark_failed, send};
use message_replay_job::sql::Status;
use uuid::Uuid;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("message-replay-job: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run())
}

/// This job's configuration (`spec/02-configuration.md` §7.2).
struct Config {
    /// `REPLAY_BATCH_ID` — the one batch this execution replays (§1.2).
    batch_id: Uuid,
    /// `REPLAY_BATCH_SIZE` — rows per page (default 100). `i32` because it
    /// is bound as the `TOP (@P1)` parameter, which TDS types as `int`.
    /// Read with `integer_or`, so `0` is accepted although
    /// `spec/02-configuration.md` §7.2 says "must be a positive integer";
    /// a `0` fetches nothing and the job reports success having replayed
    /// nothing.
    batch_size: i32,
    /// `PRIORITY_QUEUE_NAME` — where replayed messages go.
    priority_queue_name: String,
    /// `SERVICE_BUS_CONNECTION_STRING`.
    connection_string: Option<ihb_config::Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    namespace: Option<String>,
    /// The database settings of `spec/12-message-store.md` §5.
    sql: SqlConfig,
}

/// Read everything, reporting every problem at once
/// (`spec/02-configuration.md` §1.4).
///
/// # Errors
///
/// Every unusable variable, together; a `REPLAY_BATCH_ID` that is not a
/// UUID is among them (§7.2 of that spec).
fn config() -> Result<Config, ihb_config::ConfigErrors> {
    let mut reader = Reader::from_env();

    // §7.2 — a value that is not a UUID is a startup failure. A job pointed
    // at a batch that cannot exist would report "nothing to replay" and
    // exit 0, which is the worst possible answer.
    let batch_id = reader.required_uuid("REPLAY_BATCH_ID");
    let priority_queue_name = reader.required_string("PRIORITY_QUEUE_NAME");
    let batch_size: usize = reader.integer_or("REPLAY_BATCH_SIZE", 100_usize);

    let sql = SqlConfig::read(&mut reader)?;

    let config = Config {
        batch_id,
        batch_size: i32::try_from(batch_size).unwrap_or(i32::MAX),
        priority_queue_name,
        connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
        namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
        sql,
    };

    reader.finish()?;
    Ok(config)
}

/// Everything after the runtime exists.
///
/// The exit code is the job's whole result to the scheduler: `SUCCESS` only
/// when a fetch returned no rows, which is how the loop knows every page of
/// the batch was sent and marked `Loaded`. Any other exit leaves unmarked
/// rows `Pending` or `Failed`, and re-running the job is the recovery
/// (`spec/13-message-replay.md` §2.2).
async fn run() -> ExitCode {
    let config = match config() {
        Ok(config) => config,
        Err(errors) => {
            eprintln!("message-replay-job: configuration is not usable:\n{errors}");
            return ExitCode::FAILURE;
        }
    };

    // The outcome is not inspected here: this is a batch job, and
    // `spec/04-observability.md` §6.2 makes a telemetry failure non-fatal,
    // so there is nothing to branch on.
    let _telemetry = ihb_telemetry::otel::configure(&telemetry_config());

    // `spec/18-deployment.md` §4.3 — a configured CA bundle that cannot be
    // used stops the service here, rather than becoming a TLS failure in
    // whichever component happens to need it first.
    if !ihb_tls::startup_check() {
        return ExitCode::FAILURE;
    }
    tracing::info!(
        batch_id = %config.batch_id,
        batch_size = config.batch_size,
        queue = %config.priority_queue_name,
        server = %config.sql.server,
        "message-replay-job starting"
    );

    let endpoint = match Endpoint::from_config(
        config
            .connection_string
            .as_ref()
            .map(ihb_config::Secret::expose),
        config.namespace.as_deref(),
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::error!(%error, "Service Bus is not configured");
            return ExitCode::FAILURE;
        }
    };

    let bus = match LiveBus::connect(&endpoint).await {
        Ok(bus) => bus,
        Err(error) => {
            tracing::error!(%error, "could not reach Service Bus");
            return ExitCode::FAILURE;
        }
    };

    let sink = match bus.sender(&config.priority_queue_name).await {
        Ok(sender) => sender,
        Err(error) => {
            tracing::error!(%error, queue = %config.priority_queue_name, "could not open the sender");
            return ExitCode::FAILURE;
        }
    };

    let mut sql = SqlConnection::new(config.sql);
    let mut page_number = 0_usize;

    // §1.3 — fetch, send, mark, repeat, until a fetch returns nothing.
    loop {
        page_number += 1;

        let page = match fetch(&mut sql, config.batch_id, config.batch_size).await {
            Ok(page) => page,
            Err(error) => {
                tracing::error!(%error, "the replay job could not fetch");
                return ExitCode::FAILURE;
            }
        };

        if page.is_empty() {
            // §7.2's completion lines.
            tracing::info!("No more pending records found. Job complete.");
            tracing::info!("Message replay job finished successfully");
            return ExitCode::SUCCESS;
        }

        log_page(page_number, config.batch_id, &page);

        if let Err(error) = send(&sink, &page).await {
            tracing::error!(%error, "the replay job could not send a page");
            // §5.2.1 and §5.2.2 — mark the page `Failed` so a re-run picks
            // it up (§2.2), and do not let a failure doing so mask this
            // error.
            mark_failed(&mut sql, &page).await;
            return ExitCode::FAILURE;
        }

        match mark(&mut sql, &page, Status::Loaded).await {
            Ok(count) => {
                tracing::info!("Batch {page_number}: {count} records marked as Loaded");
            }
            Err(error) => {
                tracing::error!(%error, "the replay job could not mark a page");
                return ExitCode::FAILURE;
            }
        }
    }
}

/// Telemetry configuration, read straight from the environment.
///
/// Outside the validated [`config`] so telemetry is up before the rest of
/// the configuration can fail, and a bad value there can be logged.
fn telemetry_config() -> ihb_telemetry::otel::Config {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    ihb_telemetry::otel::Config {
        service_name: read("OTEL_SERVICE_NAME").unwrap_or_else(|| "message_replay_job".to_string()),
        log_level: read("LOG_LEVEL").unwrap_or_else(|| "INFO".to_string()),
        azure_log_level: read("AZURE_LOG_LEVEL").unwrap_or_else(|| "WARN".to_string()),
        rust_log: read("RUST_LOG"),
        connection_string: read("APPLICATIONINSIGHTS_CONNECTION_STRING"),
    }
}
