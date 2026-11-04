//! Every route, against a fake reader.
//!
//! Implements `spec/14-buswatch.md` §8.4: "every route renders with a fake
//! reader, including the empty-queue and unreachable-queue cases".
//!
//! # Why a fake and not the emulator
//!
//! The unreachable case. §6.2 requires a queue that cannot be read to show
//! its error while the rest of the page renders, and there is no way to ask
//! a running emulator to fail one queue and serve the others. The clause
//! that matters most is the one only a fake can reach.
//!
//! Driven through `tower::ServiceExt::oneshot` rather than a socket: these
//! are tests of the routes, not of `hyper`, and the well-formed requests a
//! browser sends are exactly what a `Request::builder` produces.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use buswatch::config::Config;
use buswatch::fake::FakeReader;
use buswatch::web::{AppState, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A real HL7 message, so the ER7 rendering of §5.4 has something to split.
const ER7: &str = "MSH|^~\\&|252|252|100|100|20260821||ADT^A31^ADT_A05|M1|P|2.5\r\
                   EVN||20260821092900\r\
                   PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M";

/// The queues these tests configure.
const QUEUES: &str = "alpha,beta,broken";

/// A running application, and the fake behind it.
struct App {
    state: Arc<AppState<FakeReader>>,
}

impl App {
    /// An application over an empty fake.
    fn new() -> Self {
        let config = Config::from_pairs([("BUSWATCH_QUEUE_NAMES", QUEUES)])
            .expect("the defaults cover everything else");

        Self {
            state: Arc::new(AppState::new(FakeReader::new(), config)),
        }
    }

    /// The fake, to arrange a case.
    fn reader(&self) -> &FakeReader {
        &self.state.reader
    }

    /// `GET path`, as a status and a body.
    async fn get(&self, path: &str) -> (StatusCode, String) {
        self.send(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
    }

    /// `POST path`, as a status and a body.
    async fn post(&self, path: &str) -> (StatusCode, String) {
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, String) {
        let response = router(Arc::clone(&self.state))
            .oneshot(request)
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
}

#[tokio::test]
async fn the_index_renders_with_no_messages_at_all() {
    // §8.4's empty case, which is also what a configured but idle queue
    // looks like every morning.
    let app = App::new();

    let (status, body) = app.get("/").await;

    assert_eq!(status, StatusCode::OK);
    for queue in ["alpha", "beta", "broken"] {
        assert!(body.contains(queue), "{queue} is missing from the page");
    }
    assert!(body.contains("No messages."), "{body}");

    // §1.2, on the page rather than only in the specification.
    assert!(
        body.contains("must\nnot be pointed at an environment holding real patient data")
            || body.contains("real patient data"),
        "the redaction warning is missing"
    );
}

#[tokio::test]
async fn the_index_shows_the_messages_on_a_queue() {
    // §4.1 and §4.2.
    let app = App::new();
    app.reader().push("alpha", 7, ER7);
    app.reader().push("alpha", 8, "MSH|^~\\&|253");

    let (status, body) = app.get("/").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("2 active"), "the count is missing: {body}");
    // §4.2 — the sequence number, and a link to the detail view.
    assert!(body.contains("/messages/alpha/7"), "{body}");
    assert!(body.contains("/messages/alpha/8"), "{body}");
    // §4.2's preview.
    assert!(body.contains("MSH|^~"), "the preview is missing");
}

#[tokio::test]
async fn one_unreachable_queue_does_not_fail_the_page() {
    // §6.2 and §8.4 — THE test of this file, and the one a real emulator
    // cannot arrange. A page that failed here would leave a developer with
    // no way to see the queues that *are* working.
    let app = App::new();
    app.reader().push("alpha", 7, ER7);
    app.reader().fail("broken", "the broker refused the link");

    let (status, body) = app.get("/").await;

    assert_eq!(status, StatusCode::OK, "the page still renders");
    assert!(body.contains("the broker refused the link"), "{body}");
    assert!(
        body.contains("/messages/alpha/7"),
        "and the working queue is still shown"
    );
}

#[tokio::test]
async fn a_quota_error_carries_the_hint() {
    // §6.1 — "the difference between a five-minute and a fifty-minute
    // debugging session". Both substrings the Python detects.
    for reason in [
        "amqp:connectionsquotaexceeded: too many connections",
        "The service returned resource-limit-exceeded",
    ] {
        let app = App::new();
        app.reader().fail("broken", reason);

        let (status, body) = app.get("/").await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("emulator has run out of connections"),
            "no hint for {reason:?}: {body}"
        );
    }
}

#[tokio::test]
async fn an_ordinary_error_carries_no_hint() {
    // The negative case of §8.2, through the page rather than the function:
    // a hint on every error is a hint on none.
    let app = App::new();
    app.reader().fail("broken", "connection refused");

    let (_, body) = app.get("/").await;

    assert!(body.contains("connection refused"));
    assert!(
        !body.contains("emulator has run out of connections"),
        "{body}"
    );
}

#[tokio::test]
async fn the_detail_view_renders_an_er7_body_segment_by_segment() {
    // §5.3 and §5.4.
    let app = App::new();
    app.reader()
        .push_with("alpha", 7, ER7, &[("CorrelationId", "3f6d")]);

    let (status, body) = app.get("/messages/alpha/7").await;

    assert_eq!(status, StatusCode::OK);
    // §5.4 — one row per segment.
    for segment in ["MSH", "EVN", "PID"] {
        assert!(body.contains(segment), "{segment} is missing");
    }
    assert!(body.contains("HL7 v2"), "the rendering is not named");
    // §5.3 — the application properties and the broker's, separately.
    assert!(body.contains("CorrelationId"), "{body}");
    assert!(body.contains("3f6d"));
    assert!(body.contains("SequenceNumber"));
}

#[tokio::test]
async fn the_detail_view_numbers_msh_fields_from_two() {
    // `MSH-1` IS the field separator, so splitting on `|` yields MSH-2
    // first. Numbering from one would label the message type as MSH-8, on
    // a page whose entire job is letting somebody read field numbers.
    //
    // Checked through the rendered HTML rather than through `Segment`,
    // because the template does the numbering and a unit test on the type
    // would not have caught the template using `loop.index`.
    let app = App::new();
    app.reader().push("alpha", 7, ER7);

    let (_, body) = app.get("/messages/alpha/7").await;

    // The message type is MSH-9 and the control id MSH-10.
    let msh_nine = r#"<span class="index">9</span>
        <span class="value">ADT^A31^ADT_A05</span>"#;
    assert!(
        body.contains(msh_nine),
        "MSH-9 is not labelled 9. Rendered:\n{}",
        section_of(&body, "ADT^A31")
    );

    // And PID-3 is the third part of PID, which starts at one.
    let pid_three = r#"<span class="index">3</span>
        <span class="value">8888888^^^252^PI</span>"#;
    assert!(
        body.contains(pid_three),
        "PID-3 is not labelled 3. Rendered:\n{}",
        section_of(&body, "8888888")
    );
}

/// The lines around `needle`, for a failure message worth reading.
fn section_of(body: &str, needle: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or(0);

    lines[at.saturating_sub(4)..(at + 2).min(lines.len())].join("\n")
}

#[tokio::test]
async fn the_detail_view_pretty_prints_a_store_document() {
    // §5.4 and `spec/03-message-bus.md` §6.
    let app = App::new();
    let document = serde_json::json!({
        "MessageReceivedAt": "2026-08-21T09:15:00+00:00",
        "CorrelationId": "3f6d",
        "SourceSystem": "252",
        "RawPayload": ER7,
    })
    .to_string();
    app.reader().push("alpha", 9, &document);

    let (status, body) = app.get("/messages/alpha/9").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("message store event"), "{body}");
    // Indented, which is the whole point of the rendering. Askama escapes
    // with numeric entities, so a quote arrives as `&#34;`.
    assert!(
        body.contains("\n  &#34;CorrelationId&#34;: &#34;3f6d&#34;"),
        "{body}"
    );
    // And its payload rendered as ER7 in its own right.
    assert!(body.contains("RawPayload"));
    assert!(body.contains("PID"), "the payload was not rendered");
}

