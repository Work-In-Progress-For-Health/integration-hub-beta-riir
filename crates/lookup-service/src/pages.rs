//! The HTML pages.
//!
//! Implements `spec/20-lookup-service.md` §9.
//!
//! The private structs below are view models: their fields are the variables
//! of the template that renders them, and the template is where they are
//! documented.
#![allow(clippy::missing_docs_in_private_items)]

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use serde_json::{Map, Value, json};

use crate::admin::{
    SettingsChange, UploadRequest, ensure_store_writable, replace_table_from_csv,
    update_table_settings,
};
use crate::error::{Kind, LookupError};
use crate::form::{MalformedEncoding, parse_pairs, percent_encode};
use crate::model::{CacheMode, Case, LookupResult, TableDetail, TableSummary};
use crate::resolver::{LookupResolver, NAMED_KEY_PREFIX, named_key_params};
use crate::security::{self, CSRF_FIELD_NAME, EDITOR_ROLES, Principal, ROLE_EDITOR};
use crate::store::TableStore;
use crate::ttl::{TTL_UNITS, format_ttl, split_ttl, ttl_to_seconds, unit_names};
use crate::web::{AppState, read_multipart};

/// Rows per page (§9.1).
const PAGE_SIZE: usize = 50;

/// The choices of the key-case selector.
const KEY_CASES: [&str; 3] = ["preserve", "upper", "lower"];

/// The query string of a stylesheet and script URL.
const ASSET_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Headers on every page (§9.1).
const SECURITY_HEADERS: [(&str, &str); 3] = [
    (
        "content-security-policy",
        "default-src 'self'; frame-ancestors 'none'; form-action 'self'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
];

/// The pages' routes.
pub fn router<S: TableStore>() -> Router<Arc<AppState<S>>> {
    Router::new()
        .route("/", get(overview::<S>))
        .route("/tables/{table}", get(table_page::<S>))
        .route(
            "/tables/{table}/settings",
            axum::routing::post(settings_submit::<S>),
        )
        .route("/upload", get(upload_page::<S>).post(upload_submit::<S>))
}

/// An error as a page shows it.
struct ErrorView {
    code: &'static str,
    detail: String,
    errors: Vec<String>,
}

impl From<&LookupError> for ErrorView {
    fn from(error: &LookupError) -> Self {
        Self {
            code: error.kind.code(),
            detail: error.detail.clone(),
            errors: error.errors.clone().unwrap_or_default(),
        }
    }
}

/// A column badge.
struct ColumnView {
    name: String,
    is_default: bool,
    /// For a key column: the case rule, when it is not `preserve`.
    case: Option<&'static str>,
}

/// A unit or case choice in a `<select>`.
struct Choice {
    name: String,
    selected: bool,
}

/// What every page carries beyond its own content (§12.5, §2.8).
///
/// Every page can embed a form, so every page carries the CSRF token, issued
/// once per browser session; and every page of an unauthenticated
/// deployment carries the banner that says so.
struct Chrome {
    /// The token a form echoes back in `csrf_token`.
    csrf_token: String,
    /// The cookie to set, when the token is new.
    set_cookie: Option<HeaderValue>,
    /// The environment, when authentication is off in a deployed one.
    unauthenticated_environment: Option<String>,
}

impl Chrome {
    /// The furniture for this request.
    fn for_request<S: TableStore>(state: &AppState<S>, headers: &HeaderMap) -> Self {
        let (csrf_token, is_new) = security::current_csrf_token(&state.config, headers);
        Self {
            set_cookie: is_new.then(|| security::csrf_set_cookie(&state.config, &csrf_token)),
            csrf_token,
            unauthenticated_environment: state
                .config
                .unauthenticated_deployment()
                .then(|| state.config.environment.clone()),
        }
    }
}

/// A page that carries [`Chrome`].
trait Page: Template {
    /// Its furniture.
    fn chrome(&self) -> &Chrome;
}

/// Render a template into a response with the security headers.
fn render<T: Page>(template: &T, status: StatusCode) -> Response {
    // A template that compiled and then failed to render is a defect; say so
    // without any of the page's content.
    template.render().map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |body| {
            let mut response = (status, Html(body)).into_response();
            if let Some(cookie) = &template.chrome().set_cookie {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, cookie.clone());
            }
            for (name, value) in SECURITY_HEADERS {
                response.headers_mut().insert(
                    header::HeaderName::from_static(name),
                    HeaderValue::from_static(value),
                );
            }
            response
        },
    )
}

