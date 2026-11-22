//! The application state, the router and the JSON API.
//!
//! Implements `spec/20-lookup-service.md` §8.
//!
//! # No request logging
//!
//! §1.3. Lookup keys travel in the query string, so there is deliberately no
//! tracing layer here: the Python turns uvicorn's access log off for the same
//! reason, and a layer that logged the URI would put every key a transformer
//! asks about into the log.

use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::admin::{
    SettingsChange, UploadRequest, ensure_store_writable, replace_table_from_csv,
    update_table_settings,
};
use crate::config::Config;
use crate::error::{Kind, LookupError, SeedDataError};
use crate::form::{boundary_of, parse_multipart, parse_pairs};
use crate::jwks::JwksKeys;
use crate::mapping::RecordMapping;
use crate::model::{RowOut, RowsPage};
use crate::pages;
use crate::resolver::{LookupResolver, named_key_params};
use crate::security::{self, Principal, SigningKeys, TokenVerifier};
use crate::seed::import_missing_seed_tables;
use crate::store::TableStore;

/// The largest page of rows (§8).
pub const MAX_PAGE_SIZE: usize = 200;

/// Slack above the upload limit for a multipart body's own framing and its
/// `definition` and `mapping` fields.
const MULTIPART_OVERHEAD: usize = 1024 * 1024;

/// The largest PATCH body.
const MAX_JSON_BODY: usize = 64 * 1024;

/// Everything the handlers share.
pub struct AppState<S: TableStore> {
    /// What the service was configured with.
    pub config: Config,
    /// The store.
    pub store: Arc<S>,
    /// `None` until startup completes (§5.1).
    resolver: RwLock<Option<Arc<LookupResolver<S>>>>,
    /// Validates bearer tokens (§12.2); `None` when authentication is off.
    verifier: Option<Arc<TokenVerifier>>,
}

impl<S: TableStore> AppState<S> {
    /// State that has not started: every route but `/health/live` answers
    /// `503 not_ready` until [`AppState::start`] completes (§5.1).
    #[must_use]
    pub fn new(config: Config, store: S) -> Self {
        let keys: Option<Arc<dyn SigningKeys>> = config
            .auth_tenant_id
            .as_deref()
            .map(|tenant| -> Arc<dyn SigningKeys> { Arc::new(JwksKeys::for_tenant(tenant)) });
        Self::with_signing_keys(config, store, keys)
    }

    /// As [`AppState::new`] with the token signing keys supplied (tests).
    ///
    /// `keys` is used only when authentication is enabled and a tenant and
    /// audience are configured; otherwise there is no verifier, and the
    /// bearer API answers `401` (it cannot answer anything else safely).
    #[must_use]
    pub fn with_signing_keys(config: Config, store: S, keys: Option<Arc<dyn SigningKeys>>) -> Self {
        let verifier = match (&config.auth_tenant_id, &config.auth_audience, keys) {
            (Some(tenant), Some(audience), Some(keys)) if config.auth_enabled() => {
                Some(Arc::new(TokenVerifier::new(tenant, audience, keys)))
            }
            _ => None,
        };
        if config.unauthenticated_deployment() {
            tracing::warn!(
                environment = %config.environment,
                "AUTHENTICATION IS DISABLED (LOOKUP_AUTH_DISABLED): every caller can read, upload \
                 and change tables as Lookup.Admin. For the unauthenticated trial only; never use \
                 with real data."
            );
        }
        Self {
            config,
            store: Arc::new(store),
            resolver: RwLock::new(None),
            verifier,
        }
    }

    /// Open the store, seed a writable one, and load the tables (§5.1, §7.3).
    ///
    /// # Errors
    ///
    /// A seed problem or a store failure. Either stops startup.
    pub async fn start(&self) -> Result<(), SeedDataError> {
        self.start_with(LookupResolver::new(
            Arc::clone(&self.store),
            self.config.default_ttl_seconds,
            self.config.cache_max_entries,
        ))
        .await
    }

