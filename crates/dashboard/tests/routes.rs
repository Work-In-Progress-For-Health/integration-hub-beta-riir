//! Every route, with no Azure configuration at all.
//!
//! Implements `spec/15-dashboard.md` §8.1: "Every route renders with **no**
//! Azure configuration (§1.2). This is the first test written and the last
//! one allowed to fail."
//!
//! # Why it is the first and the last
//!
//! §1.2 is what makes the dashboard developable on a laptop and testable in
//! CI. Every other test in this crate runs against `Unconfigured`, so if
//! this file fails, nothing else in the suite means anything.
//!
//! The second thing it checks is §2.2.2: an unconfigured dashboard must
//! render **unknown**, never zero. A page of confident zeroes during an
//! Azure outage is worse than a page that says it cannot see, because an
//! operator would believe it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty,
    clippy::unused_async_trait_impl,
    clippy::missing_docs_in_private_items,
    clippy::needless_pass_by_value,
    clippy::needless_collect
)]

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

/// Every page and every API route of §2.
const ROUTES: [&str; 22] = [
    "/",
    "/flows",
    "/exceptions",
    "/service-bus",
    "/alarms",
    "/messages",
    "/trace/op-1",
    "/api/messages",
    "/api/alarms/status",
    "/healthz",
    "/api/status",
    "/api/flows",
    "/api/servicebus-metrics",
    "/api/hl7-throughput",
    // spec/15-dashboard.md §10 -- the upstream sync's pages, which §1.2
    // requires to render with nothing configured and nothing persisted.
    "/alarms/pauses",
    "/api/alarm-pauses",
    "/api/alarm-pauses/options",
    "/contacts",
    "/network-test",
    "/network-test/config",
    "/api/network-test/list",
    "/api/network-test/sources",
];

/// An application over an observatory, and optionally an alarm store.
struct App<O, S = dashboard::alarm::store::Disabled> {
    state: Arc<AppState<O, S>>,
}

impl<O: Observatory, S: dashboard::alarm::store::AlarmStore> App<O, S> {
    async fn get(&self, path: &str) -> (StatusCode, String) {
        let response = router(Arc::clone(&self.state))
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .expect("the router answers");

        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body is readable")
            .to_bytes();

        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn json(&self, path: &str) -> serde_json::Value {
        let (status, body) = self.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path} is not JSON: {e}\n{body}"))
    }
}

/// An application with alarms not persisted — §5.6's first row, which is
/// the normal state on a laptop.
fn app_with<O: Observatory>(azure: O) -> App<O> {
    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([])
        .expect("no configuration is a configuration");

    App {
        state: Arc::new(AppState::new(azure, config)),
    }
}

/// An observatory that answers, so the "configured" side can be checked too.
struct Live;

impl Observatory for Live {
    async fn queue_depths(&self, queues: &[String]) -> BTreeMap<String, QueueDepths> {
        queues
            .iter()
            .enumerate()
            .map(|(index, queue)| {
                (
                    queue.clone(),
                    QueueDepths::known(index as u64, u64::from(index == 0)),
                )
            })
            .collect()
    }

