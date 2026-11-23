//! The Network Test page, the flow source servers screen, and their APIs.
//!
//! Implements `spec/15-dashboard.md` §10.4.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post, put};
use serde::Deserialize;
use serde_json::Value;

use crate::alarm::store::AlarmStore;
use crate::flow_sources::{self, Source, SourceError};
use crate::network_test::{self, Settings};
use crate::observe::Observatory;
use crate::pages::{error_json, script};
use crate::web::{AppState, now_utc, page};

/// Most CSV an import will read (§10.4.8).
const MAX_IMPORT_BYTES: usize = 1_000_000;

/// The routes of §10.4.7.
pub fn routes<O: Observatory, S: AlarmStore>() -> Router<Arc<AppState<O, S>>> {
    Router::new()
        .route("/network-test", get(test_page::<O, S>))
        .route("/network-test/config", get(config_page::<O, S>))
        .route("/api/network-test/run", post(api_run::<O, S>))
        .route(
            "/api/network-test/history",
            get(api_history::<O, S>).delete(api_history_delete::<O, S>),
        )
        .route("/api/network-test/list", get(api_list::<O, S>))
        .route(
            "/api/network-test/sources",
            get(api_sources::<O, S>).post(api_source_create::<O, S>),
        )
        .route("/api/network-test/sources/import", post(api_import::<O, S>))
        .route(
            "/api/network-test/sources/{id}",
            put(api_source_update::<O, S>).delete(api_source_delete::<O, S>),
        )
        .route(
            "/static/network-test.js",
            get(|| async { script(TEST_SCRIPT) }),
        )
        .route(
            "/static/network-test-config.js",
            get(|| async { script(CONFIG_SCRIPT) }),
        )
}

/// The test page script, served as `network-test.js`.
const TEST_SCRIPT: &str = include_str!("../../static/network-test.js");
/// The source-configuration page script, served as `network-test-config.js`.
const CONFIG_SCRIPT: &str = include_str!("../../static/network-test-config.js");

/// A source refusal as the JSON response the page shows.
fn source_failure(error: &SourceError) -> Response {
    let status = match error {
        SourceError::Invalid(_) => StatusCode::BAD_REQUEST,
        SourceError::Persistence(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    error_json(status, error.message())
}

/// The refusal for a host or port that is not valid.
fn invalid_target() -> Response {
    error_json(
        StatusCode::BAD_REQUEST,
        &network_test::InvalidTarget.to_string(),
    )
}

/// What the test page template renders.
#[derive(Template)]
#[template(path = "network-test.html")]
struct TestPage {
    /// The configured sources.
    sources: Vec<Source>,
    /// Whether the sources could not be read.
    unavailable: bool,
}

/// What the source-configuration page template renders.
#[derive(Template)]
#[template(path = "network-test-config.html")]
struct ConfigPage {
    /// The configured sources.
    sources: Vec<Source>,
    /// Whether the sources could not be read.
    unavailable: bool,
    /// Whether a store is configured; the page says so when it is not.
    persistence_configured: bool,
}

/// Handler: the network test page.
async fn test_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let (sources, unavailable) = flow_sources::list(&state.alarms)
        .await
        .map_or((Vec::new(), true), |s| (s, false));

    page(&TestPage {
        sources,
        unavailable,
    })
}

/// Handler: the source configuration page.
async fn config_page<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let (sources, unavailable) = flow_sources::list(&state.alarms)
        .await
        .map_or((Vec::new(), true), |s| (s, false));

    page(&ConfigPage {
        sources,
        unavailable,
        persistence_configured: state.alarms.is_enabled(),
    })
}

/// The body of a JSON request, or the rejection when it is not JSON.
type JsonBody = Result<Json<Value>, axum::extract::rejection::JsonRejection>;

/// The body as a JSON object, or the 400 response when it is not one.
fn object(body: JsonBody) -> Result<Value, Box<Response>> {
    match body {
        Ok(Json(v)) if v.is_object() => Ok(v),
        _ => Err(Box::new(error_json(
            StatusCode::BAD_REQUEST,
            "JSON body must be an object",
        ))),
    }
}

/// Handler: run a test against a host and port.
async fn api_run<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    body: JsonBody,
) -> Response {
    let payload = match object(body) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let host = payload
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let port = payload.get("port").cloned().unwrap_or(Value::Null);

    let Ok(mut result) = network_test::run(host, &port, Settings::default(), now_utc()).await
    else {
        return invalid_target();
    };

    // The test has run whatever Cosmos is doing: a failure to keep the
    // result is a warning on it, not a failed test (§10.4.4).
    if let Err(error) = network_test::save_sample(&state.alarms, &result).await {
        tracing::warn!(%error, "network test history could not be saved");
        result.history_warning = Some(
            "Result could not be saved to history — the history store is unreachable.".to_string(),
        );
    }

    Json(result).into_response()
}

