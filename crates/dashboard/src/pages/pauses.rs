//! The alarm pauses page and API.
//!
//! Implements `spec/15-dashboard.md` §10.2.8.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde_json::Value;
use time::OffsetDateTime;

use crate::alarm::pause::{
    self, Groups, KnownRules, PauseError, PauseSummary, PauseView, Scope, summarise,
};
use crate::alarm::store::{AlarmStore, Partition};
use crate::flows::FLOWS;
use crate::observe::Observatory;
use crate::pages::{error_json, script};
use crate::web::{AppState, now_utc, page};

/// The routes of §10.2.8.
pub fn routes<O: Observatory, S: AlarmStore>() -> Router<Arc<AppState<O, S>>> {
    Router::new()
        .route("/alarms/pauses", get(pauses_page::<O, S>))
        .route(
            "/api/alarm-pauses",
            get(api_list::<O, S>).post(api_create::<O, S>),
        )
        .route("/api/alarm-pauses/options", get(api_options::<O, S>))
        .route(
            "/api/alarm-pauses/{pause_id}/cancel",
            post(api_cancel::<O, S>),
        )
        .route("/static/alarm-pause.js", get(|| async { script(SCRIPT) }))
}

/// The page's script.
const SCRIPT: &str = include_str!("../../static/alarm-pause.js");

/// A short name for an alarm type, for labelling a rule target.
const fn alarm_label(alarm_type: &str) -> &str {
    // `str` equality is not const, so this compares bytes by hand.
    match alarm_type.as_bytes() {
        b"alarm1" => "Incoming",
        b"alarm2" => "Outgoing",
        b"alarm3" => "Failures",
        _ => "Alarm",
    }
}

/// Every rule that is not deleted, as `(alarm type, id, display name,
/// workflow id)`.
///
/// # Errors
///
/// [`PauseError::Persistence`] when a partition could not be read: a pause
/// validated against a guess at the rules would be refused or accepted for
/// the wrong reason (§10.2.6).
async fn catalogue<S: AlarmStore>(
    store: &S,
) -> Result<Vec<(&'static str, String, String, String)>, PauseError> {
    let mut out = Vec::new();
    for partition in Partition::all() {
        let rules = store.rules(partition).await.map_err(|error| {
            tracing::error!(%error, alarm = partition.as_str(), "the alarm rules could not be read");
            PauseError::Persistence("Pause persistence is currently unavailable.".to_string())
        })?;
        for rule in rules.into_iter().filter(|rule| !rule.deleted) {
            out.push((
                partition.as_str(),
                rule.id,
                rule.display_name,
                rule.workflow_id.trim().to_string(),
            ));
        }
    }
    Ok(out)
}

fn known_rules(catalogue: &[(&'static str, String, String, String)]) -> KnownRules {
    let mut known = KnownRules::new();
    for (alarm_type, id, _, workflow_id) in catalogue {
        known
            .entry((*alarm_type).to_string())
            .or_default()
            .insert(id.clone(), workflow_id.clone());
    }
    known
}

/// One choice in the flow picker.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlowOption {
    /// The workflow id.
    pub id: String,
    /// What an operator reads.
    pub label: String,
}

/// One choice in the rule picker.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuleOption {
    /// `alarm1`, `alarm2` or `alarm3`.
    pub alarm_type: String,
    /// The rule.
    pub rule_id: String,
    /// `Incoming: PHW → MPI`.
    pub label: String,
    /// The rule's flow.
    pub workflow_id: String,
}