    async fn topics(&self) -> Option<Vec<dashboard::topics::Topic>> {
        use dashboard::topics::{Subscription, Topic};
        Some(vec![Topic {
            name: "sbt-mpi".to_string(),
            depths: QueueDepths::known(1, 0),
            subscriptions: vec![Subscription {
                name: "sub-a".to_string(),
                topic: "sbt-mpi".to_string(),
                depths: QueueDepths::known(0, 2),
            }],
            subscriptions_read: true,
        }])
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
            timestamp: "2026-08-21T09:15:00Z".to_string(),
            kind: "ValueError".to_string(),
            outer_message: "a marker only this test uses: 7f3c9a".to_string(),
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
                time: "2026-08-21T09:00:00Z".to_string(),
                value: 12.0,
            }],
            outgoing: vec![SeriesPoint {
                time: "2026-08-21T09:00:00Z".to_string(),
                value: 11.0,
            }],
            // Echoed back, so a route that ignored `hours` is caught
            // here rather than by a chart drawing the wrong window.
            timespan: format!("{hours}h"),
        }
    }

    async fn retry_delays(&self, _hours: u32) -> Vec<RetryDelay> {
        Vec::new()
    }

    async fn messages(&self, _workflow_id: Option<&str>) -> Vec<MessageEvent> {
        vec![MessageEvent {
            timestamp: "2026-08-22T09:15:00Z".to_string(),
            event_type: "MESSAGE_RECEIVED".to_string(),
            workflow_id: "phw-to-mpi".to_string(),
            microservice_id: "phw_hl7_server".to_string(),
            correlation_id: "3f6d".to_string(),
            operation_id: "op-1".to_string(),
            // A real NHS number and a real name, so §1.3 is exercised
            // through the page rather than only in a unit test.
            payload: Some(
                "MSH|^~\\&|252|252|100|100|20260822||ADT^A31|M1|P|2.5\r\
                 PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M"
                    .to_string(),
            ),
        }]
    }

    async fn trace(&self, _operation_id: &str) -> Vec<MessageEvent> {
        self.messages(None).await
    }

    async fn last_received(
        &self,
        workflow_ids: &[String],
    ) -> Option<BTreeMap<String, time::OffsetDateTime>> {
        // Long ago, so an inactivity rule trips.
        Some(
            workflow_ids
                .iter()
                .map(|id| (id.clone(), time::macros::datetime!(2020-01-01 00:00 UTC)))
                .collect(),
        )
    }

    async fn last_sent(
        &self,
        workflow_ids: &[String],
    ) -> Option<BTreeMap<String, time::OffsetDateTime>> {
        self.last_received(workflow_ids).await
    }

    async fn failure_counts(
        &self,
        _workflow_ids: &[String],
        _window_minutes: u32,
    ) -> Option<BTreeMap<String, u64>> {
        // The query ran and found nothing — every workflow absent, which
        // means zero failures and is healthy. Distinct from `None`.
        Some(BTreeMap::new())
    }

    fn is_configured(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn every_route_answers_with_no_azure_configuration() {
    // spec/15-dashboard.md §1.2 and §8.1 — the first test written and the
    // last one allowed to fail.
    let app = app_with(Unconfigured);

    for route in ROUTES {
        let (status, body) = app.get(route).await;
        assert_eq!(status, StatusCode::OK, "{route} answered {status}: {body}");
        assert!(!body.is_empty(), "{route} answered with nothing");
    }
}

#[tokio::test]
async fn every_route_answers_with_the_real_observatory_and_no_azure() {
    // spec/15-dashboard.md §1.2 and §8.1, against `LiveObservatory` rather
    // than `Unconfigured` — which is the version that actually ships.
    //
    // `Unconfigured` passing proves the seam works. This proves the *real*
    // implementation takes the same path: it must not reach the network,
    // must not block, and must not fail, on a laptop with no credentials
    // and no route to Azure.
    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([]).unwrap();
    let app = App {
        state: Arc::new(AppState::new(
            dashboard::azure::live::LiveObservatory::new(&config)
                .expect("no CA bundle is configured in tests"),
            config,
        )),
    };

    let started = std::time::Instant::now();

    for route in ROUTES {
        let (status, body) = app.get(route).await;
        assert_eq!(status, StatusCode::OK, "{route} answered {status}: {body}");
    }

    // If any of these had tried to reach Azure, this would be the timeout
    // of `azure::http::REQUEST_TIMEOUT` times the number of panels rather
    // than an instant.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "the unconfigured path reached the network: {:?}",
        started.elapsed()
    );

    // And it says so, rather than reporting zeroes.
    let status = app.json("/api/status").await;
    assert_eq!(status["azure_configured"], false);
    assert!(
        status["kpis"]["total_active_messages"].is_null(),
        "{status}"
    );
}

