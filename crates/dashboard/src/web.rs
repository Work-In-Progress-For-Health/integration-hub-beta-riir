//! The pages and the JSON API.
//!
//! Implements `spec/15-dashboard.md` §2.
//!
//! # Every route works with no Azure at all
//!
//! §1.2, and it is structural rather than defensive: the application is
//! generic over an [`Observatory`], and [`Unconfigured`] is a perfectly
//! good one. There is no "is Azure configured" branch in any handler —
//! there is only data that may say it does not know.
//!
//! [`Unconfigured`]: crate::observe::Unconfigured

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::alarm::Trip;
use crate::alarm::rules::{
    FailureThreshold, InactivityThresholds, Row, Summary, evaluate_all, failure_trip,
    inactivity_trip,
};
use crate::alarm::store::{AlarmStore, Partition};
use crate::cache::Cached;
use crate::config::Config;
use crate::flows::FLOWS;
use crate::language::{Language, safe_redirect};
use crate::observe::{MessageEvent, Observatory};
use crate::status::Status;

/// The bundled stylesheet.
const STYLESHEET: &str = include_str!("../static/dashboard.css");

/// Everything the handlers share.
pub struct AppState<O, S = crate::alarm::store::Disabled> {
    /// Where the Azure data comes from.
    pub azure: O,
    /// Where alarm configuration and state live (§5.6).
    pub alarms: S,
    /// What we were configured with.
    pub config: Config,
    /// §2.2.1's shared 30-second cache.
    pub status: Cached<Status>,
}

impl<O: Observatory> AppState<O, crate::alarm::store::Disabled> {
    /// Build state around an observatory, with alarms not persisted.
    ///
    /// §5.6's first row, which §1.2 makes the normal state on a laptop.
    pub fn new(azure: O, config: Config) -> Self {
        Self::with_alarms(azure, crate::alarm::store::Disabled, config)
    }
}

impl<O: Observatory, S: AlarmStore> AppState<O, S> {
    /// Build state around an observatory and an alarm store.
    pub fn with_alarms(azure: O, alarms: S, config: Config) -> Self {
        let ttl = config.api_cache_ttl;
        Self {
            azure,
            alarms,
            config,
            status: Cached::new(ttl),
        }
    }

    /// Evaluate all three alarms (§5.1.2, §5.3–§5.5).
    ///
    /// One Log Analytics query per alarm, for every workflow that alarm has
    /// a rule for — not one per rule. §4.3.1's queries take an `in (...)`
    /// list precisely so a dozen rules are one query, and a workspace
    /// charges per query.
    ///
    /// A partition whose store could not be read comes back named in
    /// [`AlarmView::failed`] rather than silently contributing nothing:
    /// §5.6.3, because one alarm's outage must not blank the other two and
    /// must not look like having no rules.
    async fn evaluate_alarms(&self, now: time::OffsetDateTime) -> AlarmView {
        let mut view = AlarmView::default();

        for partition in Partition::all() {
            // The rules first, so the queries ask about exactly the
            // workflows that are configured.
            let rules = match self.alarms.rules(partition).await {
                Ok(rules) => rules,
                Err(error) => {
                    tracing::warn!(%error, alarm = partition.as_str(), "the alarm store failed");
                    view.failed.push(partition.as_str());
                    continue;
                }
            };

            let workflow_ids: Vec<String> = rules
                .iter()
                .filter(|rule| rule.is_active())
                .map(|rule| rule.workflow_id.clone())
                .collect();

            let measurement = self.measure(partition, &workflow_ids).await;

            let evaluated = evaluate_all(&self.alarms, partition, now, |rule| {
                measurement.trip_for(rule, now)
            })
            .await;

            match evaluated {
                Ok(rows) => view.push(partition, rows),
                Err(error) => {
                    tracing::warn!(%error, alarm = partition.as_str(), "the alarm store failed");
                    view.failed.push(partition.as_str());
                }
            }
        }

        view
    }

