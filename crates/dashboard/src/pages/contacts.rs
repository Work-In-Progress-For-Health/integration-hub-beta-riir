//! The Contacts page and API.
//!
//! Implements `spec/15-dashboard.md` §10.3.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::Value;

use crate::alarm::store::AlarmStore;
use crate::contacts::{
    self, Contact, ContactError, DEFAULT_PAGE_SIZE, ORGANISATION_TYPES, Query as ContactQuery,
};
use crate::flows::FLOWS;
use crate::observe::Observatory;
use crate::pages::{error_json, script};
use crate::web::{AppState, now_utc, page};

/// The most CSV a single import will read (§10.3.6).
const MAX_IMPORT_BYTES: usize = 1_000_000;

/// How often the expired-contact purge may run (§10.3.4).
const PURGE_INTERVAL: time::Duration = time::Duration::days(1);

/// The routes of §10.3.9.
pub fn routes<O: Observatory, S: AlarmStore>() -> Router<Arc<AppState<O, S>>> {
    Router::new()
        .route("/contacts", get(contacts_page::<O, S>))
        .route(
            "/api/contacts",
            get(api_list::<O, S>).post(api_create::<O, S>),
        )
        .route("/api/contacts/import", post(api_import::<O, S>))
        .route(
            "/api/contacts/{id}",
            get(api_get::<O, S>)
                .put(api_update::<O, S>)
                .delete(api_delete::<O, S>),
        )
        .route("/api/contacts/{id}/restore", post(api_restore::<O, S>))
        .route("/static/contacts.js", get(|| async { script(SCRIPT) }))
}

const SCRIPT: &str = include_str!("../../static/contacts.js");

fn failure(error: &ContactError) -> Response {
    let status = match error {
        ContactError::Invalid(_) => StatusCode::BAD_REQUEST,
        ContactError::NotFound => StatusCode::NOT_FOUND,
        ContactError::Persistence(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    error_json(status, error.message())
}

/// `/contacts`'s and `/api/contacts`'s query string (§10.3.5).
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// Search text.
    #[serde(default)]
    pub q: Option<String>,
    /// A flow id.
    #[serde(default)]
    pub flow: Option<String>,
    /// 1-based. Text, because `page=abc` is page 1 and not a rejected
    /// request (upstream's behaviour).
    #[serde(default)]
    pub page: Option<String>,
    /// Page size.
    #[serde(default)]
    pub per_page: Option<String>,
    /// `false` to hide soft-deleted contacts.
    #[serde(default)]
    pub include_deleted: Option<String>,
}

impl ListParams {
    fn query(&self) -> ContactQuery {
        let number = |text: &Option<String>, default: usize| {
            text.as_deref()
                .and_then(|t| t.trim().parse::<usize>().ok())
                .map_or(default, |n| n.max(1))
        };
        let clean = |text: &Option<String>| {
            text.as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(ToString::to_string)
        };

        ContactQuery {
            search: clean(&self.q),
            flow_id: clean(&self.flow),
            page: number(&self.page, 1),
            per_page: number(&self.per_page, DEFAULT_PAGE_SIZE),
            include_deleted: !self
                .include_deleted
                .as_deref()
                .is_some_and(|v| v.eq_ignore_ascii_case("false")),
        }
    }
}

struct Row {
    contact: Contact,
    flows: Vec<String>,
    /// The contact as JSON, for the edit button (escaped in the attribute).
    json: String,
}

struct FlowChoice {
    id: &'static str,
    label: &'static str,
}

#[derive(Template)]
#[template(path = "contacts.html")]
struct ContactsPage {
    rows: Vec<Row>,
    total: usize,
    page: usize,
    pages: usize,
    q: String,
    flow: String,
    filtered: bool,
    flow_options: Vec<FlowChoice>,
    organisation_types: [&'static str; 3],
    persistence_configured: bool,
    unavailable: bool,
}

fn flow_label(id: &str) -> String {
    FLOWS
        .iter()
        .find(|f| f.workflow_id == id)
        .map_or_else(|| id.to_string(), |f| f.label.to_string())
}

/// Run the retention purge if it has not run today (§10.3.4). A failure
/// never reaches the page.
async fn purge_if_due<O: Observatory, S: AlarmStore>(state: &AppState<O, S>) {
    let now = now_utc();
    let due = state
        .contacts_purged_at
        .lock()
        .is_ok_and(|mut last| match *last {
            // Not due again until a day after the last purge; a due time that
            // cannot be represented is never reached (RCA-TIME-3).
            Some(at) if at.checked_add(PURGE_INTERVAL).is_none_or(|due| now < due) => false,
            _ => {
                *last = Some(now);
                true
            }
        });

    if due {
        let purged =
            contacts::purge_expired(&state.alarms, state.config.contact_retention_days, now).await;
        if purged > 0 {
            tracing::info!(purged, "expired contacts purged");
        }
    }
}

async fn contacts_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(params): Query<ListParams>,
) -> Response {
    purge_if_due(&state).await;

    let (all, unavailable) = match contacts::list_all(&state.alarms).await {
        Ok(all) => (all, false),
        Err(_) => (Vec::new(), true),
    };
    let query = params.query();
    let (selected, total) = contacts::select(all, &query);
    let pages = total.div_ceil(query.per_page).max(1);

    page(&ContactsPage {
        rows: selected
            .into_iter()
            .map(|contact| Row {
                flows: contact.flow_ids.iter().map(|id| flow_label(id)).collect(),
                json: serde_json::to_string(&contact).unwrap_or_default(),
                contact,
            })
            .collect(),
        total,
        page: query.page,
        pages,
        filtered: query.search.is_some() || query.flow_id.is_some(),
        q: query.search.unwrap_or_default(),
        flow: query.flow_id.unwrap_or_default(),
        flow_options: FLOWS
            .iter()
            .map(|f| FlowChoice {
                id: f.workflow_id,
                label: f.label,
            })
            .collect(),
        organisation_types: ORGANISATION_TYPES,
        persistence_configured: state.alarms.is_enabled(),
        unavailable,
    })
}

async fn api_list<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(params): Query<ListParams>,
) -> Response {
    let all = match contacts::list_all(&state.alarms).await {
        Ok(all) => all,
        Err(error) => return failure(&error),
    };
    let query = params.query();
    let (page_of_contacts, total) = contacts::select(all, &query);

    Json(serde_json::json!({
        "contacts": page_of_contacts,
        "total": total,
        "page": query.page,
        "per_page": query.per_page,
    }))
    .into_response()
}

