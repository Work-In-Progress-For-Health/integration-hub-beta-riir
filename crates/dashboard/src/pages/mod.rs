//! The pages and APIs that arrived with the October 2026 upstream sync.
//!
//! Implements `spec/15-dashboard.md` §10. Each lives in a module of its own
//! rather than in [`crate::web`], which is already the largest file in this
//! crate; each exposes `routes()`, merged by [`crate::web::router`].

pub mod contacts;
pub mod network_test;
pub mod pauses;

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};

/// A script served from the binary, as `/static/dashboard.css` is.
pub(crate) fn script(body: &'static str) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}

/// A JSON error body in the shape these routes use: `{"error": "…"}`.
pub(crate) fn error_json(status: axum::http::StatusCode, message: &str) -> Response {
    (
        status,
        axum::response::Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}