    /// The status payload with §2.3.1's three alarm summaries added.
    ///
    /// `dashboard.js :: refreshData` reads `data.alarm1_summary` and its
    /// two siblings. They are merged here rather than built into
    /// [`Status`] because evaluating an alarm needs the store, which the
    /// status builder deliberately does not have — it is pure so §3's
    /// health logic is testable without one.
    ///
    /// Not covered by §2.2.1's cache: that exists for the Azure fan-out,
    /// and an alarm's cooldown moving between two renders is exactly what
    /// an operator is watching for.
    async fn with_alarm_summaries(&self, status: &Status) -> serde_json::Value {
        let mut body = serde_json::to_value(status).unwrap_or_else(|_| serde_json::json!({}));
        let view = self.evaluate_alarms(now_utc()).await;

        if let Some(object) = body.as_object_mut() {
            for partition in Partition::all() {
                let rows: Vec<Row> = view
                    .partitions
                    .iter()
                    .filter(|(name, _)| *name == partition.as_str())
                    .flat_map(|(_, rows)| rows.iter().cloned())
                    .collect();

                object.insert(
                    format!("{}_summary", partition.as_str()),
                    serde_json::to_value(Summary::of(&rows))
                        .unwrap_or_else(|_| serde_json::json!({})),
                );
            }
        }

        body
    }

    /// Ask Azure what this alarm needs to know (§4.3.1).
    async fn measure(&self, partition: Partition, workflow_ids: &[String]) -> Measurement {
        if workflow_ids.is_empty() {
            // Nothing configured, so nothing to ask. `Absent` rather than
            // an empty answer: there is no rule to report on either way.
            return Measurement::Absent;
        }

        match partition {
            Partition::Alarm1 => Measurement::Inactivity {
                last: self.azure.last_received(workflow_ids).await,
                thresholds: InactivityThresholds::default(),
            },
            Partition::Alarm2 => Measurement::Inactivity {
                last: self.azure.last_sent(workflow_ids).await,
                thresholds: InactivityThresholds::default(),
            },
            Partition::Alarm3 => {
                let settings = FailureThreshold::default();
                Measurement::Failures {
                    counts: self
                        .azure
                        .failure_counts(workflow_ids, settings.window_minutes)
                        .await,
                    settings,
                }
            }
        }
    }

    /// Fetch everything and build §2.2's payload.
    ///
    /// The four Azure calls run concurrently: on a cold cache this is one
    /// round-trip rather than four, and §2.2.2 gives the page a deadline.
    async fn build_status(&self) -> Status {
        let queues: Vec<String> = self.queue_names();

        let (depths, containers, exceptions, retry_delays) = tokio::join!(
            self.azure.queue_depths(&queues),
            self.azure.containers(),
            self.azure.exceptions(1),
            self.azure.retry_delays(1),
        );

        crate::status::build(
            &depths,
            &containers,
            &exceptions,
            &retry_delays,
            self.config.thresholds,
            self.azure.is_configured(),
            now(),
        )
    }

    /// The queues to ask about.
    ///
    /// One per flow, named from the flow's `WORKFLOW_ID`. A real deployment
    /// resolves these from the environment (`spec/01-architecture.md`
    /// §2.1.1); this is what an unconfigured one asks for so that every
    /// flow appears on the page as Unknown rather than being absent.
    fn queue_names(&self) -> Vec<String> {
        FLOWS
            .iter()
            .map(|flow| flow.workflow_id.to_string())
            .collect()
    }
}

/// The routes of §2.
pub fn router<O: Observatory, S: AlarmStore>(state: Arc<AppState<O, S>>) -> Router {
    Router::new()
        // §2.1's pages.
        .route("/", get(overview::<O, S>))
        .route("/flows", get(flows_page::<O, S>))
        .route("/exceptions", get(exceptions_page::<O, S>))
        .route("/service-bus", get(service_bus_page::<O, S>))
        .route("/alarms", get(alarms_page::<O, S>))
        .route("/messages", get(messages_page::<O, S>))
        .route("/trace/{operation_id}", get(trace_page::<O, S>))
        .route("/set-language", post(set_language))
        // §2.2's JSON API.
        .route("/healthz", get(healthz))
        .route("/api/status", get(api_status::<O, S>))
        .route("/api/refresh", post(api_refresh::<O, S>))
        .route("/api/refresh", get(api_refresh::<O, S>))
        .route("/api/flows", get(api_flows::<O, S>))
        .route("/api/servicebus-metrics", get(api_service_bus::<O, S>))
        .route("/api/hl7-throughput", get(api_throughput::<O, S>))
        .route("/api/messages", get(api_messages::<O, S>))
        .route("/api/alarms/status", get(api_alarms::<O, S>))
        .route("/static/dashboard.css", get(stylesheet))
        .with_state(state)
}

