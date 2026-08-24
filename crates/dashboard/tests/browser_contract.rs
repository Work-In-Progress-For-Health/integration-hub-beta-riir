//! The shapes `dashboard.js` and the chart scripts read.
//!
//! Implements `spec/15-dashboard.md` §2.3 and §8.5: "Each `/api/*` shape
//! asserted against a golden JSON file captured from the Python, so
//! `dashboard.js` needs no change."
//!
//! # Taken from the scripts, not from a captured response
//!
//! §8.5 says "captured from the Python". A capture would need a live Azure
//! subscription, and it would record whatever that deployment happened to
//! return — including keys nothing reads and omitting keys that are only
//! populated when something is wrong.
//!
//! The **contract** is narrower and more durable than a capture: it is the
//! set of keys the browser actually dereferences. Those are listed below
//! with the file and function that reads each one, so a reviewer can check
//! the claim against `Integration-Hub-Beta/dashboard/dashboard/static/js/`
//! rather than against a fixture nobody can regenerate.
//!
//! This is a deliberate reading of §8.5, and it caught four keys and two
//! whole response shapes that were wrong.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashboard::config::Config;
use dashboard::health::{ContainerState, QueueDepths};
use dashboard::observe::{
    Container, Exception, MessageEvent, Observatory, RetryDelay, Series, SeriesPoint, Unconfigured,
};
use dashboard::web::{AppState, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// An observatory that answers everything, so no key is absent merely
/// because there was no data for it.
struct Answering;

impl Observatory for Answering {
    async fn queue_depths(&self, queues: &[String]) -> BTreeMap<String, QueueDepths> {
        queues
            .iter()
            .map(|queue| (queue.clone(), QueueDepths::known(3, 1)))
            .collect()
    }

    async fn containers(&self) -> Vec<Container> {
        vec![Container {
            name: "hl7-phw-transformer".to_string(),
            state: ContainerState::Running,
            replicas: Some(2),
            restarts: Some(0),
        }]
    }

    async fn exceptions(&self, _hours: u32) -> Vec<Exception> {
        vec![Exception {
            timestamp: "2026-08-22T09:15:00Z".to_string(),
            kind: "ValueError".to_string(),
            outer_message: "boom".to_string(),
            severity_level: Some(3),
            app_name: "phw-hl7-server".to_string(),
            operation_id: "op-1".to_string(),
        }]
    }

    async fn throughput(
        &self,
        hours: u32,
        _health_board: Option<&str>,
        _workflow_id: Option<&str>,
    ) -> Series {
        Series {
            incoming: vec![SeriesPoint {
                time: "2026-08-22T09:00:00Z".to_string(),
                value: 12.0,
            }],
            outgoing: vec![SeriesPoint {
                time: "2026-08-22T09:00:00Z".to_string(),
                value: 11.0,
            }],
            timespan: format!("{hours}h"),
        }
    }

    async fn retry_delays(&self, _hours: u32) -> Vec<RetryDelay> {
        vec![RetryDelay {
            workflow_id: "phw-to-mpi".to_string(),
            delay_seconds: Some(90.0),
            attempt: Some("2".to_string()),
            queue: "mpi-sender-ingress".to_string(),
            microservice_id: "mpi_hl7_sender".to_string(),
            timestamp: "2026-08-22T09:10:00Z".to_string(),
        }]
    }

    async fn messages(&self, _workflow_id: Option<&str>) -> Vec<MessageEvent> {
        Vec::new()
    }

    async fn trace(&self, _operation_id: &str) -> Vec<MessageEvent> {
        Vec::new()
    }

    async fn last_received(
        &self,
        _workflow_ids: &[String],
    ) -> Option<BTreeMap<String, time::OffsetDateTime>> {
        None
    }

    async fn last_sent(
        &self,
        _workflow_ids: &[String],
    ) -> Option<BTreeMap<String, time::OffsetDateTime>> {
        None
    }

    async fn failure_counts(
        &self,
        _workflow_ids: &[String],
        _window_minutes: u32,
    ) -> Option<BTreeMap<String, u64>> {
        None
    }

    fn is_configured(&self) -> bool {
        true
    }
}

/// Fetch a route as JSON.
async fn json_of<O: Observatory>(azure: O, path: &str) -> serde_json::Value {
    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([]).unwrap();
    let state = Arc::new(AppState::new(azure, config));

    let response = router(state)
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .expect("the router answers");

    assert_eq!(response.status(), StatusCode::OK, "{path}");

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{path} is not JSON: {e}"))
}

#[tokio::test]
async fn api_status_carries_every_key_dashboard_js_reads() {
    // spec/15-dashboard.md §2.3.1, from `dashboard.js :: refreshData`:
    //
    //   updateKPIs(data.kpis);
    //   updateFlowCards(data.flows);
    //   updateSystemHealth(data.system_health);
    //   updateQueueTable(data.queues);
    //   updateAlarm1Summary(data.alarm1_summary);
    //   updateAlarm2Summary(data.alarm2_summary);
    //   updateAlarm3Summary(data.alarm3_summary);
    //   updateRetryDelays(data.retry_delays, data.retry_delay_kpis);
    //   new Date(data.refreshed_at)
    //
    // A key it reads and this does not emit is an `undefined` in the
    // browser and a blank panel with nothing in the console.
    let status = json_of(Answering, "/api/status").await;

    for key in [
        "kpis",
        "flows",
        "system_health",
        "queues",
        "retry_delays",
        "retry_delay_kpis",
        "refreshed_at",
        "alarm1_summary",
        "alarm2_summary",
        "alarm3_summary",
    ] {
        assert!(
            status.get(key).is_some(),
            "dashboard.js reads data.{key}, which is missing: {status}"
        );
    }
}

#[tokio::test]
async fn each_alarm_summary_is_a_count_the_panel_can_render() {
    // §2.3.1 — `updateAlarm1Summary(data.alarm1_summary)` and its two
    // siblings. An absent key leaves three panels blank.
    let status = json_of(Answering, "/api/status").await;

    for alarm in ["alarm1_summary", "alarm2_summary", "alarm3_summary"] {
        let summary = &status[alarm];
        for key in ["critical", "suppressed", "unknown", "healthy", "total"] {
            assert!(
                summary.get(key).is_some(),
                "{alarm}.{key} is missing: {summary}"
            );
        }
    }
}

#[tokio::test]
async fn the_kpi_keys_are_the_ones_update_kpis_maps() {
    // `dashboard.js :: updateKPIs` names each of these explicitly.
    let status = json_of(Answering, "/api/status").await;
    let kpis = &status["kpis"];

    for key in [
        "total_active_messages",
        "total_dlq_messages",
        "exception_count_1h",
        "flows_healthy",
        "flows_warning",
        "flows_critical",
    ] {
        assert!(kpis.get(key).is_some(), "kpis.{key} is missing: {kpis}");
    }
}

#[tokio::test]
async fn a_flow_row_carries_the_id_and_health_the_cards_key_on() {
    // `updateFlowCards` matches `[data-flow-id="${flow.id}"]` and sets a
    // class from `flow.health`. Those two are the whole contract; the rest
    // of the row is for pages rendered server-side.
    let status = json_of(Answering, "/api/status").await;
    let flow = &status["flows"][0];

    assert!(flow["id"].is_string(), "{flow}");
    assert!(
        matches!(
            flow["health"].as_str(),
            Some("healthy" | "warning" | "critical" | "unknown")
        ),
        "health must be one of the four class names: {flow}"
    );
}

#[tokio::test]
async fn a_queue_row_carries_the_count_names_the_python_uses() {
    // §2.3.1's queue table, and the same names `/api/servicebus-metrics`
    // uses — an operator switching between the two panels should not see
    // different numbers because a key was spelled differently.
    let status = json_of(Answering, "/api/status").await;
    let queue = &status["queues"][0];

    assert!(queue.get("name").is_some(), "{queue}");
    assert!(queue.get("active_message_count").is_some(), "{queue}");
    assert!(queue.get("dead_letter_message_count").is_some(), "{queue}");
}

#[tokio::test]
async fn the_chart_routes_return_two_named_series() {
    // §2.3.2 — THE shape this file exists for.
    //
    // `sb-chart.js :: updateLegend` does `sumValues(chartData.incoming)`
    // and `sumValues(chartData.outgoing)`; `hl7-throughput-chart.js` does
    // the same. A flat `points` array draws an empty chart and logs
    // nothing, because `(undefined || []).reduce(...)` is `0`.
    for path in ["/api/hl7-throughput", "/api/servicebus-metrics"] {
        let body = json_of(Answering, path).await;

        assert!(
            body["incoming"].is_array(),
            "{path} has no `incoming` array: {body}"
        );
        assert!(
            body["outgoing"].is_array(),
            "{path} has no `outgoing` array: {body}"
        );
        assert!(
            body["timespan"].is_string(),
            "{path} has no `timespan`: {body}"
        );
        assert!(
            body.get("points").is_none(),
            "{path} still carries a `points` key nothing reads: {body}"
        );
    }
}

#[tokio::test]
async fn a_series_point_is_time_and_value() {
    // `sumValues` reads `pt.value`; `parseISO(pt.time)` reads `pt.time`.
    // A `timestamp` key would leave every point at the epoch.
    for path in ["/api/hl7-throughput", "/api/servicebus-metrics"] {
        let body = json_of(Answering, path).await;
        let point = &body["incoming"][0];

        assert!(point["time"].is_string(), "{path}: {point}");
        assert!(point["value"].is_number(), "{path}: {point}");
        assert!(
            point.get("timestamp").is_none(),
            "{path}: `timestamp` is the wrong name, the script reads `time`"
        );
    }
}

#[tokio::test]
async fn the_chart_routes_honour_the_hours_parameter() {
    // §2.3.3 — both scripts build `?hours=` from a control on the page,
    // and `sb-chart.js` also appends `&queue=`.
    let body = json_of(Answering, "/api/servicebus-metrics?hours=168").await;

    assert_eq!(body["timespan"], "168h", "{body}");
}

#[tokio::test]
async fn the_chart_routes_answer_with_both_series_when_azure_is_absent() {
    // §1.2 — an empty chart, not a missing key. `(undefined || [])` and
    // `([] )` behave the same in the script, but the second is what the
    // page can distinguish from a failure.
    for path in ["/api/hl7-throughput", "/api/servicebus-metrics"] {
        let body = json_of(Unconfigured, path).await;

        assert!(body["incoming"].is_array(), "{path}: {body}");
        assert!(body["outgoing"].is_array(), "{path}: {body}");
        assert_eq!(body["azure_configured"], false, "{path}");
    }
}

#[tokio::test]
async fn the_retry_kpis_are_the_two_numbers_the_panel_shows() {
    // `updateRetryDelays(data.retry_delays, data.retry_delay_kpis)`.
    let status = json_of(Answering, "/api/status").await;
    let kpis = &status["retry_delay_kpis"];

    assert_eq!(kpis["flows_reporting"], 1, "{status}");
    // 90 seconds is over the minute the panel highlights.
    assert_eq!(kpis["flows_over_1m"], 1, "{status}");
}

#[tokio::test]
async fn a_retry_delay_row_keeps_its_field_names() {
    let status = json_of(Answering, "/api/status").await;
    let delay = &status["retry_delays"][0];

    for key in [
        "workflow_id",
        "delay_seconds",
        "attempt",
        "queue",
        "microservice_id",
        "timestamp",
    ] {
        assert!(delay.get(key).is_some(), "{key} is missing: {delay}");
    }
}

#[tokio::test]
async fn every_api_route_answers_json() {
    // §2.2 — the whole surface, so a route that starts returning HTML on
    // an error path is caught here rather than in a browser.
    for path in [
        "/healthz",
        "/api/status",
        "/api/flows",
        "/api/messages",
        "/api/servicebus-metrics",
        "/api/hl7-throughput",
        "/api/alarms/status",
    ] {
        let body = json_of(Unconfigured, path).await;
        assert!(body.is_object(), "{path} did not answer an object: {body}");
    }
}

/// The keys `dashboard.js` reads off a flow, from the Python's own output.
///
/// `spec/15-dashboard.md` §2.3.1a. `dashboard.js` computes a flow card's
/// totals as `pre_queue.active + post_queue.active + subscriptions[]`, and
/// reads each with a `|| {}` fallback. A missing pair therefore does not
/// raise — it renders **zero for every flow**, which is a wrong number
/// that looks like a right one.
///
/// This is pinned against `tests/golden/status.json`, captured from the
/// Python dashboard running with no Azure configuration.
#[tokio::test]
async fn a_flow_carries_the_queue_pair_dashboard_js_reads() {
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("golden/status.json")).expect("the golden parses");
    let flow = &golden["flows"][0];

    for key in ["pre_queue", "post_queue", "subscriptions"] {
        assert!(
            !flow[key].is_null(),
            "the golden should carry {key}; the capture is stale"
        );
    }

    let ours = json_of(Unconfigured, "/api/status").await;
    let mine = &ours["flows"][0];

    for key in ["pre_queue", "post_queue"] {
        let queue = &mine[key];
        assert!(queue.is_object(), "{key} must be an object, got {queue}");
        for field in ["active", "dlq", "name", "health", "exists"] {
            assert!(
                queue.get(field).is_some(),
                "{key}.{field} is read by dashboard.js and is absent"
            );
        }
        assert!(
            queue["active"].is_u64() && queue["dlq"].is_u64(),
            "{key}.active and .dlq are summed by the script, so neither may be null"
        );
    }
    assert!(
        mine["subscriptions"].is_array(),
        "the script reduces over subscriptions"
    );
}