#[tokio::test]
async fn an_unconfigured_dashboard_says_unknown_rather_than_zero() {
    // §2.2.2 and §2.2.3 — THE test of this file after §8.1 itself. A page
    // of confident zeroes during an Azure outage is worse than a page that
    // says it cannot see, because an operator would believe it.
    let app = app_with(Unconfigured);

    let status = app.json("/api/status").await;

    assert_eq!(status["azure_configured"], false);
    assert_eq!(status["system_health"], "unknown");
    assert!(
        status["kpis"]["total_active_messages"].is_null(),
        "{status}"
    );
    assert!(status["kpis"]["total_dlq_messages"].is_null(), "{status}");
    assert_eq!(status["kpis"]["flows_unknown"], 8);
    assert_eq!(status["kpis"]["flows_healthy"], 0);

    // And every flow row says so too.
    for flow in status["flows"].as_array().expect("flows is an array") {
        assert_eq!(flow["health"], "unknown", "{flow}");
        assert!(flow["active"].is_null(), "{flow}");
    }
}

#[tokio::test]
async fn the_pages_show_unknown_rather_than_zero_too() {
    // The same clause, in the HTML an operator actually reads. A JSON-only
    // test would pass with a template that rendered `None` as `0`.
    let app = app_with(Unconfigured);

    for route in ["/", "/flows", "/service-bus"] {
        let (_, body) = app.get(route).await;

        assert!(
            body.to_lowercase().contains("unknown"),
            "{route} does not say unknown anywhere"
        );
        assert!(
            body.contains("Azure is not configured"),
            "{route} does not carry §2.2.2's flag"
        );
    }
}

#[tokio::test]
async fn healthz_does_not_depend_on_azure() {
    // A dashboard that reported itself unhealthy because a *watched*
    // service was unreachable would be restarted by its orchestrator at
    // exactly the moment somebody needed to look at it.
    let app = app_with(Unconfigured);

    let health = app.json("/healthz").await;

    assert_eq!(health["status"], "ok");
}

#[tokio::test]
async fn a_configured_dashboard_reports_real_numbers() {
    // The other side of §2.2.2: when Azure *is* reachable, the totals are
    // present and the flag is set.
    let app = app_with(Live);

    let status = app.json("/api/status").await;

    assert_eq!(status["azure_configured"], true);
    assert!(
        status["kpis"]["total_active_messages"].is_number(),
        "{status}"
    );
    assert_eq!(status["kpis"]["queues_unknown"], 0);
    assert_eq!(status["kpis"]["exception_count_1h"], 1);
}

#[tokio::test]
async fn the_status_payload_keeps_its_shape() {
    // §8.5 — a contract with `dashboard.js`.
    let app = app_with(Live);
    let status = app.json("/api/status").await;

    for key in [
        "refreshed_at",
        "system_health",
        "azure_configured",
        "kpis",
        "flows",
        "recent_exceptions",
        "retry_delays",
    ] {
        assert!(status.get(key).is_some(), "{key} is missing: {status}");
    }

    let flow = &status["flows"][0];
    for key in [
        "id",
        "label",
        "source",
        "destination",
        "source_port",
        "transformer",
        "health",
        "active",
        "dlq",
    ] {
        assert!(flow.get(key).is_some(), "{key} is missing: {flow}");
    }
}