/// `/set-language`'s form body (§6.2).
#[derive(Debug, Default, Deserialize)]
pub struct LanguageForm {
    /// `en` or `cy`. Anything else falls back to English.
    #[serde(default)]
    pub lang: Option<String>,
}

/// How long the language preference is remembered.
const LANGUAGE_COOKIE_MAX_AGE: u32 = 60 * 60 * 24 * 365;

/// `POST /set-language` (§2.1, §6).
///
/// Stores the choice in a cookie and returns to where the form was posted
/// from — via [`safe_redirect`], because the `Referer` is client-supplied
/// (§6.4).
///
/// A cookie rather than a session: the preference is not a secret, and
/// §9.3 means this deployment may have no stable session key at all.
async fn set_language(
    headers: axum::http::HeaderMap,
    axum::extract::Form(form): axum::extract::Form<LanguageForm>,
) -> Response {
    let language = Language::parse(form.lang.as_deref().unwrap_or_default());

    let referer = headers
        .get(axum::http::header::REFERER)
        .and_then(|value| value.to_str().ok());

    let cookie = format!(
        "lang={}; Path=/; Max-Age={LANGUAGE_COOKIE_MAX_AGE}; SameSite=Lax; HttpOnly",
        language.code()
    );

    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, safe_redirect(referer)),
            (header::SET_COOKIE, cookie),
        ],
    )
        .into_response()
}

/// `/healthz` — liveness (§2.2).
///
/// Deliberately independent of Azure. A dashboard that reported itself
/// unhealthy because a *watched* service was unreachable would be
/// restarted by its orchestrator at exactly the moment somebody needed to
/// look at it.
async fn healthz() -> Response {
    Json(serde_json::json!({ "status": "ok" })).into_response()
}

/// `/api/status` (§2.2.1), from the shared cache.
async fn api_status<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let status = state.status.get_or_refresh(|| state.build_status()).await;

    Json(state.with_alarm_summaries(&status).await).into_response()
}

/// `/api/refresh` (§2.2) — rebuild, then answer.
async fn api_refresh<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let status = state.status.refresh(|| state.build_status()).await;

    Json(state.with_alarm_summaries(&status).await).into_response()
}

/// `/api/flows` (§2.2).
async fn api_flows<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let status = state.status.get_or_refresh(|| state.build_status()).await;

    Json(serde_json::json!({
        "flows": status.flows,
        "azure_configured": status.azure_configured,
    }))
    .into_response()
}

/// `/api/servicebus-metrics` (§2.2, §2.3.2).
///
/// Two named series, as `sb-chart.js` reads them — plus the per-queue
/// depths, which the Service Bus page renders.
async fn api_service_bus<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(query): Query<ThroughputQuery>,
) -> Response {
    let hours = query.hours_or_default();

    let queues = state.queue_names();
    let (depths, series) = tokio::join!(
        state.azure.queue_depths(&queues),
        state.azure.throughput(hours, None, None),
    );

    let rows: Vec<serde_json::Value> = depths
        .iter()
        .map(|(name, depths)| {
            serde_json::json!({
                "name": name,
                // Null, not zero: §2.2.2.
                "active_message_count": depths.active,
                "dead_letter_message_count": depths.dead_letter,
                "known": depths.is_known(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "incoming": series.incoming,
        "outgoing": series.outgoing,
        "timespan": series.timespan,
        "queues": rows,
        "azure_configured": state.azure.is_configured(),
    }))
    .into_response()
}

/// `/api/hl7-throughput`'s query string.
///
/// Every field is lenient. §1.2 makes this a page rather than a 400: a
/// bookmarked URL from an older version, or one somebody typed, must render
/// with the defaults instead of refusing.
#[derive(Debug, Default, Deserialize)]
pub struct ThroughputQuery {
    /// Hours to look back.
    ///
    /// Read as text and parsed here, because `Option<u32>` makes `axum`'s
    /// extractor reject `hours=banana` with a `400` before any handler
    /// runs — and a 400 is not a page.
    #[serde(default)]
    pub hours: Option<String>,
    /// Narrow to one health board.
    #[serde(default)]
    pub health_board: Option<String>,
    /// Narrow to one flow.
    #[serde(default)]
    pub workflow_id: Option<String>,
}

impl ThroughputQuery {
    /// The window asked for, bounded (§2.3.3).
    fn hours_or_default(&self) -> u32 {
        self.hours
            .as_deref()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_THROUGHPUT_HOURS)
            .clamp(1, MAX_THROUGHPUT_HOURS)
    }
}

/// How many hours the throughput chart shows by default.
const DEFAULT_THROUGHPUT_HOURS: u32 = 24;

/// The most a caller may ask for.
///
/// A bound rather than trust: `hours` reaches a KQL `ago({hours}h)`, and an
/// unbounded value is a query that scans the whole workspace retention and
/// times the page out. §4.4 sanitises identifiers; this is the same idea
/// for a number.
const MAX_THROUGHPUT_HOURS: u32 = 24 * 30;

/// `/api/hl7-throughput` (§2.2).
async fn api_throughput<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(query): Query<ThroughputQuery>,
) -> Response {
    let hours = query.hours_or_default();

    let series = state
        .azure
        .throughput(
            hours,
            query.health_board.as_deref(),
            query.workflow_id.as_deref(),
        )
        .await;

    // §2.3.2 — the browser reads `incoming` and `outgoing` off the top
    // level. Nesting them under a `points` key draws an empty chart with
    // nothing in the console.
    let mut body = serde_json::to_value(&series).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "azure_configured".to_string(),
            serde_json::json!(state.azure.is_configured()),
        );
    }

    Json(body).into_response()
}

