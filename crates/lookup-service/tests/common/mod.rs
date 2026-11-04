//! Helpers shared by the test files.
//!
//! Mirrors the Python's `tests/helpers.py`.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lookup_service::config::{Config, SeedLocation};
use lookup_service::memory_store::MemoryTableStore;
use lookup_service::model::{Row, TableDefinition, Values};
use lookup_service::store::TableStore;
use lookup_service::web::{AppState, router};
use tower::ServiceExt;

/// The fixture tables (spec §7.0): not shipped, only tested against.
pub fn repo_seed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("seed")
}

/// A `LOCAL` configuration reading the repository seed directory.
pub fn local_config() -> Config {
    Config {
        seed: SeedLocation::Dir(repo_seed_dir()),
        environment: "LOCAL".to_owned(),
        // `LOCAL` runs with authentication off (spec §2.8); the tests of §12
        // build their own configurations.
        auth_disabled: true,
        ..Config::default()
    }
}

/// A row from a key and `column=value` pairs.
pub fn make_row(key: &[&str], values: &[(&str, &str)]) -> Row {
    Row {
        key: key.iter().map(|p| (*p).to_owned()).collect(),
        values: Values::new(
            values
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ),
    }
}

/// The `codes` table of the Python's helpers: one key `code`, one value
/// `label`.
pub fn codes_definition() -> TableDefinition {
    TableDefinition {
        name: "codes".to_owned(),
        description: String::new(),
        key_columns: vec!["code".to_owned()],
        value_columns: vec!["label".to_owned()],
        default_value_column: "label".to_owned(),
        key_normalisation: BTreeMap::new(),
        ttl_seconds: None,
        preload: false,
    }
}

/// A throwaway seed directory.
pub struct SeedDir {
    pub path: PathBuf,
}

impl SeedDir {
    pub fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lookup-service-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub fn write_csv(&self, name: &str, content: &str) {
        std::fs::write(self.path.join(format!("{name}.csv")), content).unwrap();
    }

    pub fn write_bytes(&self, name: &str, content: &[u8]) {
        std::fs::write(self.path.join(format!("{name}.csv")), content).unwrap();
    }

    pub fn write_manifest(&self, manifest: &serde_json::Value) {
        std::fs::write(
            self.path.join("tables.json"),
            serde_json::json!({ "tables": manifest }).to_string(),
        )
        .unwrap();
    }
}

impl Drop for SeedDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A started application over a store.
pub struct App<S: TableStore> {
    pub state: Arc<AppState<S>>,
    pub router: Router,
}

/// Start an application and its router.
pub async fn app<S: TableStore>(config: Config, store: S) -> App<S> {
    let state = Arc::new(AppState::new(config, store));
    state.start().await.unwrap();
    let router = router(Arc::clone(&state));
    App { state, router }
}

/// A router over a state that has not started.
pub fn unstarted<S: TableStore>(config: Config, store: S) -> Router {
    router(Arc::new(AppState::new(config, store)))
}

/// A response, read.
pub struct Reply {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    }
}

/// Send a request and read the whole response.
pub async fn send(router: &Router, request: Request<Body>) -> Reply {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// `GET path`.
pub async fn get(router: &Router, path: &str) -> Reply {
    send(router, Request::get(path).body(Body::empty()).unwrap()).await
}

/// A request with a JSON body.
pub async fn json_request(
    router: &Router,
    method: &str,
    path: &str,
    body: &serde_json::Value,
) -> Reply {
    send(
        router,
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

/// A multipart field.
pub enum Field<'a> {
    Text(&'a str, String),
    File(&'a str, &'a str, Vec<u8>),
}

/// A CSRF token of the right shape, and the cookie that holds it on a
/// `LOCAL` configuration (spec §12.5). The form helpers below send both, so
/// the tests of the pages and the API are about their subject; the tests of
/// the check itself build their requests by hand (`tests/auth.rs`).
pub const CSRF_TOKEN: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

/// `POST path` as multipart form data.
pub async fn post_multipart(router: &Router, path: &str, mut fields: Vec<Field<'_>>) -> Reply {
    fields.push(Field::Text("csrf_token", CSRF_TOKEN.to_owned()));
    let boundary = "----lookup-service-test-boundary";
    let mut body = Vec::new();
    for field in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match field {
            Field::Text(name, value) => {
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                );
                body.extend_from_slice(value.as_bytes());
            }
            Field::File(name, file_name, data) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file_name}\"\r\n\
                         Content-Type: text/csv\r\n\r\n"
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(&data);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    send(
        router,
        Request::post(path)
            .header("cookie", format!("lookup_csrf={CSRF_TOKEN}"))
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

/// `POST path` as an urlencoded form.
pub async fn post_form(router: &Router, path: &str, pairs: &[(&str, &str)]) -> Reply {
    let body = pairs
        .iter()
        .chain(std::iter::once(&("csrf_token", CSRF_TOKEN)))
        .map(|(k, v)| {
            format!(
                "{}={}",
                lookup_service::form::percent_encode(k),
                lookup_service::form::percent_encode(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    send(
        router,
        Request::post(path)
            .header("cookie", format!("lookup_csrf={CSRF_TOKEN}"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

/// A memory store holding nothing.
pub fn memory() -> MemoryTableStore {
    MemoryTableStore::default()
}