#[tokio::test]
async fn refresh_rebuilds_and_answers() {
    // §2.2's `/api/refresh`, on both verbs — the page uses POST and a
    // developer reaches for the URL bar.
    let app = app_with(Live);

    let (get_status, _) = app.get("/api/refresh").await;
    assert_eq!(get_status, StatusCode::OK);

    let response = router(Arc::clone(&app.state))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/refresh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_queue_metrics_distinguish_unknown_from_empty() {
    // §2.2.2 on `/api/servicebus-metrics`, which is what the Service Bus
    // page polls.
    let unknown = app_with(Unconfigured).json("/api/servicebus-metrics").await;

    for queue in unknown["queues"].as_array().unwrap() {
        assert!(queue["active_message_count"].is_null(), "{queue}");
        assert_eq!(queue["known"], false, "{queue}");
    }

    let known = app_with(Live).json("/api/servicebus-metrics").await;
    for queue in known["queues"].as_array().unwrap() {
        assert!(queue["active_message_count"].is_number(), "{queue}");
        assert_eq!(queue["known"], true);
    }
}

#[tokio::test]
async fn the_throughput_window_is_bounded() {
    // `hours` reaches a KQL `ago({hours}h)`. An unbounded value is a query
    // that scans the whole workspace retention and times the page out —
    // the same idea as §4.4's sanitiser, for a number.
    let app = app_with(Live);

    // The window comes back as §2.3.2's `timespan`, which is the key the
    // chart scripts read — not an `hours` number nothing looks at.
    assert_eq!(app.json("/api/hl7-throughput").await["timespan"], "24h");
    assert_eq!(
        app.json("/api/hl7-throughput?hours=6").await["timespan"],
        "6h"
    );

    // The edges are valid; anything outside them is refused naming `hours`
    // and its range, never moved to the nearest bound (§2.3.3, RCA-IN-2).
    assert_eq!(
        app.json("/api/hl7-throughput?hours=720").await["timespan"],
        "720h"
    );
    assert_eq!(
        app.json("/api/hl7-throughput?hours=1").await["timespan"],
        "1h"
    );
    for hours in ["999999", "721", "0", "-1", ""] {
        let (status, body) = app.get(&format!("/api/hl7-throughput?hours={hours}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "hours={hours:?}");
        assert!(
            body.contains("hours must be a whole number from 1 to 720"),
            "{body}"
        );
    }
}

#[tokio::test]
async fn a_nonsense_hours_is_refused_and_does_not_fall_back_to_the_default() {
    // §2.3.3, RCA-IN-2: a supplied `hours` that is not a number is a client
    // error naming it. Only an *absent* `hours` takes the default.
    let app = app_with(Live);

    let (status, body) = app.get("/api/hl7-throughput?hours=banana").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("hours"), "{body}");

    let (status, _) = app.get("/api/hl7-throughput").await;
    assert_eq!(status, StatusCode::OK, "absent takes the default");
}

#[tokio::test]
async fn exception_text_is_escaped_rather_than_injected() {
    // An exception message contains whatever an upstream system put in it,
    // rendered on a page an operator is logged into.
    struct Hostile;

    impl Observatory for Hostile {
        async fn queue_depths(&self, _queues: &[String]) -> BTreeMap<String, QueueDepths> {
            BTreeMap::new()
        }
        async fn containers(&self) -> Vec<Container> {
            Vec::new()
        }
        async fn exceptions(&self, _hours: u32) -> Vec<Exception> {
            vec![Exception {
                timestamp: "2026-08-21T09:15:00Z".to_string(),
                kind: "<script>alert('x')</script>".to_string(),
                outer_message: "<img src=x onerror=alert(1)>".to_string(),
                severity_level: None,
                app_name: "x".to_string(),
                operation_id: "y".to_string(),
            }]
        }
        async fn throughput(
            &self,
            _hours: u32,
            _health_board: Option<&str>,
            _workflow_id: Option<&str>,
        ) -> Series {
            Series::default()
        }
        async fn retry_delays(&self, _hours: u32) -> Vec<RetryDelay> {
            Vec::new()
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

    let app = app_with(Hostile);

    for route in ["/", "/exceptions"] {
        let (_, body) = app.get(route).await;

        // What matters is that no *element* reaches the browser. The
        // attribute text `onerror=alert(1)` survives escaping and is
        // harmless: with its angle brackets escaped it is a text node, not
        // a tag, so there is nothing for the attribute to attach to.
        assert!(!body.contains("<script"), "{route}: a script tag survived");
        assert!(!body.contains("<img"), "{route}: an img tag survived");
        assert!(
            body.contains("&#60;script&#62;alert(&#39;x&#39;)"),
            "{route}: the type was not rendered escaped"
        );
        assert!(
            body.contains("&#60;img src=x onerror=alert(1)&#62;"),
            "{route}: the message was not rendered escaped"
        );
    }
}

#[tokio::test]
async fn a_page_never_shows_an_unredacted_payload() {
    // spec/15-dashboard.md §1.3 and §8.6 — the patient-safety correction
    // over the Python, checked in the HTML an operator actually sees.
    //
    // The unit test in `observe` proves `rendered` redacts. This proves the
    // *page* uses it: a template reaching for the raw field would pass that
    // test and fail this one.
    let app = app_with(Live);

    for route in ["/messages", "/trace/op-1"] {
        let (status, body) = app.get(route).await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            !body.contains("8888888"),
            "{route}: an NHS number reached the page"
        );
        assert!(
            !body.contains("SURNAME"),
            "{route}: a name reached the page"
        );

        // Still identifiable: §2.6's allow-list keeps the header.
        assert!(body.contains("ADT^A31"), "{route}: {body}");
        // And the page says what it did, so `***` is not read as data.
        assert!(
            body.contains("redacted"),
            "{route} does not say it redacted"
        );
    }
}

#[tokio::test]
async fn the_message_api_never_carries_a_payload_at_all() {
    // §1.3, one layer down. `/api/messages` is JSON a browser or a script
    // can keep; putting raw patient data there would be outside anything
    // the page's redaction can reach.
    let app = app_with(Live);

    let (status, body) = app.get("/api/messages").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("8888888"), "{body}");
    assert!(!body.contains("payload"), "{body}");
    // The metadata is still there, which is what the route is for.
    assert!(body.contains("MESSAGE_RECEIVED"), "{body}");
    assert!(body.contains("3f6d"), "{body}");
}

#[tokio::test]
async fn turning_redaction_off_shows_the_message_and_says_so() {
    // §1.3 — "unless the deployment explicitly disables redaction". It is
    // allowed, and the page must not pretend otherwise.
    let config = Config::from_pairs([("DASHBOARD_REDACT_MESSAGES", "false")])
        .expect("a valid configuration");
    let app = App {
        state: Arc::new(AppState::new(Live, config)),
    };

    let (_, body) = app.get("/messages").await;

    assert!(
        body.contains("8888888"),
        "the payload should be shown in full"
    );
    assert!(
        body.contains("Redaction is <strong>off</strong>"),
        "the page must say redaction is off: {body}"
    );
}

#[tokio::test]
async fn the_alarm_page_says_persistence_is_disabled() {
    // spec/15-dashboard.md §5.6's first row and §8.3. With no
    // `COSMOS_ENDPOINT` the page renders and says nothing is stored —
    // rather than showing an empty rule list that looks like a clean bill
    // of health.
    let app = app_with(Unconfigured);

    let (status, body) = app.get("/alarms").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("not persisted"), "{body}");
    assert!(body.contains("COSMOS_ENDPOINT"), "{body}");
}

#[tokio::test]
async fn a_store_outage_is_named_rather_than_looking_like_no_rules() {
    // §5.6 — THE distinction. A Cosmos outage and "no rules configured"
    // must not render the same, or an operator reads an estate with every
    // alarm silently switched off as an estate with nothing wrong.
    use dashboard::alarm::RuleConfig;
    use dashboard::alarm::store::{InMemory, Partition};

    let store = InMemory::new();
    store.add(Partition::Alarm1, RuleConfig::for_test("phw-to-mpi"));
    store.fail("Cosmos returned 503");

    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([]).unwrap();
    let app = App {
        state: Arc::new(AppState::with_alarms(Unconfigured, store, config)),
    };

    let (status, body) = app.get("/alarms").await;

    assert_eq!(status, StatusCode::OK, "the page still renders");
    assert!(body.contains("could not be read"), "{body}");
    assert!(body.contains("not being evaluated"), "{body}");

    // And the API says so too, so a browser can render it.
    let json = app.json("/api/alarms/status").await;
    let unavailable = json["unavailable"].as_array().expect("an array");
    assert_eq!(unavailable.len(), 3, "{json}");
}

#[tokio::test]
async fn configured_rules_are_evaluated_and_reported_unknown_without_data() {
    // §4.5 and §5.1.2 step 3 — with no Log Analytics there is nothing to
    // measure, and a rule with nothing to measure is `unknown`, never
    // `healthy`. Reporting healthy would be a clean bill of health nobody
    // checked.
    use dashboard::alarm::RuleConfig;
    use dashboard::alarm::store::{InMemory, Partition};

    let store = InMemory::new();
    store.add(Partition::Alarm1, RuleConfig::for_test("phw-to-mpi"));
    store.add(Partition::Alarm3, RuleConfig::for_test("phw-to-mpi"));

    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([]).unwrap();
    let app = App {
        state: Arc::new(AppState::with_alarms(Unconfigured, store, config)),
    };

    let json = app.json("/api/alarms/status").await;

    assert_eq!(json["summary"]["unknown"], 2, "{json}");
    assert_eq!(json["summary"]["healthy"], 0, "{json}");
    assert_eq!(json["summary"]["total"], 2);

    // And the page shows them.
    let (_, body) = app.get("/alarms").await;
    assert!(body.contains("phw-to-mpi"), "{body}");
    assert!(body.contains("Unknown"), "{body}");
}

#[tokio::test]
async fn the_alarms_measure_when_azure_answers() {
    // spec/15-dashboard.md §5.3–§5.5, end to end. `Live` reports last
    // activity in 2020, so both inactivity alarms trip; and it reports a
    // failure query that **ran and found nothing**, so Alarm 3 is healthy.
    use dashboard::alarm::RuleConfig;
    use dashboard::alarm::store::{InMemory, Partition};

    let store = InMemory::new();
    for partition in Partition::all() {
        store.add(partition, RuleConfig::for_test("phw-to-mpi"));
    }

    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([]).unwrap();
    let app = App {
        state: Arc::new(AppState::with_alarms(Live, store, config)),
    };

    let json = app.json("/api/alarms/status").await;
    let by_alarm: std::collections::BTreeMap<String, String> = json["alarms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["alarm"].as_str().unwrap().to_string(),
                entry["rules"][0]["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();

    // §5.3.2 and §5.4.2 — years of silence is well past any threshold.
    assert_eq!(by_alarm["alarm1"], "critical", "{json}");
    assert_eq!(by_alarm["alarm2"], "critical", "{json}");

    // §5.5.2 — THE distinction. The failure query ran and returned no row
    // for this workflow, which means **zero failures**, which is healthy.
    // If a missing row were read as "unknown", a quiet estate would show
    // every failure alarm as unreadable for ever.
    assert_eq!(
        by_alarm["alarm3"], "healthy",
        "a query that ran and found nothing means zero failures: {json}"
    );
}

#[tokio::test]
async fn a_failure_query_that_did_not_run_is_unknown_not_healthy() {
    // The other half of §5.5.2, and the one that matters for safety: a
    // broken query must never read as "no failures".
    use dashboard::alarm::RuleConfig;
    use dashboard::alarm::store::{InMemory, Partition};

    let store = InMemory::new();
    store.add(Partition::Alarm3, RuleConfig::for_test("phw-to-mpi"));

    // `Unconfigured` returns `None` from `failure_counts` — the query did
    // not run.
    let config = Config::from_pairs::<[(&str, &str); 0], _, _>([]).unwrap();
    let app = App {
        state: Arc::new(AppState::with_alarms(Unconfigured, store, config)),
    };

    let json = app.json("/api/alarms/status").await;

    assert_eq!(json["summary"]["unknown"], 1, "{json}");
    assert_eq!(
        json["summary"]["healthy"], 0,
        "a query that did not run must not report a clean bill of health: {json}"
    );
}

/// `POST` a form, returning the status and the headers.
async fn post_form<O: Observatory, S: dashboard::alarm::store::AlarmStore>(
    app: &App<O, S>,
    path: &str,
    body: &str,
    referer: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded");

    if let Some(referer) = referer {
        builder = builder.header("referer", referer);
    }

    let response = router(Arc::clone(&app.state))
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .expect("the router answers");

    (response.status(), response.headers().clone())
}

#[tokio::test]
async fn setting_a_language_stores_it_and_returns_where_it_came_from() {
    // spec/15-dashboard.md §6.2 and §6.4. The route is `/set-language`,
    // POST — not `/language`, which an earlier draft of §2.1 said and
    // nothing posts to (§9.11 is the same class of mistake).
    let app = app_with(Unconfigured);

    let (status, headers) = post_form(
        &app,
        "/set-language",
        "lang=cy",
        Some("https://dash.example/flows?hours=6"),
    )
    .await;

    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get("location").unwrap(),
        "/flows?hours=6",
        "the filtered page a user was on must stay filtered"
    );

    let cookie = headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.contains("lang=cy"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
}

#[tokio::test]
async fn the_language_redirect_can_never_leave_the_site() {
    // §6.4 — the `Referer` is client-supplied. The unit tests cover the
    // parsing; this covers the *route*, because a handler that used the
    // header directly would pass those and fail this.
    let app = app_with(Unconfigured);

    for referer in [
        "https://evil.example/steal",
        "//evil.example/steal",
        "/\\evil.example",
    ] {
        let (status, headers) = post_form(&app, "/set-language", "lang=en", Some(referer)).await;

        assert_eq!(status, StatusCode::SEE_OTHER);
        let location = headers.get("location").unwrap().to_str().unwrap();

        assert!(location.starts_with('/'), "{referer} -> {location}");
        assert!(!location.starts_with("//"), "{referer} -> {location}");
        assert!(
            !location.contains("//evil.example"),
            "{referer} -> {location}"
        );
    }
}

#[tokio::test]
async fn an_unrecognised_language_falls_back_rather_than_failing() {
    // §6.2 and §1.2 — this arrives from a form.
    let app = app_with(Unconfigured);

    for body in ["lang=fr", "lang=", "", "lang=%F0%9F%8F%A5"] {
        let (status, headers) = post_form(&app, "/set-language", body, None).await;

        assert_eq!(status, StatusCode::SEE_OTHER, "{body:?}");
        assert!(
            headers
                .get("set-cookie")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("lang=en"),
            "{body:?} should fall back to English"
        );
        // With no referer, back to the overview.
        assert_eq!(headers.get("location").unwrap(), "/");
    }
}

#[tokio::test]
async fn an_unknown_path_is_a_404() {
    let app = app_with(Unconfigured);

    assert_eq!(app.get("/nope").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_stylesheet_carries_the_brand() {
    // The state colours are the load-bearing part of this page on a wall
    // display, and §3.3 requires unknown to be distinguishable from
    // healthy rather than a lighter shade of it.
    let app = app_with(Unconfigured);

    let (status, css) = app.get("/static/dashboard.css").await;

    assert_eq!(status, StatusCode::OK);
    assert!(css.contains("#325083"), "NHS Wales Blue is missing");
    assert!(
        css.contains("--unknown:"),
        "unknown has no colour of its own"
    );
}

#[tokio::test]
async fn topics_and_subscriptions_reach_the_page_and_the_api() {
    // spec/15-dashboard.md §10.5.5.
    let app = app_with(Live);

    let (_, page) = app.get("/service-bus").await;
    assert!(page.contains("sbt-mpi") && page.contains("sub-a"), "{page}");

    let metrics = app.json("/api/servicebus-metrics").await;
    assert_eq!(
        metrics["topics"][0]["subscriptions"][0]["entity_name"],
        "sbt-mpi/sub-a"
    );
    assert_eq!(
        metrics["topics"][0]["subscriptions"][0]["health"],
        "critical"
    );
    assert_eq!(
        metrics["namespace_kpis"]["subscription_dead_letter_messages"],
        2
    );
    assert!(
        metrics["queues"].is_array(),
        "the existing fields are unchanged (§2.3.2)"
    );
}

#[tokio::test]
async fn unreadable_topics_are_said_not_shown_as_none() {
    // §10.5.2 -- Unconfigured cannot ask.
    let app = app_with(Unconfigured);

    let (_, page) = app.get("/service-bus").await;
    assert!(page.contains("could not be read"), "{page}");

    let metrics = app.json("/api/servicebus-metrics").await;
    assert_eq!(metrics["topics_known"], false);
    assert!(metrics["namespace_kpis"].is_null());
}