/// The overview (§2.1).
#[derive(Template)]
#[template(path = "overview.html")]
struct OverviewPage {
    /// The payload the page renders.
    status: Status,
}

/// The flow table (§2.1).
#[derive(Template)]
#[template(path = "flows.html")]
struct FlowsPage {
    /// The payload the page renders.
    status: Status,
}

/// The exceptions page (§2.1).
#[derive(Template)]
#[template(path = "exceptions.html")]
struct ExceptionsPage {
    /// What came back, newest first.
    exceptions: Vec<crate::observe::Exception>,
    /// Whether Azure could be reached at all (§2.2.2).
    configured: bool,
    /// How far back this looked.
    hours: u32,
}

/// The queue depths (§2.1).
#[derive(Template)]
#[template(path = "service-bus.html")]
struct ServiceBusPage {
    /// One row per queue.
    queues: Vec<QueueRow>,
    /// Whether Azure could be reached at all.
    configured: bool,
}

/// One queue, as the page shows it.
struct QueueRow {
    /// The queue's name.
    name: String,
    /// Messages waiting, or `None`.
    active: Option<u64>,
    /// Dead-lettered messages, or `None`.
    dead_letter: Option<u64>,
}

/// The message search (§2.1).
///
/// # Every payload here is redacted
///
/// §1.3. The template renders [`MessageRow::payload`], which is already
/// through [`MessageEvent::rendered`] — the unredacted text does not reach
/// this struct at all, so a template cannot show it by accident.
#[derive(Template)]
#[template(path = "messages.html")]
struct MessagesPage {
    /// The events, newest first.
    messages: Vec<MessageRow>,
    /// Whether Azure could be reached at all.
    configured: bool,
    /// Which flow was asked for, if any.
    workflow_id: Option<String>,
    /// Whether redaction is on, so the page can say so.
    redacted: bool,
}

/// One distributed trace (§2.1).
#[derive(Template)]
#[template(path = "trace.html")]
struct TracePage {
    /// The operation id.
    operation_id: String,
    /// Its events, in order.
    messages: Vec<MessageRow>,
    /// Whether Azure could be reached at all.
    configured: bool,
    /// Whether redaction is on.
    redacted: bool,
}

/// One message event, as a page may show it.
///
/// Deliberately not [`MessageEvent`]: the payload here has already been
/// through §1.3's redaction, and there is no field carrying the original.
struct MessageRow {
    /// When, RFC 3339.
    timestamp: String,
    /// The event type.
    event_type: String,
    /// Which flow.
    workflow_id: String,
    /// Which service.
    microservice_id: String,
    /// The correlation id.
    correlation_id: String,
    /// The operation id, which links to the trace page.
    operation_id: String,
    /// The payload, **already redacted** unless the deployment turned
    /// redaction off.
    payload: Option<String>,
}

impl MessageRow {
    /// Redact an event and keep only what a page may show (§1.3).
    fn from(event: &MessageEvent, redact: bool) -> Self {
        Self {
            timestamp: event.timestamp.clone(),
            event_type: event.event_type.clone(),
            workflow_id: event.workflow_id.clone(),
            microservice_id: event.microservice_id.clone(),
            correlation_id: event.correlation_id.clone(),
            operation_id: event.operation_id.clone(),
            payload: event.rendered(redact),
        }
    }
}