#[tokio::test]
async fn the_detail_view_shows_an_unrecognised_body_raw() {
    // §5.4 — "Neither rendering may fail the page".
    let app = App::new();
    app.reader()
        .push("alpha", 11, "this is not a message at all");

    let (status, body) = app.get("/messages/alpha/11").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("this is not a message at all"), "{body}");
    assert!(body.contains(">raw<"), "the rendering is not named: {body}");
}

#[tokio::test]
async fn a_body_is_escaped_rather_than_injected() {
    // Not a clause, an obligation. BusWatch shows attacker-influenced
    // content -- an HL7 field arrives from an upstream clinical system --
    // on a page a developer is logged into.
    let app = App::new();
    app.reader()
        .push("alpha", 13, "<script>alert('x')</script>");

    let (_, body) = app.get("/messages/alpha/13").await;

    assert!(
        !body.contains("<script>alert"),
        "the script survived: {body}"
    );
    assert!(
        body.contains("&#60;script&#62;alert(&#39;x&#39;)"),
        "{body}"
    );
}

#[tokio::test]
async fn a_missing_message_is_a_404_that_names_the_limit() {
    // §5.2 — "so the reader knows to raise it rather than concluding the
    // message is gone". A bare 404 is the wrong answer to a bounded scan.
    let app = App::new();
    app.reader().push("alpha", 7, ER7);

    let (status, body) = app.get("/messages/alpha/999").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("BUSWATCH_DETAIL_SEARCH_LIMIT"), "{body}");
    assert!(body.contains("250"), "the limit's value is missing: {body}");
    assert!(body.contains("999"), "the sequence number is missing");
}