/// Why this caller cannot change tables, if they cannot (§9.1, §12.1).
///
/// A reader may open the upload page and see why the form is disabled,
/// rather than be told `403` after filling it in.
fn uploads_blocked<S: TableStore>(
    principal: &Principal,
    resolver: &LookupResolver<S>,
) -> Option<String> {
    if !principal.has_any(&EDITOR_ROLES) {
        return Some(format!("Changing tables needs the {ROLE_EDITOR} role"));
    }
    ensure_store_writable(resolver)
        .err()
        .map(|error| error.detail)
}

/// A summary's last-upload time, shortened to the second.
fn short_time(at: &str) -> String {
    at.replace('T', " ").chars().take(19).collect()
}

/// A column list marking the default.
fn value_columns(summary: &TableSummary) -> Vec<ColumnView> {
    summary
        .value_columns
        .iter()
        .map(|name| ColumnView {
            name: name.clone(),
            is_default: *name == summary.default_value_column,
            case: None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

/// One row of the overview.
struct OverviewRow {
    name: String,
    description: String,
    key_columns: Vec<String>,
    value_columns: Vec<ColumnView>,
    row_count: usize,
    preload: bool,
    ttl_text: String,
    cache_entries: usize,
    hits: u64,
    misses: u64,
    last_upload_at: Option<String>,
    last_upload_file: String,
    last_upload_by: String,
}

/// The table overview.
#[derive(Template)]
#[template(path = "overview.html")]
struct OverviewPage {
    chrome: Chrome,
    active: &'static str,
    asset_version: &'static str,
    tables: Vec<OverviewRow>,
    total_rows: usize,
    composite_count: usize,
    cached_entries: usize,
    source: String,
    uploads_blocked: Option<String>,
}

/// `GET /`.
async fn overview<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
) -> Result<Response, LookupError> {
    let principal = security::require_reader(&state.config, &headers)?;
    let resolver = state.resolver()?;
    let summaries = resolver.summaries();
    let page = OverviewPage {
        chrome: Chrome::for_request(&state, &headers),
        active: "tables",
        asset_version: ASSET_VERSION,
        total_rows: summaries.iter().map(|t| t.row_count).sum(),
        composite_count: summaries.iter().filter(|t| t.key_columns.len() > 1).count(),
        cached_entries: summaries
            .iter()
            .filter(|t| t.cache.mode == CacheMode::Ttl)
            .map(|t| t.cache.entries)
            .sum(),
        source: resolver.store().source().to_owned(),
        uploads_blocked: uploads_blocked(&principal, &resolver),
        tables: summaries
            .iter()
            .map(|t| OverviewRow {
                name: t.name.clone(),
                description: t.description.clone(),
                key_columns: t.key_columns.clone(),
                value_columns: value_columns(t),
                row_count: t.row_count,
                preload: t.preload,
                ttl_text: format_ttl(t.ttl_seconds),
                cache_entries: t.cache.entries,
                hits: t.cache.hits,
                misses: t.cache.misses,
                last_upload_at: t.last_upload_at.as_deref().map(short_time),
                last_upload_file: t.last_upload_file.clone().unwrap_or_default(),
                last_upload_by: t.last_upload_by.clone().unwrap_or_default(),
            })
            .collect(),
    };
    Ok(render(&page, StatusCode::OK))
}

// ---------------------------------------------------------------------------
// A table
// ---------------------------------------------------------------------------

/// Not found.
#[derive(Template)]
#[template(path = "not_found.html")]
struct NotFoundPage {
    chrome: Chrome,
    active: &'static str,
    asset_version: &'static str,
    table: String,
}

/// A table in full.
struct TableView {
    name: String,
    description: String,
    row_count: usize,
    preload: bool,
    ttl_text: String,
    cache_entries: usize,
    hits: u64,
    misses: u64,
    last_upload_at: Option<String>,
    last_upload_by: Option<String>,
    last_upload_file: Option<String>,
    key_columns: Vec<ColumnView>,
    value_columns: Vec<ColumnView>,
    default_value_column: String,
    source: String,
}

/// One input of the lookup tester.
struct TesterField {
    name: String,
    value: String,
}

/// A successful tester lookup.
struct ResultView {
    key_text: String,
    value: String,
    values: Vec<(String, String)>,
    served: String,
}

/// One row of the rows table.
struct RowView {
    key: Vec<String>,
    cells: Vec<String>,
}

/// The table page.
#[derive(Template)]
#[template(path = "table.html")]
struct TablePage {
    chrome: Chrome,
    active: &'static str,
    asset_version: &'static str,
    t: TableView,
    uploads_blocked: Option<String>,
    uploaded: Option<i64>,
    saved: bool,
    settings_error: Option<ErrorView>,
    ttl_value: String,
    ttl_units: Vec<Choice>,
    preload_on: bool,
    default_ttl_text: String,
    tester_fields: Vec<TesterField>,
    tester_result: Option<ResultView>,
    tester_error: Option<ErrorView>,
    search: String,
    rows: Vec<RowView>,
    total: usize,
    page: usize,
    pages: usize,
    previous_href: Option<String>,
    next_href: Option<String>,
}

/// What a table page is rendered with beyond the table itself.
#[derive(Default)]
struct TableExtras {
    result: Option<LookupResult>,
    error: Option<LookupError>,
    submitted: HashMap<String, String>,
    uploaded: Option<i64>,
    saved: bool,
    settings_error: Option<LookupError>,
    settings_fields: Option<HashMap<String, String>>,
    search: String,
    page: usize,
}

/// `GET /tables/{table}`.
async fn table_page<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    Path(table): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Response, LookupError> {
    let principal = security::require_reader(&state.config, &headers)?;
    let resolver = state.resolver()?;
    let chrome = Chrome::for_request(&state, &headers);
    if !resolver.has_table(&table) {
        return Ok(not_found(&table, chrome));
    }
    let pairs = parse_pairs(query.as_deref().unwrap_or_default())?;
    let get_one = |name: &str| {
        pairs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };

    let search = get_one("search").unwrap_or_default();
    if search.chars().count() > 200 {
        return Err(LookupError::invalid_request(
            "search: must be at most 200 characters",
        ));
    }
    let page = match get_one("page") {
        None => 1,
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|p| *p >= 1)
            .ok_or_else(|| {
                LookupError::invalid_request("page: must be a whole number of at least 1")
            })?,
    };
    let number = |name: &str| -> Result<Option<i64>, LookupError> {
        get_one(name).map_or(Ok(None), |raw| {
            raw.parse().map(Some).map_err(|_| {
                LookupError::invalid_request(format!("{name}: must be a whole number"))
            })
        })
    };
    let uploaded = number("uploaded")?;
    let saved = number("saved")?.is_some();

    let mut extras = TableExtras {
        uploaded,
        saved,
        search,
        page,
        ..TableExtras::default()
    };
    if pairs.iter().any(|(n, _)| n.starts_with(NAMED_KEY_PREFIX)) {
        let outcome = async {
            let named =
                named_key_params(&table, pairs.iter().map(|(n, v)| (n.as_str(), v.as_str())))?;
            extras.submitted = named.clone().into_iter().collect();
            resolver.lookup(&table, &[], &named).await
        }
        .await;
        match outcome {
            Ok(result) => extras.result = Some(result),
            Err(error) => extras.error = Some(error),
        }
    }
    render_table(
        &state,
        &resolver,
        &principal,
        chrome,
        &table,
        extras,
        StatusCode::OK,
    )
    .await
}

/// Build and render the table page.
// One straight-line mapping from the store's view of a table to the
// template's; splitting it would only scatter the field list.
#[allow(clippy::too_many_lines)]
async fn render_table<S: TableStore>(
    state: &AppState<S>,
    resolver: &LookupResolver<S>,
    principal: &Principal,
    chrome: Chrome,
    table: &str,
    extras: TableExtras,
    status: StatusCode,
) -> Result<Response, LookupError> {
    let detail: TableDetail = resolver.detail(table)?;
    let definition = resolver.definition(table)?;
    // `page` is at least 1: `table_page` refused anything else (`§9.1`).
    let page_number = extras.page;
    // `page_number >= 1` above, so this cannot underflow; an offset that
    // overflows is a page that does not exist, refused as an invalid request.
    let offset = page_number
        .checked_sub(1)
        .and_then(|before| before.checked_mul(PAGE_SIZE))
        .ok_or_else(|| LookupError::invalid_request("page is out of range"))?;
    let (rows, total) = resolver
        .store()
        .query_rows(
            table,
            Some(extras.search.as_str()).filter(|s| !s.is_empty()),
            offset,
            PAGE_SIZE,
        )
        .await?;
    let pages = total.div_ceil(PAGE_SIZE).max(1);
    let summary = &detail.summary;
    let t = TableView {
        name: summary.name.clone(),
        description: summary.description.clone(),
        row_count: summary.row_count,
        preload: summary.preload,
        ttl_text: format_ttl(summary.ttl_seconds),
        cache_entries: summary.cache.entries,
        hits: summary.cache.hits,
        misses: summary.cache.misses,
        last_upload_at: summary
            .last_upload_at
            .as_deref()
            .map(|at| at.replace('T', " ").chars().take(16).collect()),
        last_upload_by: summary.last_upload_by.clone(),
        last_upload_file: summary.last_upload_file.clone(),
        key_columns: summary
            .key_columns
            .iter()
            .map(|name| ColumnView {
                name: name.clone(),
                is_default: false,
                case: match detail.key_normalisation.get(name).map(|r| r.case) {
                    Some(Case::Upper) => Some("upper"),
                    Some(Case::Lower) => Some("lower"),
                    _ => None,
                },
            })
            .collect(),
        value_columns: value_columns(summary),
        default_value_column: summary.default_value_column.clone(),
        source: summary.source.clone(),
    };

    // §9.4: the form shows what the user typed after an error, else the
    // stored TTL split into its largest exact unit.
    let (ttl_value, ttl_unit, preload_on) = match &extras.settings_fields {
        Some(fields) => (
            fields.get("ttl_value").cloned().unwrap_or_default(),
            fields
                .get("ttl_unit")
                .cloned()
                .unwrap_or_else(|| "seconds".to_owned()),
            fields.get("preload").is_some_and(|v| v == "on"),
        ),
        None => definition.ttl_seconds.map_or_else(
            || (String::new(), "seconds".to_owned(), summary.preload),
            |ttl| {
                let (value, unit) = split_ttl(ttl);
                (value.to_string(), unit.to_owned(), summary.preload)
            },
        ),
    };

    let href = |page: usize| {
        let mut href = format!("/tables/{table}?page={page}");
        if !extras.search.is_empty() {
            href.push_str("&search=");
            href.push_str(&percent_encode(&extras.search));
        }
        href
    };
    let page = TablePage {
        chrome,
        active: "tables",
        asset_version: ASSET_VERSION,
        uploads_blocked: uploads_blocked(principal, resolver),
        uploaded: extras.uploaded,
        saved: extras.saved,
        settings_error: extras.settings_error.as_ref().map(ErrorView::from),
        ttl_value,
        ttl_units: unit_choices(&ttl_unit),
        preload_on,
        default_ttl_text: format_ttl(state.config.default_ttl_seconds),
        tester_fields: definition
            .key_columns
            .iter()
            .map(|name| TesterField {
                name: name.clone(),
                value: extras.submitted.get(name).cloned().unwrap_or_default(),
            })
            .collect(),
        tester_result: extras.result.as_ref().map(|r| ResultView {
            key_text: crate::keys::key_text(&r.key),
            value: r.value.clone(),
            values: r
                .values
                .iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            served: if r.cached {
                "from cache".to_owned()
            } else {
                format!("from {}", r.source)
            },
        }),
        tester_error: extras.error.as_ref().map(ErrorView::from),
        search: extras.search.clone(),
        rows: rows
            .iter()
            .map(|row| RowView {
                key: row.key.clone(),
                cells: definition
                    .value_columns
                    .iter()
                    .map(|c| row.values.get(c).unwrap_or_default().to_owned())
                    .collect(),
            })
            .collect(),
        total,
        page: page_number,
        pages,
        previous_href: page_number
            .checked_sub(1)
            .filter(|previous| *previous >= 1)
            .map(href),
        next_href: page_number
            .checked_add(1)
            .filter(|next| *next <= pages)
            .map(href),
        t,
    };
    Ok(render(&page, status))
}

/// The page for a table that does not exist (§9.1).
fn not_found(table: &str, chrome: Chrome) -> Response {
    render(
        &NotFoundPage {
            chrome,
            active: "tables",
            asset_version: ASSET_VERSION,
            table: table.to_owned(),
        },
        StatusCode::NOT_FOUND,
    )
}

/// The TTL units as `<option>`s, with `selected` marked.
fn unit_choices(selected: &str) -> Vec<Choice> {
    unit_names()
        .into_iter()
        .map(|name| Choice {
            name: name.to_owned(),
            selected: name == selected,
        })
        .collect()
}

/// Parse an urlencoded POST body into fields; the first of a repeat wins
/// only where the Python's form dictionary would, which is the last.
fn form_fields(bytes: &[u8]) -> Result<HashMap<String, String>, MalformedEncoding> {
    // Not `from_utf8_lossy`: a body that is not UTF-8 is refused, not repaired.
    let text = std::str::from_utf8(bytes).map_err(|_| MalformedEncoding)?;
    Ok(parse_pairs(text)?.into_iter().collect())
}

/// `POST /tables/{table}/settings`.
async fn settings_submit<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    Path(table): Path<String>,
    request: Request,
) -> Result<Response, LookupError> {
    // §12: the role check is a dependency, so it precedes everything else.
    let principal = security::require_editor(&state.config, &headers)?;
    let resolver = state.resolver()?;
    if !resolver.has_table(&table) {
        return Ok(not_found(&table, Chrome::for_request(&state, &headers)));
    }
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .map_err(|_| LookupError::invalid_request("The form is too large or unreadable"))?;
    let fields = form_fields(&bytes)?;
    let outcome = async {
        // §12.5: the form must carry the token its cookie holds.
        security::verify_csrf(
            &state.config,
            &headers,
            fields.get(CSRF_FIELD_NAME).map(String::as_str),
        )?;
        let ttl = ttl_from_form(&fields, Some(&table))?;
        let change = SettingsChange {
            ttl_seconds: Some(ttl),
            preload: Some(fields.get("preload").is_some_and(|v| v == "on")),
        };
        update_table_settings(&resolver, &table, change, &principal.name).await
    }
    .await;
    match outcome {
        Ok(()) => Ok(Redirect::to(&format!("/tables/{table}?saved=1")).into_response()),
        Err(error) => {
            let status = StatusCode::from_u16(error.kind.status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            render_table(
                &state,
                &resolver,
                &principal,
                Chrome::for_request(&state, &headers),
                &table,
                TableExtras {
                    settings_error: Some(error),
                    settings_fields: Some(fields),
                    page: 1,
                    ..TableExtras::default()
                },
                status,
            )
            .await
        }
    }
}

/// TTL in seconds from `ttl_value` and `ttl_unit`; blank means the service
/// default (§9.4).
fn ttl_from_form(
    fields: &HashMap<String, String>,
    table: Option<&str>,
) -> Result<Option<u32>, LookupError> {
    let raw = fields.get("ttl_value").map_or("", |v| v.trim());
    if raw.is_empty() {
        return Ok(None);
    }
    let value: i64 = raw
        .parse()
        .map_err(|_| LookupError::invalid_settings("TTL must be a whole number", table))?;
    let unit = fields.get("ttl_unit").map_or("seconds", String::as_str);
    ttl_to_seconds(value, unit, table).map(Some)
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// What the upload form holds.
#[derive(Default)]
struct UploadForm {
    table: String,
    redefine: bool,
    key_columns: String,
    value_columns: String,
    default_value_column: String,
    ttl_value: String,
    preload: bool,
    description: String,
}

/// The upload page.
#[derive(Template)]
#[template(path = "upload.html")]
struct UploadPage {
    chrome: Chrome,
    active: &'static str,
    asset_version: &'static str,
    form: UploadForm,
    tables: Vec<(String, usize)>,
    key_cases: Vec<Choice>,
    ttl_units: Vec<Choice>,
    max_upload_mb: usize,
    default_ttl_text: String,
    uploads_blocked: Option<String>,
    error: Option<ErrorView>,
}

/// Build the upload page for a set of submitted fields.
fn upload_page_for<S: TableStore>(
    state: &AppState<S>,
    chrome: Chrome,
    principal: &Principal,
    resolver: &LookupResolver<S>,
    fields: &HashMap<String, String>,
    error: Option<&LookupError>,
) -> UploadPage {
    let get = |name: &str| fields.get(name).cloned().unwrap_or_default();
    let case = fields.get("key_case").map_or("preserve", String::as_str);
    UploadPage {
        chrome,
        active: "upload",
        asset_version: ASSET_VERSION,
        form: UploadForm {
            table: get("table"),
            redefine: fields.contains_key("redefine"),
            key_columns: get("key_columns"),
            value_columns: get("value_columns"),
            default_value_column: get("default_value_column"),
            ttl_value: get("ttl_value"),
            preload: fields.contains_key("preload"),
            description: get("description"),
        },
        tables: resolver
            .summaries()
            .into_iter()
            .map(|t| (t.name, t.row_count))
            .collect(),
        key_cases: KEY_CASES
            .iter()
            .map(|name| Choice {
                name: (*name).to_owned(),
                selected: *name == case,
            })
            .collect(),
        ttl_units: unit_choices(fields.get("ttl_unit").map_or("seconds", String::as_str)),
        max_upload_mb: state.config.max_upload_mb,
        default_ttl_text: format_ttl(state.config.default_ttl_seconds),
        uploads_blocked: uploads_blocked(principal, resolver),
        error: error.map(ErrorView::from),
    }
}

/// `GET /upload`.
async fn upload_page<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<Response, LookupError> {
    let principal = security::require_reader(&state.config, &headers)?;
    let resolver = state.resolver()?;
    let table = parse_pairs(query.as_deref().unwrap_or_default())?
        .into_iter()
        .find(|(name, _)| name == "table")
        .map_or_default(|(_, value)| value);
    let fields = HashMap::from([("table".to_owned(), table)]);
    Ok(render(
        &upload_page_for(
            &state,
            Chrome::for_request(&state, &headers),
            &principal,
            &resolver,
            &fields,
            None,
        ),
        StatusCode::OK,
    ))
}

/// `POST /upload`.
async fn upload_submit<S: TableStore>(
    State(state): State<Arc<AppState<S>>>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, LookupError> {
    let principal = security::require_editor(&state.config, &headers)?;
    let resolver = state.resolver()?;
    let parts = match read_multipart(request, &state.config).await {
        Ok(parts) => parts,
        Err(error) => {
            let status = StatusCode::from_u16(error.kind.status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return Ok(render(
                &upload_page_for(
                    &state,
                    Chrome::for_request(&state, &headers),
                    &principal,
                    &resolver,
                    &HashMap::new(),
                    Some(&error),
                ),
                status,
            ));
        }
    };
    let fields: HashMap<String, String> = parts
        .iter()
        .filter(|p| p.file_name.is_none())
        .map(|p| {
            (
                p.name.clone(),
                String::from_utf8_lossy(&p.data).into_owned(),
            )
        })
        .collect();
    let table = fields.get("table").map_or("", |t| t.trim()).to_owned();

    let outcome = async {
        security::verify_csrf(
            &state.config,
            &headers,
            fields.get(CSRF_FIELD_NAME).map(String::as_str),
        )?;
        ensure_store_writable(&resolver)?;
        let file = parts
            .iter()
            .find(|p| p.name == "file" && p.file_name.as_deref().is_some_and(|n| !n.is_empty()))
            .ok_or_else(|| {
                LookupError::invalid_upload("Choose a CSV file to upload").for_table_opt(&table)
            })?;
        let definition_settings =
            if !resolver.has_table(&table) || fields.get("redefine").is_some_and(|v| v == "on") {
                Some(definition_from_form(&fields)?)
            } else {
                None
            };
        let mut upload = UploadRequest::new(
            table.clone(),
            file.data.clone(),
            file.file_name.clone(),
            principal.name.clone(),
        );
        upload.definition_settings = definition_settings;
        replace_table_from_csv(&state.config, &resolver, upload).await
    }
    .await;
    match outcome {
        Ok(result) => Ok(Redirect::to(&format!(
            "/tables/{}?uploaded={}",
            result.table, result.row_count
        ))
        .into_response()),
        Err(error) => {
            let status = StatusCode::from_u16(error.kind.status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            Ok(render(
                &upload_page_for(
                    &state,
                    Chrome::for_request(&state, &headers),
                    &principal,
                    &resolver,
                    &fields,
                    Some(&error),
                ),
                status,
            ))
        }
    }
}

/// The table definition the upload form describes (§9.4).
fn definition_from_form(
    fields: &HashMap<String, String>,
) -> Result<Map<String, Value>, LookupError> {
    let table = fields.get("table").map(|t| t.trim().to_owned());
    let split = |name: &str| -> Vec<String> {
        fields.get(name).map_or_default(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect()
        })
    };
    let key_columns = split("key_columns");
    let case = fields.get("key_case").map_or("preserve", String::as_str);
    if !KEY_CASES.contains(&case) {
        return Err(LookupError::invalid_upload(format!(
            "Key case must be one of {}",
            KEY_CASES.join(", ")
        ))
        .for_table_opt(table.as_deref().unwrap_or_default()));
    }
    let ttl_seconds = ttl_from_form(fields, table.as_deref().filter(|t| !t.is_empty()))?;
    let default_column = fields
        .get("default_value_column")
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    let normalisation =
        crate::import::case_rules(&key_columns, Case::parse(case).unwrap_or(Case::Preserve));
    let mut definition = Map::new();
    definition.insert(
        "description".to_owned(),
        json!(fields.get("description").map_or("", |d| d.trim())),
    );
    definition.insert("key_columns".to_owned(), json!(key_columns));
    definition.insert("value_columns".to_owned(), json!(split("value_columns")));
    definition.insert("default_value_column".to_owned(), json!(default_column));
    definition.insert("key_normalisation".to_owned(), normalisation);
    definition.insert("ttl_seconds".to_owned(), json!(ttl_seconds));
    definition.insert(
        "preload".to_owned(),
        json!(fields.get("preload").is_some_and(|v| v == "on")),
    );
    Ok(definition)
}

impl LookupError {
    /// Name the table concerned unless it is blank (a form may omit it).
    fn for_table_opt(self, table: &str) -> Self {
        if table.is_empty() {
            self
        } else {
            self.for_table(table)
        }
    }
}

/// Unused outside this module; keeps the units visible to the compiler's
/// dead-code analysis where only the templates would otherwise name them.
const _: &[(&str, u32)] = &TTL_UNITS;

/// A kind check kept next to the one place a page needs it.
const _: Kind = Kind::InvalidUpload;

/// Implements [`Page`] for the view models, which all hold a `chrome` field.
macro_rules! pages_with_chrome {
    ($($page:ty),+) => {$(
        impl Page for $page {
            fn chrome(&self) -> &Chrome {
                &self.chrome
            }
        }
    )+};
}

pages_with_chrome!(OverviewPage, NotFoundPage, TablePage, UploadPage);