    /// As [`AppState::start`] with a resolver the caller built, so a test can
    /// give it a clock it controls.
    ///
    /// # Errors
    ///
    /// As [`AppState::start`].
    pub async fn start_with(&self, resolver: LookupResolver<S>) -> Result<(), SeedDataError> {
        self.store
            .open()
            .await
            .map_err(|error| SeedDataError(format!("could not open the store: {error}")))?;
        import_missing_seed_tables(&*self.store, &self.config.seed).await?;
        resolver
            .start()
            .await
            .map_err(|error| SeedDataError(format!("could not load the tables: {error}")))?;
        *self
            .resolver
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(resolver));
        Ok(())
    }

    /// Stop serving lookups and release the store.
    pub async fn stop(&self) {
        *self
            .resolver
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.store.close().await;
    }

    /// Lookup API: a valid Entra bearer token holding a reader role (§12.2).
    ///
    /// Verification may fetch the tenant's signing keys, which blocks, so it
    /// runs on a blocking thread; upstream makes the dependency `def` rather
    /// than `async def` for the same reason.
    ///
    /// # Errors
    ///
    /// `401` (with the `Bearer` challenge) without a valid token, `403`
    /// without a reader role, `503` when the signing keys cannot be fetched.
    pub async fn api_reader(&self, headers: &HeaderMap) -> Result<Principal, LookupError> {
        if !self.config.auth_enabled() {
            return Ok(security::auth_disabled_principal(&self.config));
        }
        let token = security::bearer_token(headers)?;
        let Some(verifier) = self.verifier.clone() else {
            // Authentication is on but no tenant and audience: unreachable
            // through `Config::read`, which refuses to start; answer closed.
            return Err(LookupError::bearer_unauthorized("Invalid token"));
        };
        let principal = tokio::task::spawn_blocking(move || verifier.verify(&token))
            .await
            .map_err(|_| LookupError::backend_unavailable("Token validation failed", None))??;
        if principal.has_any(&security::READER_ROLES) {
            Ok(principal)
        } else {
            Err(LookupError::new(
                Kind::Forbidden,
                "Requires one of the roles: Lookup.Admin, Lookup.Editor, Lookup.Reader",
            ))
        }
    }

    /// The resolver, or `not_ready` (§5.1).
    ///
    /// # Errors
    ///
    /// `not_ready` before startup completes.
    pub fn resolver(&self) -> Result<Arc<LookupResolver<S>>, LookupError> {
        self.resolver
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| LookupError::new(Kind::NotReady, "Lookup tables are not loaded yet"))
    }
}

/// The error body of §8.
#[derive(Serialize)]
struct ErrorBody<'a> {
    /// The code (§8).
    error: &'static str,
    /// The sentence.
    detail: &'a str,
    /// The table, or null.
    table: Option<&'a str>,
    /// Per-problem messages, absent unless there are some.
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<&'a Vec<String>>,
}

impl IntoResponse for LookupError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.kind.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = ErrorBody {
            error: self.kind.code(),
            detail: &self.detail,
            table: self.table.as_deref(),
            errors: self.errors.as_ref(),
        };
        let mut response = (status, Json(body)).into_response();
        if self.bearer_challenge {
            // RFC 6750 §3: a 401 from a bearer-token API names the scheme.
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

/// The router: API, pages and static files.
pub fn router<S: TableStore>(state: Arc<AppState<S>>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready::<S>))
        .route("/api/v1/lookup/{table}", get(lookup::<S>))
        .route("/api/v1/tables", get(list_tables::<S>))
        .route(
            "/api/v1/tables/{table}",
            get(get_table::<S>).patch(update_table::<S>),
        )
        .route("/api/v1/tables/{table}/rows", get(list_rows::<S>))
        .route(
            "/api/v1/tables/{table}/uploads",
            axum::routing::post(upload_table::<S>),
        )
        .merge(pages::router::<S>())
        .route("/static/{file}", get(static_file))
        .with_state(state)
}