/// The JSON body, as an object (§10.3.9).
fn object(
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Value, Box<Response>> {
    match body {
        Ok(Json(value)) if value.is_object() => Ok(value),
        _ => Err(Box::new(error_json(
            StatusCode::BAD_REQUEST,
            "JSON body must be an object",
        ))),
    }
}

async fn api_create<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let data = match object(body) {
        Ok(data) => data,
        Err(response) => return *response,
    };

    match contacts::add(&state.alarms, &data, now_utc()).await {
        Ok(contact) => (StatusCode::CREATED, Json(contact)).into_response(),
        Err(error) => failure(&error),
    }
}

async fn api_get<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(id): Path<String>,
) -> Response {
    match contacts::get(&state.alarms, &id).await {
        Ok(Some(contact)) => Json(contact).into_response(),
        Ok(None) => failure(&ContactError::NotFound),
        Err(error) => failure(&error),
    }
}

async fn api_update<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(id): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let data = match object(body) {
        Ok(data) => data,
        Err(response) => return *response,
    };

    match contacts::update(&state.alarms, &id, &data, now_utc()).await {
        Ok(contact) => Json(contact).into_response(),
        Err(error) => failure(&error),
    }
}

async fn api_delete<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(id): Path<String>,
) -> Response {
    match contacts::delete(&state.alarms, &id, now_utc()).await {
        Ok(()) => Json(serde_json::json!({ "deleted": true, "id": id })).into_response(),
        Err(error) => failure(&error),
    }
}

async fn api_restore<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(id): Path<String>,
) -> Response {
    match contacts::restore(&state.alarms, &id, now_utc()).await {
        Ok(contact) => Json(contact).into_response(),
        Err(error) => failure(&error),
    }
}

async fn api_import<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    body: Bytes,
) -> Response {
    if body.is_empty() {
        return error_json(StatusCode::BAD_REQUEST, "No CSV file uploaded.");
    }
    if body.len() > MAX_IMPORT_BYTES {
        return error_json(StatusCode::BAD_REQUEST, "CSV file is too large.");
    }
    let Ok(content) = std::str::from_utf8(&body) else {
        return error_json(StatusCode::BAD_REQUEST, "CSV file must be UTF-8 encoded.");
    };

    let result = contacts::import(&state.alarms, content, now_utc()).await;
    let status = if result.persistence_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (status, Json(result)).into_response()
}
