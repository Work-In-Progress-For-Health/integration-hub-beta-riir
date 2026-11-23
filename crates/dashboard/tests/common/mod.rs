//! A tiny HTTP harness shared by the upstream-sync route tests
//! (`spec/15-dashboard.md` §10).

#![allow(dead_code, unreachable_pub, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashboard::alarm::store::AlarmStore;
use dashboard::config::Config;
use dashboard::observe::{Observatory, Unconfigured};
use dashboard::web::{AppState, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// An application over [`Unconfigured`] Azure and the given store.
pub struct App<S: AlarmStore> {
    /// The application state the router was built over.
    pub state: Arc<AppState<Unconfigured, S>>,
}

/// Build the application.
pub fn app<S: AlarmStore>(store: S) -> App<S> {
    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([]).unwrap();

    App {
        state: Arc::new(AppState::with_alarms(Unconfigured, store, config)),
    }
}

impl<S: AlarmStore> App<S> {
    /// Any request; the body is the status and the text.
    pub(crate) async fn send(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }

        let response = router(Arc::clone(&self.state))
            .oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();

        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(crate) async fn get(&self, path: &str) -> (StatusCode, String) {
        self.send("GET", path, None, Vec::new()).await
    }

    pub(crate) async fn json_request(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let (status, text) = self
            .send(
                method,
                path,
                Some("application/json"),
                body.to_string().into_bytes(),
            )
            .await;

        (
            status,
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)),
        )
    }

    pub(crate) async fn get_json(&self, path: &str) -> (StatusCode, serde_json::Value) {
        let (status, text) = self.get(path).await;

        (
            status,
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)),
        )
    }
}

/// A rule of `workflow_id`, enabled.
pub fn rule(
    store: &dashboard::alarm::store::InMemory,
    partition: dashboard::alarm::store::Partition,
    id: &str,
    workflow_id: &str,
) {
    store.add(
        partition,
        dashboard::alarm::RuleConfig {
            id: id.to_string(),
            display_name: format!("{id} name"),
            workflow_id: workflow_id.to_string(),
            ..dashboard::alarm::RuleConfig::for_test(workflow_id)
        },
    );
}

#[allow(unused)]
const fn _unused<O: Observatory>() {}