/// What one alarm's query came back with.
///
/// The `Option` in each variant is §4.5's distinction: `None` means the
/// query did not run, which is **unknown**. It is not an empty result.
enum Measurement {
    /// Nothing was asked, because nothing is configured.
    Absent,
    /// Alarm 1 and 2: the last activity per workflow.
    Inactivity {
        /// `None` when the query did not run.
        last: Option<std::collections::BTreeMap<String, time::OffsetDateTime>>,
        /// The thresholds to compare against.
        thresholds: InactivityThresholds,
    },
    /// Alarm 3: failure counts per workflow.
    Failures {
        /// `None` when the query did not run.
        counts: Option<std::collections::BTreeMap<String, u64>>,
        /// The window and threshold.
        settings: FailureThreshold,
    },
}

impl Measurement {
    /// Whether one rule has tripped (§5.3.2, §5.4.2, §5.5.2).
    fn trip_for(&self, rule: &crate::alarm::RuleConfig, now: time::OffsetDateTime) -> Trip {
        match self {
            Self::Absent => Trip::NoData,

            Self::Inactivity { last, thresholds } => {
                // A query that did not run and a workflow with no activity
                // in the window are both `NoData` here — §5.3.3 makes "no
                // trace in thirty days" unknown rather than critical, which
                // is the same answer.
                let seen = last
                    .as_ref()
                    .and_then(|found| found.get(&rule.workflow_id).copied());

                inactivity_trip(seen, now, *thresholds)
            }

            Self::Failures { counts, settings } => {
                // THE distinction. A query that did not run is unknown; a
                // workflow **absent from a query that did run** had no
                // failures, which is zero and healthy. `summarize count()`
                // emits no row for a workflow with nothing to count.
                let count = counts
                    .as_ref()
                    .map(|found| found.get(&rule.workflow_id).copied().unwrap_or(0));

                failure_trip(count, *settings)
            }
        }
    }
}

/// What all three alarms are doing (§5).
///
/// A partition whose store could not be read is named in `failed` rather
/// than silently contributing nothing — §5.6's outage and §5.6's "nothing
/// configured" must not look the same on the page, for the reason
/// `alarm::store` gives.
#[derive(Debug, Default)]
pub struct AlarmView {
    /// The rows, per partition, worst first.
    pub partitions: Vec<(&'static str, Vec<Row>)>,
    /// Partitions whose store could not be read.
    pub failed: Vec<&'static str>,
}

impl AlarmView {
    /// Record a partition's rows.
    fn push(&mut self, partition: Partition, rows: Vec<Row>) {
        self.partitions.push((partition.as_str(), rows));
    }

    /// Every row, across the three alarms.
    #[must_use]
    pub fn all_rows(&self) -> Vec<&Row> {
        self.partitions
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .collect()
    }

    /// A count of what every rule is doing.
    #[must_use]
    pub fn summary(&self) -> Summary {
        let owned: Vec<Row> = self.all_rows().into_iter().cloned().collect();
        Summary::of(&owned)
    }
}

/// The alarm page (§2.1).
#[derive(Template)]
#[template(path = "alarms.html")]
struct AlarmsPage {
    /// Whether alarm state is persisted at all (§5.6).
    persistence: &'static str,
    /// What every rule is doing.
    summary: Summary,
    /// The rules, per alarm.
    partitions: Vec<AlarmPartition>,
    /// Alarms whose store could not be read.
    failed: Vec<&'static str>,
}

/// One alarm's rules, as the page shows them.
struct AlarmPartition {
    /// `alarm1`, `alarm2` or `alarm3`.
    name: &'static str,
    /// Its rules, worst first.
    rules: Vec<AlarmRow>,
}

/// One rule, as the page shows it.
struct AlarmRow {
    /// What an operator reads.
    display_name: String,
    /// Which flow it watches.
    workflow_id: String,
    /// What it is doing.
    status: crate::alarm::Status,
}

async fn overview<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let status = state.status.get_or_refresh(|| state.build_status()).await;

    page(&OverviewPage { status })
}

async fn flows_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let status = state.status.get_or_refresh(|| state.build_status()).await;

    page(&FlowsPage { status })
}

/// How far back the exceptions page looks.
const EXCEPTION_HOURS: u32 = 24;