#[tokio::test]
async fn a_message_past_the_search_limit_is_the_same_404() {
    // §5.1 and §5.2 — the case the limit exists for, rather than a message
    // that was never there.
    let config = Config::from_pairs([
        ("BUSWATCH_QUEUE_NAMES", "alpha"),
        ("BUSWATCH_DETAIL_SEARCH_LIMIT", "3"),
    ])
    .unwrap();
    let app = App {
        state: Arc::new(AppState::new(FakeReader::new(), config)),
    };

    for sequence in 1..=10 {
        app.reader().push("alpha", sequence, ER7);
    }

    // Within the limit.
    assert_eq!(app.get("/messages/alpha/2").await.0, StatusCode::OK);

    // Past it, though the message is certainly there.
    let (status, body) = app.get("/messages/alpha/9").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("first 3 messages"), "{body}");
}

#[tokio::test]
async fn a_message_on_an_unreachable_queue_is_a_404_rather_than_a_crash() {
    // §6.2, on the detail route as well as the index.
    let app = App::new();
    app.reader().fail("broken", "the broker refused the link");

    let (status, _) = app.get("/messages/broken/7").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn clearing_a_queue_redirects_and_leaves_a_flash() {
    // §3 and §3.4.
    let app = App::new();
    app.reader().push("alpha", 7, ER7);
    app.reader().push("alpha", 8, ER7);

    let (status, _) = app.post("/queues/alpha/clear").await;

    // §3 — a redirect, so a reload does not clear the queue again.
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(app.reader().depth("alpha"), 0, "the queue was cleared");

    // §3.4 — consumed by the next render.
    let (_, body) = app.get("/").await;
    assert!(body.contains("Cleared 2 messages from alpha."), "{body}");

    // And then discarded: a flash that survived would say a queue had just
    // been cleared every time the page was reloaded.
    let (_, again) = app.get("/").await;
    assert!(!again.contains("Cleared 2 messages"), "{again}");
}

#[tokio::test]
async fn a_failed_clear_says_so_rather_than_claiming_success() {
    // §3.4 and §6.1 — including the hint, because a clear is exactly what a
    // developer does after the emulator has run out of connections.
    let app = App::new();
    app.reader().fail("broken", "amqp:connectionsquotaexceeded");

    let (status, _) = app.post("/queues/broken/clear").await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (_, body) = app.get("/").await;
    assert!(body.contains("Could not clear broken"), "{body}");
    assert!(
        body.contains("emulator has run out of connections"),
        "{body}"
    );
}

#[tokio::test]
async fn a_render_serves_the_cache_until_a_refresh_is_asked_for() {
    // §3.2 — "A page render serves the cache for every queue, refreshing
    // only those missing from it plus any explicitly requested." This is
    // what keeps the page responsive against a slow emulator.
    let app = App::new();
    app.reader().push("alpha", 7, ER7);

    // First render: nothing cached, so everything is read.
    let (_, first) = app.get("/").await;
    assert!(first.contains("/messages/alpha/7"));

    // The queue changes underneath.
    app.reader().push("alpha", 8, ER7);

    // A plain render serves what it already has.
    let (_, cached) = app.get("/").await;
    assert!(
        !cached.contains("/messages/alpha/8"),
        "the cache was bypassed: {cached}"
    );

    // §3.1 — until one queue is asked for by name.
    let (_, refreshed) = app.get("/?refresh_queue=alpha").await;
    assert!(refreshed.contains("/messages/alpha/8"), "{refreshed}");
}

#[tokio::test]
async fn refresh_all_refreshes_every_queue() {
    // §3.1.
    let app = App::new();
    app.get("/").await;

    app.reader().push("alpha", 1, ER7);
    app.reader().push("beta", 2, ER7);

    let (_, cached) = app.get("/").await;
    assert!(!cached.contains("/messages/alpha/1"));

    let (_, refreshed) = app.get("/?refresh_all=true").await;
    assert!(refreshed.contains("/messages/alpha/1"), "{refreshed}");
    assert!(refreshed.contains("/messages/beta/2"), "{refreshed}");
}

#[tokio::test]
async fn refresh_all_is_only_true() {
    // A `refresh_all=false` in a bookmarked URL should not refresh
    // everything, which is what any "the parameter is present" test would
    // have made it do.
    let app = App::new();
    app.get("/").await;
    app.reader().push("alpha", 1, ER7);

    let (_, body) = app.get("/?refresh_all=false").await;

    assert!(!body.contains("/messages/alpha/1"), "{body}");
}

#[tokio::test]
async fn a_clear_drops_the_snapshot_so_the_next_render_is_honest() {
    // Not a clause, a consequence of §3.2 and §3: without it, the page
    // after a clear shows the messages that were just deleted, and the
    // flash says they are gone. One of the two is wrong.
    let app = App::new();
    app.reader().push("alpha", 7, ER7);
    app.get("/").await;

    app.post("/queues/alpha/clear").await;

    let (_, body) = app.get("/").await;
    assert!(!body.contains("/messages/alpha/7"), "{body}");
    assert!(body.contains("No messages."), "{body}");
}

#[tokio::test]
async fn the_static_assets_are_served() {
    // §7.1 — bundled, so BusWatch works with no network and no volume
    // mount. §7.2's brand is in the stylesheet.
    let app = App::new();

    let (status, css) = app.get("/static/buswatch.css").await;
    assert_eq!(status, StatusCode::OK);
    assert!(css.contains("#325083"), "NHS Wales Blue is missing");
    assert!(css.contains("Rubik"), "the typeface is missing");

    let (status, _) = app.get("/static/DHCW-logo-RGB-White.png").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_unknown_path_is_a_404() {
    let app = App::new();

    assert_eq!(app.get("/nope").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        app.get("/static/../Cargo.toml").await.0,
        StatusCode::NOT_FOUND,
        "there is no path parameter under /static to traverse"
    );
}

#[tokio::test]
async fn a_queue_that_is_not_configured_is_not_shown() {
    // §2 — the configured names are the page. A queue the fake knows about
    // but the configuration does not is invisible, which is what stops a
    // stale emulator config from putting queues on the page that the rest
    // of the estate has never heard of.
    let app = App::new();
    app.reader().push("gamma", 1, ER7);

    let (_, body) = app.get("/?refresh_all=true").await;

    assert!(!body.contains("gamma"), "{body}");
}

#[tokio::test]
async fn the_refresh_bar_and_theme_control_render_and_their_script_is_served() {
    // spec/14-buswatch.md §7.5, §8.1.2.
    let app = App::new();

    let (status, body) = app.get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"id="refresh-bar""#), "{body}");
    assert!(body.contains("Last updated"));
    assert!(body.contains(r#"id="theme-toggle""#));
    assert!(body.contains("/static/buswatch.js"));
    // Without JavaScript the bar still has a way to refresh.
    assert!(body.contains("/?refresh_all=true"));

    let response = router(Arc::clone(&app.state))
        .oneshot(
            Request::builder()
                .uri("/static/buswatch.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
}

#[tokio::test]
async fn the_stylesheet_has_a_dark_theme_that_the_control_can_override() {
    // §7.5.
    let css = buswatch::web::STYLESHEET_FOR_TESTS;

    assert!(css.contains("prefers-color-scheme: dark"));
    assert!(css.contains(r#":root[data-theme="dark"]"#));
    assert!(
        css.contains(r#":root:not([data-theme="light"])"#),
        "light must win over the system"
    );
}

#[tokio::test]
async fn a_subscription_is_listed_beside_the_queues_and_its_name_survives_the_url() {
    // §2.2.1 -- `topic:subscription` is a path segment, so the colon has to
    // round-trip through the links the page builds.
    let config = Config::from_pairs([("BUSWATCH_QUEUE_NAMES", "q,topic:sub")]).unwrap();
    let app = App {
        state: Arc::new(AppState::new(FakeReader::new(), config)),
    };
    app.reader().push("topic:sub", 7, "hello");

    let (_, index) = app.get("/").await;
    assert!(index.contains("topic:sub"), "{index}");
    assert!(index.contains("/messages/topic%3Asub/7"), "{index}");

    let (status, detail) = app.get("/messages/topic%3Asub/7").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(detail.contains("hello"));
}