/// `?host=&port=` (§10.4.4).
#[derive(Debug, Default, Deserialize)]
pub struct Target {
    /// The host asked for.
    #[serde(default)]
    host: String,
    /// The port asked for, as text, so a non-number can be refused by name.
    #[serde(default)]
    port: String,
}

/// The host and port from the query, or the 400 response naming what is wrong.
fn target(query: &Target) -> Result<(String, u16), Box<Response>> {
    let host = network_test::validate_host(&query.host).map_err(|_| Box::new(invalid_target()))?;
    let port = network_test::validate_port(&Value::String(query.port.clone()))
        .map_err(|_| Box::new(invalid_target()))?;
    Ok((host, port))
}

/// Handler: read the history for a host and port.
async fn api_history<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(query): Query<Target>,
) -> Response {
    let (host, port) = match target(&query) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match network_test::history(&state.alarms, &host, port).await {
        Ok(samples) => Json(serde_json::json!({"host": host, "port": port, "samples": samples}))
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "network test history could not be read");
            error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "History is currently unavailable.",
            )
        }
    }
}

/// Handler: clear the history for a host and port.
async fn api_history_delete<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Query(query): Query<Target>,
) -> Response {
    let (host, port) = match target(&query) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match network_test::delete_history(&state.alarms, &host, port).await {
        Ok(()) => {
            Json(serde_json::json!({"deleted": true, "host": host, "port": port})).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "network test history could not be deleted");
            error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "History is currently unavailable.",
            )
        }
    }
}

/// Handler: list tested endpoints with their descriptions.
async fn api_list<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    let tested = match network_test::list_tested(&state.alarms).await {
        Ok(t) => t,
        Err(error) => {
            tracing::warn!(%error, "tested endpoints could not be read");
            return error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "History is currently unavailable.",
            );
        }
    };
    // A source list that cannot be read only costs the descriptions.
    let sources = match flow_sources::list(&state.alarms).await {
        Ok(sources) => sources,
        Err(error) => {
            tracing::warn!(
                error = error.message(),
                "flow sources could not be read for the descriptions"
            );
            Vec::new()
        }
    };

    let endpoints: Vec<Value> = tested
        .into_iter()
        .map(|t| {
            let description = sources
                .iter()
                .find(|s| s.port == t.port && s.url.eq_ignore_ascii_case(&t.host))
                .map(|s| s.description.clone());
            serde_json::json!({
                "host": t.host, "port": t.port, "latest": t.latest, "description": description,
            })
        })
        .collect();

    Json(serde_json::json!({ "endpoints": endpoints })).into_response()
}

/// Handler: list the configured sources.
async fn api_sources<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
) -> Response {
    match flow_sources::list(&state.alarms).await {
        Ok(sources) => Json(serde_json::json!({ "sources": sources })).into_response(),
        Err(error) => source_failure(&error),
    }
}

/// The description, URL and port in a source request body.
fn fields(payload: &Value) -> (String, String, Value) {
    (
        payload
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        payload
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        payload.get("port").cloned().unwrap_or(Value::Null),
    )
}

/// Handler: create a source.
async fn api_source_create<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    body: JsonBody,
) -> Response {
    let payload = match object(body) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let (description, url, port) = fields(&payload);

    match flow_sources::add(&state.alarms, &description, &url, &port).await {
        Ok(source) => (StatusCode::CREATED, Json(source)).into_response(),
        Err(error) => source_failure(&error),
    }
}

/// Handler: update a source.
async fn api_source_update<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(id): Path<String>,
    body: JsonBody,
) -> Response {
    let payload = match object(body) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let (description, url, port) = fields(&payload);

    match flow_sources::update(&state.alarms, &id, &description, &url, &port).await {
        Ok(source) => Json(source).into_response(),
        Err(error) => source_failure(&error),
    }
}

/// Handler: delete a source.
async fn api_source_delete<O: Observatory, S: AlarmStore>(
    State(state): State<Arc<AppState<O, S>>>,
    Path(id): Path<String>,
) -> Response {
    match flow_sources::delete(&state.alarms, &id).await {
        Ok(()) => Json(serde_json::json!({"deleted": true, "id": id})).into_response(),
        Err(error) => source_failure(&error),
    }
}

/// Handler: import sources from CSV.
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

    let result = flow_sources::import(&state.alarms, content).await;
    let status = if result.persistence_failed {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (status, Json(result)).into_response()
}