/// `GET /health/live`: the process is serving HTTP; no dependency checks.
async fn live() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// `GET /health/ready`: tables are loaded and preloaded tables are in memory.
async fn ready<S: TableStore>(State(state): State<Arc<AppState<S>>>) -> Response {
    match state.resolver() {
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready" })),
        )
            .into_response(),
        Ok(resolver) => Json(json!({
            "status": "ready",
            "tables": resolver.summaries().len(),
            "source": resolver.store().source(),
        }))
        .into_response(),
    }
}

/// `GET /api/v1/lookup/{table}`.
async fn lookup<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    Path(table): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<crate::model::LookupResult>, LookupError> {
    // §8.1 and §12: authentication comes before readiness and validation,
    // as FastAPI solves route dependencies first.
    state.api_reader(&headers).await?;
    let resolver = state.resolver()?;
    let pairs = parse_pairs(query.as_deref().unwrap_or_default());
    let positional: Vec<String> = pairs
        .iter()
        .filter(|(name, _)| name == "k")
        .map(|(_, value)| value.clone())
        .collect();
    let named = named_key_params(&table, pairs.iter().map(|(n, v)| (n.as_str(), v.as_str())))?;
    Ok(Json(resolver.lookup(&table, &positional, &named).await?))
}

/// `GET /api/v1/tables`.
async fn list_tables<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::model::TableSummary>>, LookupError> {
    security::require_reader(&state.config, &headers)?;
    Ok(Json(state.resolver()?.summaries()))
}

/// `GET /api/v1/tables/{table}`.
async fn get_table<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    Path(table): Path<String>,
) -> Result<Json<crate::model::TableDetail>, LookupError> {
    security::require_reader(&state.config, &headers)?;
    Ok(Json(state.resolver()?.detail(&table)?))
}

/// `PATCH /api/v1/tables/{table}`.
async fn update_table<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    Path(table): Path<String>,
    request: Request,
) -> Result<Json<crate::model::TableDetail>, LookupError> {
    let principal = security::require_api_editor(&state.config, &headers)?;
    let resolver = state.resolver()?;
    let bytes = axum::body::to_bytes(request.into_body(), MAX_JSON_BODY)
        .await
        .map_err(|_| LookupError::invalid_request("The request body is too large or unreadable"))?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| LookupError::invalid_request("The request body is not valid JSON"))?;
    let change = SettingsChange::from_json(&body)?;
    resolver.definition(&table)?;
    update_table_settings(&resolver, &table, change, &principal.name).await?;
    Ok(Json(resolver.detail(&table)?))
}

/// `GET /api/v1/tables/{table}/rows`.
async fn list_rows<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    Path(table): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<RowsPage>, LookupError> {
    security::require_reader(&state.config, &headers)?;
    let resolver = state.resolver()?;
    let mut search = None;
    let mut offset = 0_usize;
    let mut limit = 50_usize;
    for (name, value) in parse_pairs(query.as_deref().unwrap_or_default()) {
        match name.as_str() {
            "search" => {
                if value.chars().count() > 200 {
                    return Err(LookupError::invalid_request(
                        "search: must be at most 200 characters",
                    ));
                }
                search = Some(value).filter(|v| !v.is_empty());
            }
            "offset" => {
                offset = value.parse().map_err(|_| {
                    LookupError::invalid_request("offset: must be a whole number of at least 0")
                })?;
            }
            "limit" => {
                limit = value
                    .parse()
                    .ok()
                    .filter(|n| (1..=MAX_PAGE_SIZE).contains(n))
                    .ok_or_else(|| {
                        LookupError::invalid_request(format!(
                            "limit: must be a whole number from 1 to {MAX_PAGE_SIZE}"
                        ))
                    })?;
            }
            _ => {}
        }
    }
    resolver.definition(&table)?;
    let (rows, total) = resolver
        .store()
        .query_rows(&table, search.as_deref(), offset, limit)
        .await?;
    Ok(Json(RowsPage {
        table,
        total,
        offset,
        limit,
        search,
        rows: rows
            .into_iter()
            .map(|row| RowOut {
                key: row.key,
                values: row.values,
            })
            .collect(),
    }))
}

