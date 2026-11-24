//! The Network Test routes, end to end through the router.
//!
//! Implements `spec/15-dashboard.md` §10.4.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::app;
use dashboard::alarm::store::{Disabled, InMemory};

#[tokio::test]
async fn a_run_connects_records_history_and_lists_the_endpoint() {
    // §10.4.2, §10.4.4 -- against a real listener on loopback.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = app(InMemory::new());

    app.json_request(
        "POST",
        "/api/network-test/sources",
        &serde_json::json!({"description": "Local listener", "url": "127.0.0.1", "port": port}),
    )
    .await;

    let (status, result) = app
        .json_request(
            "POST",
            "/api/network-test/run",
            &serde_json::json!({"host": "127.0.0.1", "port": port}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["success"], true);
    assert_eq!(result["success_count"], 4);
    assert!(result.get("history_warning").is_none());

    let (_, history) = app
        .get_json(&format!(
            "/api/network-test/history?host=127.0.0.1&port={port}"
        ))
        .await;
    assert_eq!(history["samples"].as_array().unwrap().len(), 1);

    let (_, list) = app.get_json("/api/network-test/list").await;
    assert_eq!(list["endpoints"][0]["description"], "Local listener");

    let (status, deleted) = app
        .send(
            "DELETE",
            &format!("/api/network-test/history?host=127.0.0.1&port={port}"),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let (_, list) = app.get_json("/api/network-test/list").await;
    assert_eq!(list["endpoints"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_bad_target_is_a_400_that_echoes_nothing() {
    // §10.4.3.
    let app = app(InMemory::new());

    for body in [
        serde_json::json!({"host": "bad host; rm -rf /", "port": 80}),
        serde_json::json!({"host": "example.org", "port": 0}),
        serde_json::json!({}),
    ] {
        let (status, response) = app
            .json_request("POST", "/api/network-test/run", &body)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["error"], "Invalid host or port.");
    }

    let (status, _) = app
        .get_json("/api/network-test/history?host=x%20y&port=1")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, response) = app
        .json_request("POST", "/api/network-test/run", &serde_json::json!([1]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response["error"], "JSON body must be an object");
}

#[tokio::test]
async fn a_run_with_a_failing_history_store_still_returns_the_result_with_a_warning() {
    // §10.4.4 -- the test itself ran.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let store = InMemory::new();
    store.fail_writes("throttled");
    let app = app(store);

    let (status, result) = app
        .json_request(
            "POST",
            "/api/network-test/run",
            &serde_json::json!({"host": "127.0.0.1", "port": port}),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["success"], true);
    assert!(
        result["history_warning"]
            .as_str()
            .unwrap()
            .contains("could not be saved")
    );
    assert!(!result.to_string().contains("throttled"));
}

#[tokio::test]
async fn with_nothing_persisted_a_run_has_no_warning() {
    // §10.4.4 -- nothing was ever going to be kept.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = app(Disabled);

    let (_, result) = app
        .json_request(
            "POST",
            "/api/network-test/run",
            &serde_json::json!({"host": "127.0.0.1", "port": port}),
        )
        .await;

    assert!(result.get("history_warning").is_none(), "{result}");
}

#[tokio::test]
async fn sources_can_be_added_edited_and_deleted_and_duplicates_are_refused() {
    // §10.4.6, §10.4.7.
    let app = app(InMemory::new());
    let body = serde_json::json!({"description": "PHW", "url": "phw.example", "port": 2575});

    let (status, created) = app
        .json_request("POST", "/api/network-test/sources", &body)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (status, dup) = app
        .json_request(
            "POST",
            "/api/network-test/sources",
            &serde_json::json!({"description": "x", "url": "PHW.example", "port": 2575}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(dup["error"].as_str().unwrap().contains("already exists"));

    let (status, edited) = app
        .json_request(
            "PUT",
            &format!("/api/network-test/sources/{id}"),
            &serde_json::json!({"description": "PHW 2", "url": "phw.example", "port": 2575}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");

    let (_, listed) = app.get_json("/api/network-test/sources").await;
    assert_eq!(listed["sources"][0]["description"], "PHW 2");

    let (status, _) = app
        .send(
            "DELETE",
            &format!("/api/network-test/sources/{id}"),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = app.get_json("/api/network-test/sources").await;
    assert_eq!(listed["sources"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn sources_without_persistence_are_a_503_and_the_pages_render() {
    // §10.4.7, §1.2.
    let app = app(Disabled);

    let (status, body) = app
        .json_request(
            "POST",
            "/api/network-test/sources",
            &serde_json::json!({"description": "A", "url": "a.example", "port": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "Source persistence is not configured.");

    assert_eq!(app.get("/network-test").await.0, StatusCode::OK);
    assert_eq!(app.get("/network-test/config").await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_source_import_reports_and_the_picker_lists_the_result() {
    // §10.4.8.
    let app = app(InMemory::new());
    let csv = "description,url,port\nPHW,phw.example,2575\nBad,bad host,1\n";

    let (status, text) = app
        .send(
            "POST",
            "/api/network-test/sources/import",
            Some("text/csv"),
            csv.as_bytes().to_vec(),
        )
        .await;
    let result: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(result["imported"], 1);
    assert!(result["errors"][0].as_str().unwrap().starts_with("Row 3:"));

    let (_, page) = app.get("/network-test").await;
    assert!(page.contains("PHW (phw.example:2575)"), "{page}");
}

#[tokio::test]
async fn a_hostile_description_is_escaped_on_both_pages() {
    let app = app(InMemory::new());
    app.json_request(
        "POST",
        "/api/network-test/sources",
        &serde_json::json!({"description": "<script>alert(1)</script>", "url": "a.example", "port": 1}),
    )
    .await;

    for path in ["/network-test", "/network-test/config"] {
        let (_, page) = app.get(path).await;
        assert!(!page.contains("<script>alert(1)</script>"), "{path}");
    }
}

#[tokio::test]
async fn a_store_outage_is_a_503_that_names_no_cause() {
    // §10.4.4, §10.4.5: history that cannot be read or cleared, and a list
    // of tested endpoints that cannot be read, say so in one fixed sentence.
    let store = InMemory::new();
    store.fail("cosmos account key rejected");
    let app = app(store);
    let target = "host=127.0.0.1&port=2575";

    let (status, history) = app
        .get_json(&format!("/api/network-test/history?{target}"))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(history["error"], "History is currently unavailable.");

    let (status, deleted) = app
        .send(
            "DELETE",
            &format!("/api/network-test/history?{target}"),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        deleted.contains("History is currently unavailable."),
        "{deleted}"
    );
    assert!(!deleted.contains("cosmos"), "{deleted}");

    let (status, list) = app.get_json("/api/network-test/list").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!list.to_string().contains("cosmos"), "{list}");
}

#[tokio::test]
async fn a_bad_target_on_history_is_refused_for_read_and_delete() {
    // §10.4.3: the same validation guards every route that takes a target.
    let app = app(InMemory::new());
    for target in [
        "host=x%20y&port=1",
        "host=example.org&port=0",
        "host=example.org&port=abc",
    ] {
        let (status, body) = app
            .get_json(&format!("/api/network-test/history?{target}"))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{target}");
        assert_eq!(body["error"], "Invalid host or port.");
        let (status, _) = app
            .send(
                "DELETE",
                &format!("/api/network-test/history?{target}"),
                None,
                Vec::new(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{target}");
    }
}