/// The pickers: the known flows, plus any flow a rule names that this build
/// does not know (a rule can outlive a flow, and must still be pausable).
fn options(
    catalogue: &[(&'static str, String, String, String)],
) -> (Vec<FlowOption>, Vec<RuleOption>) {
    let mut flows: Vec<FlowOption> = FLOWS
        .iter()
        .map(|flow| FlowOption {
            id: flow.workflow_id.to_string(),
            label: flow.label.to_string(),
        })
        .collect();
    for (_, _, _, workflow_id) in catalogue {
        if !workflow_id.is_empty() && !flows.iter().any(|f| &f.id == workflow_id) {
            flows.push(FlowOption {
                id: workflow_id.clone(),
                label: workflow_id.clone(),
            });
        }
    }
    flows.sort_by(|a, b| a.label.cmp(&b.label));

    let rules = catalogue
        .iter()
        .map(|(alarm_type, id, name, workflow_id)| RuleOption {
            alarm_type: (*alarm_type).to_string(),
            rule_id: id.clone(),
            label: format!("{}: {name}", alarm_label(alarm_type)),
            workflow_id: workflow_id.clone(),
        })
        .collect();

    (flows, rules)
}

/// Human labels for a pause's targets.
fn target_labels(
    view: &PauseView,
    flows: &[FlowOption],
    catalogue: &[(&'static str, String, String, String)],
) -> Vec<String> {
    let Some(targets) = view.targets.as_array() else {
        return Vec::new();
    };

    if view.scope_type == Scope::Flows.as_str() {
        return targets
            .iter()
            .filter_map(Value::as_str)
            .map(|id| {
                flows
                    .iter()
                    .find(|f| f.id == id)
                    .map_or_else(|| id.to_string(), |f| f.label.clone())
            })
            .collect();
    }

    targets
        .iter()
        .map(|target| {
            let field = |name: &str| target.get(name).and_then(Value::as_str).unwrap_or_default();
            let name = catalogue
                .iter()
                .find(|(t, id, _, _)| *t == field("alarm_type") && id == field("rule_id"))
                .map_or_else(
                    || field("rule_id").to_string(),
                    |(_, _, name, _)| name.clone(),
                );
            format!("{}: {name}", alarm_label(field("alarm_type")))
        })
        .collect()
}

/// The page's rows.
struct PageGroup {
    title: &'static str,
    empty: &'static str,
    cancellable: bool,
    pauses: Vec<PageRow>,
}

struct PageRow {
    view: PauseView,
    labels: Vec<String>,
    start: String,
    end: String,
    cancelled: String,
}

#[derive(Template)]
#[template(path = "alarm-pauses.html")]
struct PausesPage {
    groups: Vec<PageGroup>,
    summary: PauseSummary,
    flow_options: Vec<FlowOption>,
    rule_options: Vec<RuleOption>,
    persistence_configured: bool,
    unavailable: bool,
}

fn page_group(
    title: &'static str,
    empty: &'static str,
    cancellable: bool,
    views: Vec<PauseView>,
    flows: &[FlowOption],
    catalogue: &[(&'static str, String, String, String)],
) -> PageGroup {
    PageGroup {
        title,
        empty,
        cancellable,
        pauses: views
            .into_iter()
            .map(|view| PageRow {
                labels: target_labels(&view, flows, catalogue),
                start: view.start_display.clone().unwrap_or_default(),
                end: view.end_display.clone().unwrap_or_default(),
                cancelled: view.cancelled_display.clone().unwrap_or_default(),
                view,
            })
            .collect(),
    }
}

/// What the page and the list API both need.
struct Context {
    groups: Groups,
    summary: PauseSummary,
    flows: Vec<FlowOption>,
    rules: Vec<RuleOption>,
    catalogue: Vec<(&'static str, String, String, String)>,
    unavailable: bool,
}

async fn context<S: AlarmStore>(store: &S, now: OffsetDateTime) -> Context {
    // The page reads fail-open (§10.2.6) but says so, rather than showing an
    // empty list that reads as "nothing is paused".
    let (pauses, unavailable) = match pause::list(store).await {
        Ok(pauses) => (pauses, false),
        Err(error) => {
            tracing::warn!(%error, "alarm pauses could not be read for the page");
            (Vec::new(), true)
        }
    };
    let catalogue = match catalogue(store).await {
        Ok(catalogue) => catalogue,
        Err(error) => {
            tracing::warn!(%error, "the flow catalogue could not be read for the page");
            Vec::new()
        }
    };
    let (flows, rules) = options(&catalogue);

    Context {
        groups: pause::group_for_display(&pauses, now),
        summary: summarise(&pauses, now),
        flows,
        rules,
        catalogue,
        unavailable,
    }
}

async fn pauses_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let context = context(&state.alarms, now_utc()).await;
    let Context {
        groups,
        summary,
        flows,
        rules,
        catalogue,
        unavailable,
    } = context;

    page(&PausesPage {
        groups: vec![
            page_group(
                "Active",
                "No alarms are paused.",
                true,
                groups.active,
                &flows,
                &catalogue,
            ),
            page_group(
                "Scheduled",
                "No pauses are scheduled.",
                true,
                groups.scheduled,
                &flows,
                &catalogue,
            ),
            page_group(
                "Recently ended",
                "Nothing has ended in the last seven days.",
                false,
                groups.recent,
                &flows,
                &catalogue,
            ),
        ],
        summary,
        flow_options: flows,
        rule_options: rules,
        persistence_configured: state.alarms.is_enabled(),
        unavailable,
    })
}

/// A view as JSON with its `target_labels` (upstream's `_label_targets`).
fn labelled(
    view: &PauseView,
    flows: &[FlowOption],
    catalogue: &[(&'static str, String, String, String)],
) -> Value {
    let mut value = serde_json::to_value(view).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "target_labels".to_string(),
            serde_json::json!(target_labels(view, flows, catalogue)),
        );
    }
    value
}

async fn api_list<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let Context {
        groups,
        summary,
        flows,
        catalogue,
        unavailable,
        ..
    } = context(&state.alarms, now_utc()).await;

    let group = |views: &[PauseView]| -> Vec<Value> {
        views
            .iter()
            .map(|v| labelled(v, &flows, &catalogue))
            .collect()
    };

    Json(serde_json::json!({
        "groups": {
            "active": group(&groups.active),
            "scheduled": group(&groups.scheduled),
            "recent": group(&groups.recent),
        },
        "summary": summary,
        // §10.2.6 — an unreadable store is not an empty list.
        "unavailable": unavailable,
    }))
    .into_response()
}

async fn api_options<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let Context { flows, rules, .. } = context(&state.alarms, now_utc()).await;

    Json(serde_json::json!({ "flow_options": flows, "rule_options": rules })).into_response()
}

/// A pause failure as the status and body upstream returns (§10.2.8).
fn failure(error: &PauseError) -> Response {
    let status = match error {
        PauseError::Invalid(_) => StatusCode::BAD_REQUEST,
        PauseError::NotFound => StatusCode::NOT_FOUND,
        PauseError::Persistence(_) => StatusCode::SERVICE_UNAVAILABLE,
    };

    (
        status,
        Json(serde_json::json!({ "ok": false, "error": error.message() })),
    )
        .into_response()
}

async fn api_create<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = body else {
        return error_json(StatusCode::BAD_REQUEST, "Expected a JSON object.");
    };
    if !payload.is_object() {
        return error_json(StatusCode::BAD_REQUEST, "Expected a JSON object.");
    }

    let known = match catalogue(&state.alarms).await {
        Ok(catalogue) => known_rules(&catalogue),
        Err(error) => return failure(&error),
    };

    match pause::create(&state.alarms, &payload, &known, now_utc()).await {
        Ok(view) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "ok": true, "pause": view })),
        )
            .into_response(),
        Err(error) => failure(&error),
    }
}

async fn api_cancel<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(pause_id): Path<String>,
) -> Response {
    match pause::cancel(&state.alarms, &pause_id, now_utc()).await {
        Ok(view) => Json(serde_json::json!({ "ok": true, "pause": view })).into_response(),
        Err(error) => failure(&error),
    }
}