/// `POST /api/v1/tables/{table}/uploads`.
async fn upload_table<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    Path(table): Path<String>,
    request: Request,
) -> Result<Response, LookupError> {
    let principal = security::require_api_editor(&state.config, &headers)?;
    let resolver = state.resolver()?;
    let fields = read_multipart(request, &state.config).await?;
    ensure_store_writable(&resolver)?;
    let Some(file) = fields
        .iter()
        .find(|p| p.file_name.is_some() && p.name == "file")
    else {
        return Err(LookupError::invalid_request("file: field required"));
    };
    let text_field = |name: &str| {
        fields
            .iter()
            .find(|p| p.name == name && p.file_name.is_none())
            .map(|p| String::from_utf8_lossy(&p.data).into_owned())
    };
    let mut upload = UploadRequest::new(
        table.clone(),
        file.data.clone(),
        file.file_name.clone(),
        principal.name,
    );
    upload.definition_settings =
        parse_json_object(text_field("definition").as_deref(), "definition", &table)?;
    upload.mapping = parse_mapping(text_field("mapping").as_deref(), &table)?;
    let result = replace_table_from_csv(&state.config, &resolver, upload).await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

/// Read a multipart request body within the upload limit (§8.1).
///
/// # Errors
///
/// `upload_too_large` when the body exceeds the limit plus framing;
/// `invalid_request` when it is not multipart.
pub async fn read_multipart(
    request: Request,
    config: &Config,
) -> Result<Vec<crate::form::Part>, LookupError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let boundary = boundary_of(&content_type)
        .ok_or_else(|| LookupError::invalid_request("The request must be multipart/form-data"))?;
    // An unrepresentable limit refuses the upload; it is never "no limit".
    let Some(max_bytes) = config.max_upload_bytes() else {
        return Err(too_large(config));
    };
    let limit = max_bytes
        .checked_add(MULTIPART_OVERHEAD)
        .ok_or_else(|| too_large(config))?;
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| too_large(config))?;
    let parts = parse_multipart(&bytes, &boundary).map_err(LookupError::invalid_request)?;
    if let Some(file) = parts.iter().find(|p| p.file_name.is_some())
        && file.data.len() > max_bytes
    {
        return Err(too_large(config));
    }
    Ok(parts)
}

/// `upload_too_large` (413).
fn too_large(config: &Config) -> LookupError {
    LookupError::new(
        Kind::UploadTooLarge,
        format!("File exceeds the {} MB upload limit", config.max_upload_mb),
    )
}

/// A JSON object from a form field; blank means absent.
fn parse_json_object(
    raw: Option<&str>,
    field: &str,
    table: &str,
) -> Result<Option<Map<String, Value>>, LookupError> {
    let Some(raw) = raw.filter(|r| !r.trim().is_empty()) else {
        return Ok(None);
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(object)) => Ok(Some(object)),
        Ok(_) => Err(
            LookupError::invalid_upload(format!("{field} must be a JSON object")).for_table(table),
        ),
        Err(_) => {
            Err(LookupError::invalid_upload(format!("{field} is not valid JSON")).for_table(table))
        }
    }
}

/// The `mapping` form field.
fn parse_mapping(raw: Option<&str>, table: &str) -> Result<Option<RecordMapping>, LookupError> {
    let Some(object) = parse_json_object(raw, "mapping", table)? else {
        return Ok(None);
    };
    RecordMapping::from_json(&Value::Object(object))
        .map(Some)
        .map_err(|problems| {
            LookupError::invalid_upload("mapping is invalid")
                .for_table(table)
                .with_errors(problems)
        })
}

/// `GET /static/{file}`.
async fn static_file(Path(file): Path<String>) -> Response {
    let (content_type, bytes): (&'static str, &'static [u8]) = match file.as_str() {
        "lookup.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../static/lookup.css"),
        ),
        "upload.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../static/upload.js"),
        ),
        "theme-init.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../static/theme-init.js"),
        ),
        "theme-toggle.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../static/theme-toggle.js"),
        ),
        "DHCW-logo-RGB-White.png" => (
            "image/png",
            include_bytes!("../static/DHCW-logo-RGB-White.png"),
        ),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}