async fn exceptions_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let exceptions = state.azure.exceptions(EXCEPTION_HOURS).await;

    page(&ExceptionsPage {
        exceptions,
        configured: state.azure.is_configured(),
        hours: EXCEPTION_HOURS,
    })
}

async fn service_bus_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let names = state.queue_names();
    let depths = state.azure.queue_depths(&names).await;

    page(&ServiceBusPage {
        queues: depths
            .into_iter()
            .map(|(name, depths)| QueueRow {
                name,
                active: depths.active,
                dead_letter: depths.dead_letter,
            })
            .collect(),
        configured: state.azure.is_configured(),
    })
}

/// `/messages`'s query string.
#[derive(Debug, Default, Deserialize)]
pub struct MessageQuery {
    /// Narrow to one flow.
    #[serde(default)]
    pub workflow_id: Option<String>,
}

async fn messages_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(query): Query<MessageQuery>,
) -> Response {
    let workflow_id = query.workflow_id.filter(|id| !id.trim().is_empty());
    let events = state.azure.messages(workflow_id.as_deref()).await;
    let redact = state.config.redact;

    page(&MessagesPage {
        messages: events
            .iter()
            .map(|event| MessageRow::from(event, redact))
            .collect(),
        configured: state.azure.is_configured(),
        workflow_id,
        redacted: redact,
    })
}

async fn trace_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(operation_id): Path<String>,
) -> Response {
    let events = state.azure.trace(&operation_id).await;
    let redact = state.config.redact;

    page(&TracePage {
        operation_id,
        messages: events
            .iter()
            .map(|event| MessageRow::from(event, redact))
            .collect(),
        configured: state.azure.is_configured(),
        redacted: redact,
    })
}

/// `/api/messages` (§2.2).
///
/// The payload is **not** in this response — [`MessageEvent`] does not
/// serialise it (§1.3). A caller wanting message content reads the page,
/// where redaction is applied.
async fn api_messages<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(query): Query<MessageQuery>,
) -> Response {
    let events = state
        .azure
        .messages(
            query
                .workflow_id
                .as_deref()
                .filter(|id| !id.trim().is_empty()),
        )
        .await;

    Json(serde_json::json!({
        "messages": events,
        "azure_configured": state.azure.is_configured(),
    }))
    .into_response()
}

/// `/api/alarms/status` (§2.2).
async fn api_alarms<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let view = state.evaluate_alarms(now_utc()).await;

    let partitions: Vec<serde_json::Value> = view
        .partitions
        .iter()
        .map(|(name, rows)| {
            serde_json::json!({
                "alarm": name,
                "rules": rows
                    .iter()
                    .map(|row| serde_json::json!({
                        "id": row.rule.id,
                        "display_name": row.rule.display_name,
                        "workflow_id": row.rule.workflow_id,
                        "status": row.outcome.status.label().to_lowercase(),
                        "cooldown_remaining_minutes": row.outcome.cooldown_remaining,
                        "pause_remaining_minutes": row.outcome.pause_remaining,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "alarms": partitions,
        "summary": view.summary(),
        // §5.6 — a store outage is named, not swallowed. Without this a
        // Cosmos failure and "no rules configured" are the same JSON.
        "unavailable": view.failed,
        "persistence": state.config.cosmos.describe(),
    }))
    .into_response()
}

async fn alarms_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let view = state.evaluate_alarms(now_utc()).await;

    page(&AlarmsPage {
        persistence: state.config.cosmos.describe(),
        summary: view.summary(),
        partitions: view
            .partitions
            .iter()
            .map(|(name, rows)| AlarmPartition {
                name,
                rules: rows
                    .iter()
                    .map(|row| AlarmRow {
                        display_name: row.rule.display_name.clone(),
                        workflow_id: row.rule.workflow_id.clone(),
                        status: row.outcome.status,
                    })
                    .collect(),
            })
            .collect(),
        failed: view.failed.clone(),
    })
}

async fn stylesheet() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        STYLESHEET,
    )
        .into_response()
}

/// Render a template, or say plainly that it could not be rendered.
fn page<T: Template>(template: &T) -> Response {
    template.render().map_or_else(
        |error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("The dashboard could not render this page: {error}"),
            )
                .into_response()
        },
        |html| Html(html).into_response(),
    )
}

/// Now.
fn now_utc() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

/// Now, in RFC 3339.
fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}
