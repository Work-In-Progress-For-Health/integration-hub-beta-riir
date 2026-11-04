//! The alarm pauses routes, end to end through the router.
//!
//! Implements `spec/15-dashboard.md` §10.2.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{app, rule};
use dashboard::alarm::store::{Disabled, InMemory, Partition};

fn create_body(extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "scope_type": "flows",
        "targets": ["phw-to-mpi"],
        "end_mode": "duration",
        "duration_minutes": 60,
        "reason": "planned maintenance",
        "requested_by": "ops",
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn store_with_rules() -> InMemory {
    let store = InMemory::new();
    rule(&store, Partition::Alarm1, "phw-in", "phw-to-mpi");
    rule(&store, Partition::Alarm3, "phw-fail", "phw-to-mpi");
    store
}

#[tokio::test]
async fn a_pause_can_be_created_listed_and_cancelled() {
    // §10.2.8.
    let app = app(store_with_rules());

    let (status, created) = app
        .json_request(
            "POST",
            "/api/alarm-pauses",
            &create_body(serde_json::json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["ok"], true);
    let id = created["pause"]["pause_id"].as_str().unwrap().to_string();

    let (_, listed) = app.get_json("/api/alarm-pauses").await;
    assert_eq!(listed["summary"]["active"], 1, "{listed}");
    assert_eq!(
        listed["groups"]["active"][0]["target_labels"][0],
        "PHW → MPI"
    );

    let (status, cancelled) = app
        .json_request(
            "POST",
            &format!("/api/alarm-pauses/{id}/cancel"),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["pause"]["status"], "ended");

    let (_, after) = app.get_json("/api/alarm-pauses").await;
    assert_eq!(after["summary"]["active"], 0);
    assert_eq!(after["groups"]["recent"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_pause_on_a_flow_pauses_its_alarms_on_the_status_api() {
    // §10.2.4, §10.2.5, §10.2.9 — the end of the chain: an operator pauses a
    // flow and the alarm status says so.
    let app = app(store_with_rules());
    app.json_request(
        "POST",
        "/api/alarm-pauses",
        &create_body(serde_json::json!({})),
    )
    .await;

    let (_, status) = app.get_json("/api/alarms/status").await;

    let rules: Vec<&serde_json::Value> = status["alarms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|a| a["rules"].as_array().unwrap())
        .collect();
    assert_eq!(rules.len(), 2);
    for rule in rules {
        assert_eq!(rule["status"], "paused", "{rule}");
        assert_eq!(rule["pause"]["paused_by"], "ops");
    }
    assert_eq!(status["pause_summary"]["active"], 1);
    assert_eq!(status["summary"]["paused"], 2);
}

#[tokio::test]
async fn a_rule_scoped_pause_leaves_the_other_alarms_running() {
    let app = app(store_with_rules());
    let body = create_body(serde_json::json!({
        "scope_type": "rule",
        "targets": [{"alarm_type": "alarm1", "rule_id": "phw-in"}],
    }));
    let (status, created) = app.json_request("POST", "/api/alarm-pauses", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (_, status) = app.get_json("/api/alarms/status").await;
    let by_alarm = |name: &str| {
        status["alarms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["alarm"] == name)
            .unwrap()["rules"][0]["status"]
            .clone()
    };
    assert_eq!(by_alarm("alarm1"), "paused");
    assert_ne!(by_alarm("alarm3"), "paused");
}

#[tokio::test]
async fn a_bad_request_is_a_400_with_a_message() {
    let app = app(store_with_rules());

    for (body, fragment) in [
        (
            create_body(serde_json::json!({"reason": ""})),
            "Reason is required",
        ),
        (
            create_body(serde_json::json!({"scope_type": "x"})),
            "Invalid pause scope",
        ),
        (
            create_body(
                serde_json::json!({"scope_type": "rule", "targets": [{"alarm_type": "alarm1", "rule_id": "ghost"}]}),
            ),
            "Unknown alarm rule",
        ),
    ] {
        let (status, response) = app.json_request("POST", "/api/alarm-pauses", &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert!(
            response["error"].as_str().unwrap().contains(fragment),
            "{response}"
        );
        assert_eq!(response["ok"], false);
    }

    let (status, response) = app
        .send(
            "POST",
            "/api/alarm-pauses",
            Some("application/json"),
            b"[1]".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert!(response.contains("Expected a JSON object"));
}

#[tokio::test]
async fn cancelling_something_that_does_not_exist_is_a_404() {
    let app = app(store_with_rules());

    let (status, _) = app
        .json_request(
            "POST",
            "/api/alarm-pauses/nope/cancel",
            &serde_json::json!({}),
        )
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nothing_is_persisted_so_a_pause_is_refused_with_a_503() {
    // §10.2.6 — never accepted-and-dropped.
    let app = app(Disabled);

    let (status, response) = app
        .json_request(
            "POST",
            "/api/alarm-pauses",
            &create_body(serde_json::json!({"scope_type": "all", "targets": []})),
        )
        .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("not configured")
    );

    // And the page still renders (§1.2).
    let (status, page) = app.get("/alarms/pauses").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("COSMOS_ENDPOINT"), "says why");
}

#[tokio::test]
async fn an_unreadable_store_is_said_on_the_page_not_shown_as_empty() {
    // §10.2.6.
    let store = store_with_rules();
    store.fail("503");
    let app = app(store);

    let (status, page) = app.get("/alarms/pauses").await;

    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("could not be read"), "{page}");
    let (_, api) = app.get_json("/api/alarm-pauses").await;
    assert_eq!(api["unavailable"], true);
}

#[tokio::test]
async fn an_operator_typed_reason_is_escaped_on_the_page() {
    // The reason is free text from an operator and is rendered into a page
    // other operators read.
    let app = app(store_with_rules());
    app.json_request(
        "POST",
        "/api/alarm-pauses",
        &create_body(serde_json::json!({"reason": "<script>alert(1)</script>"})),
    )
    .await;

    let (_, page) = app.get("/alarms/pauses").await;

    assert!(!page.contains("<script>alert(1)</script>"), "unescaped");
    assert!(
        page.contains("&lt;script&gt;alert(1)&lt;/script&gt;") || page.contains("&#60;script&#62;"),
        "{page}"
    );
}

#[tokio::test]
async fn the_alarm_page_links_to_the_pauses_and_counts_them() {
    let app = app(store_with_rules());
    app.json_request(
        "POST",
        "/api/alarm-pauses",
        &create_body(serde_json::json!({})),
    )
    .await;

    let (_, page) = app.get("/alarms").await;

    assert!(page.contains("1 paused / 0 scheduled"), "{page}");
    assert!(page.contains("/alarms/pauses"));
}
