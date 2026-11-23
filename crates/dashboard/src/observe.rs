//! Where the Azure data comes in.
//!
//! Implements `spec/15-dashboard.md` §4.2 and §2.2.2.
//!
//! # The seam §1.2 needs
//!
//! "The dashboard MUST start and serve every page with no Azure
//! configuration at all." That is not a special case handled at the edges;
//! it is the **default implementation**. [`Unconfigured`] answers every
//! question with "I do not know", and every page renders from it.
//!
//! So the empty path is the one exercised by every test in this crate, and
//! a live implementation is the variant. The Python has this the other way
//! round — `DEMO_MODE` and a scattering of `if not config.X: return []`
//! guards — which is why "works with no Azure" is something its test suite
//! has to keep re-proving.
//!
//! # Unknown, not empty
//!
//! §2.2.2: "returning empty state with a flag the browser can render as
//! 'unavailable' rather than 'zero'". Every method here returns something
//! that can say *I could not tell you*, distinctly from *there is nothing*.
//! A queue with no messages and a queue that could not be read are the same
//! JSON in the Python and different JSON here.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::health::{ContainerState, QueueDepths};

/// One exception from Log Analytics (§4.3.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Exception {
    /// When it happened, RFC 3339.
    pub timestamp: String,
    /// The exception type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Its message.
    #[serde(rename = "outerMessage")]
    pub outer_message: String,
    /// Application Insights' severity level.
    #[serde(rename = "severityLevel")]
    pub severity_level: Option<i64>,
    /// Which service raised it.
    #[serde(rename = "appName")]
    pub app_name: String,
    /// The operation id, which is the link to a trace.
    #[serde(rename = "operation_Id")]
    pub operation_id: String,
}

/// One container's state (§3.2's container row).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Container {
    /// The Container App's name.
    pub name: String,
    /// Whether it is running.
    #[serde(serialize_with = "container_state")]
    pub state: ContainerState,
    /// Replicas now, when known.
    pub replicas: Option<u32>,
    /// Restarts in the observation window, when known.
    pub restarts: Option<u32>,
}

/// Serialise a [`ContainerState`] as the string the browser expects.
// serde calls `serialize_with` functions with a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn container_state<S: serde::Serializer>(
    state: &ContainerState,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(match state {
        ContainerState::Running => "running",
        ContainerState::NotRunning => "not_running",
        ContainerState::Unknown => "unknown",
    })
}

/// One point of a chart series (§2.3.2).
///
/// The field names are the browser's: `sb-chart.js` and
/// `hl7-throughput-chart.js` both read `pt.time` and `pt.value`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeriesPoint {
    /// The bin's start, RFC 3339.
    pub time: String,
    /// The summed value.
    pub value: f64,
}

/// Two named series over a window (§2.3.2).
///
/// **Not** a flat list of points. `sb-chart.js` and
/// `hl7-throughput-chart.js` read `data.incoming` and `data.outgoing`; a
/// `points` array renders as an empty chart with nothing in the console.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Series {
    /// Messages received.
    pub incoming: Vec<SeriesPoint>,
    /// Messages sent.
    pub outgoing: Vec<SeriesPoint>,
    /// The window, as the page labels it: `24h`.
    pub timespan: String,
}

/// One "Integration Hub Event" trace (§4.3.1's "Messages today").
///
/// # This carries patient data
///
/// `payload` is the message content, which is exactly what
/// `spec/04-observability.md` §2 exists to keep out of a log. §1.3 requires
/// it to be redacted **before display**, and [`MessageEvent::rendered`] is
/// the only way this type should ever reach a page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MessageEvent {
    /// When, RFC 3339.
    pub timestamp: String,
    /// The event type, e.g. `MESSAGE_RECEIVED`.
    pub event_type: String,
    /// Which flow.
    pub workflow_id: String,
    /// Which service.
    pub microservice_id: String,
    /// The correlation id.
    pub correlation_id: String,
    /// The operation id, which links to a trace.
    pub operation_id: String,
    /// The message content, **unredacted**.
    ///
    /// Not serialised: an `/api/*` response carrying raw patient data would
    /// put it somewhere §1.3 cannot reach. Pages call
    /// [`MessageEvent::rendered`].
    #[serde(skip)]
    pub payload: Option<String>,
}

impl MessageEvent {
    /// The payload as it may be shown (§1.3).
    ///
    /// `redact` comes from `DASHBOARD_REDACT_MESSAGES`, which defaults on.
    /// The Python does no redaction at all on these pages; this is the
    /// deliberate correction §1.3 describes.
    ///
    /// # Examples
    ///
    /// ```
    /// use dashboard::observe::MessageEvent;
    ///
    /// let event = MessageEvent {
    ///     timestamp: String::new(),
    ///     event_type: "MESSAGE_RECEIVED".to_string(),
    ///     workflow_id: "phw-to-mpi".to_string(),
    ///     microservice_id: String::new(),
    ///     correlation_id: String::new(),
    ///     operation_id: String::new(),
    ///     payload: Some("MSH|^~\\&|252\rPID|||8888888".to_string()),
    /// };
    ///
    /// // Redacted: the NHS number does not survive.
    /// let shown = event.rendered(true).unwrap();
    /// assert!(!shown.contains("8888888"), "{shown}");
    /// assert!(shown.contains("MSH|^~\\&|252"), "the header survives: {shown}");
    ///
    /// // And the deployment may turn it off deliberately.
    /// assert!(event.rendered(false).unwrap().contains("8888888"));
    /// ```
    #[must_use]
    pub fn rendered(&self, redact: bool) -> Option<String> {
        let payload = self.payload.as_deref()?;
        Some(if redact {
            ihb_telemetry::redact::redact_hl7(payload)
        } else {
            payload.to_string()
        })
    }
}

/// A retry delay reported by a sender (`spec/10-senders.md`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RetryDelay {
    /// Which flow.
    pub workflow_id: String,
    /// The delay, when it was reported.
    pub delay_seconds: Option<f64>,
    /// Which attempt.
    pub attempt: Option<String>,
    /// Which queue.
    pub queue: String,
    /// Which service.
    pub microservice_id: String,
    /// When, RFC 3339.
    pub timestamp: String,
}

/// Everything the dashboard can ask Azure.
///
/// Not dyn-compatible — these are `async fn`s in a trait — so the
/// application is generic over it. That costs nothing: the implementation
/// is chosen once at startup.
///
/// **No method returns a `Result`.** §4.2 makes every failure an empty
/// answer, and a `Result` nobody may propagate is an invitation to
/// propagate it. A failure is expressed in the data — a `None` depth, an
/// empty list — where the page can render it as "unavailable".
pub trait Observatory: Send + Sync + 'static {
    /// Depths for every queue named, by queue name.
    ///
    /// A queue missing from the map, or present with `None` depths, could
    /// not be read (§3.3).
    fn queue_depths(
        &self,
        queues: &[String],
    ) -> impl Future<Output = BTreeMap<String, QueueDepths>> + Send;

    /// Every topic in the namespace with its subscriptions
    /// (`spec/15-dashboard.md` §10.5.1).
    ///
    /// `None` means the namespace could not be read — unknown. `Some` of an
    /// empty list means it was read and has no topics. The default is
    /// `None`, which is right for an observatory that cannot ask.
    fn topics(&self) -> impl Future<Output = Option<Vec<crate::topics::Topic>>> + Send {
        async { None }
    }

    /// Every Container App the dashboard knows about.
    fn containers(&self) -> impl Future<Output = Vec<Container>> + Send;

    /// Exceptions from the last `hours` hours, newest first, at most 200.
    fn exceptions(&self, hours: u32) -> impl Future<Output = Vec<Exception>> + Send;

    /// The throughput series over `hours`, binned (§2.3.2).
    fn throughput(
        &self,
        hours: u32,
        health_board: Option<&str>,
        workflow_id: Option<&str>,
    ) -> impl Future<Output = Series> + Send;

    /// The most recent retry delay per flow, over `hours`.
    fn retry_delays(&self, hours: u32) -> impl Future<Output = Vec<RetryDelay>> + Send;

    /// Message events from today, newest first, at most 500 (§4.3.1).
    ///
    /// `workflow_id` narrows to one flow.
    fn messages(&self, workflow_id: Option<&str>)
    -> impl Future<Output = Vec<MessageEvent>> + Send;

    /// Every event sharing one operation id — a distributed trace (§2.1).
    fn trace(&self, operation_id: &str) -> impl Future<Output = Vec<MessageEvent>> + Send;

    /// The most recent `MESSAGE_RECEIVED` per workflow (§4.3.1, Alarm 1).
    ///
    /// `None` means the query did not run. `Some` with a workflow absent
    /// means it ran and found nothing in the window — which §5.3.3 also
    /// treats as unknown, but the two are kept distinguishable because
    /// Alarm 3 needs the difference and one shape is easier to reason
    /// about than two.
    fn last_received(
        &self,
        workflow_ids: &[String],
    ) -> impl Future<Output = Option<BTreeMap<String, OffsetDateTime>>> + Send;

    /// The most recent `messages_sent` metric per workflow (§4.3.1, Alarm 2).
    fn last_sent(
        &self,
        workflow_ids: &[String],
    ) -> impl Future<Output = Option<BTreeMap<String, OffsetDateTime>>> + Send;

    /// `MESSAGE_FAILED` counts per workflow, within `window_minutes`
    /// (§4.3.1, Alarm 3).
    ///
    /// `None` means the query did not run — **unknown**. `Some` with a
    /// workflow absent means it ran and that workflow had **no failures**,
    /// which is zero and healthy. `summarize count()` emits no row at all
    /// for a workflow with nothing to count, so collapsing these two would
    /// turn every quiet flow into an unknown one, or every broken query
    /// into a clean bill of health.
    fn failure_counts(
        &self,
        workflow_ids: &[String],
        window_minutes: u32,
    ) -> impl Future<Output = Option<BTreeMap<String, u64>>> + Send;

    /// Whether this observatory can reach Azure at all.
    ///
    /// Drives §2.2.2's "unavailable" flag. It is not derived from an empty
    /// result, because a correctly-configured estate with nothing wrong
    /// also returns empty lists — and those two must look different.
    fn is_configured(&self) -> bool;
}

/// An [`Observatory`] that knows nothing.
///
/// The implementation §1.2 is about: no Azure configuration, every page
/// renders, everything reads as unavailable rather than as zero.
///
/// # Examples
///
/// ```
/// # tokio_test::block_on(async {
/// use dashboard::observe::{Observatory, Unconfigured};
///
/// let azure = Unconfigured;
///
/// assert!(!azure.is_configured());
/// assert!(azure.exceptions(24).await.is_empty());
///
/// // And a queue reads as unknown rather than as empty.
/// let depths = azure.queue_depths(&["inbound".to_string()]).await;
/// assert_eq!(depths["inbound"].active, None, "unknown, not zero");
/// # })
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct Unconfigured;

// The trait is async because the real implementation awaits the network; this one has
// nothing to wait for.
#[allow(clippy::unused_async_trait_impl)]
impl Observatory for Unconfigured {
    async fn queue_depths(&self, queues: &[String]) -> BTreeMap<String, QueueDepths> {
        // Present, with nothing known. An *absent* entry would let a caller
        // that used `unwrap_or_default()` read the queue as empty, which is
        // exactly the confusion §2.2.2 forbids.
        queues
            .iter()
            .map(|queue| (queue.clone(), QueueDepths::unknown()))
            .collect()
    }

    async fn containers(&self) -> Vec<Container> {
        Vec::new()
    }

    async fn exceptions(&self, _hours: u32) -> Vec<Exception> {
        Vec::new()
    }

    async fn throughput(
        &self,
        hours: u32,
        _health_board: Option<&str>,
        _workflow_id: Option<&str>,
    ) -> Series {
        // Both series present and empty, with the window labelled — the
        // chart draws its own empty state from that rather than from a
        // missing key.
        Series {
            timespan: format!("{hours}h"),
            ..Series::default()
        }
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
    ) -> Option<BTreeMap<String, OffsetDateTime>> {
        // The query did not run, so nothing is known. Not an empty map,
        // which would mean "it ran and found nothing".
        None
    }

    async fn last_sent(
        &self,
        _workflow_ids: &[String],
    ) -> Option<BTreeMap<String, OffsetDateTime>> {
        None
    }

    async fn failure_counts(
        &self,
        _workflow_ids: &[String],
        _window_minutes: u32,
    ) -> Option<BTreeMap<String, u64>> {
        // §4.5 — and this is the one where it matters most. `Some(empty)`
        // would report every rule healthy on the strength of a query that
        // never ran.
        None
    }

    fn is_configured(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unconfigured_observatory_answers_every_question() {
        // spec/15-dashboard.md §1.2 and §8.1 — the behaviour every page
        // depends on. Nothing here may panic, block or error.
        let azure = Unconfigured;

        assert!(azure.exceptions(24).await.is_empty());
        assert!(azure.containers().await.is_empty());
        assert!(azure.throughput(1, None, None).await.incoming.is_empty());
        assert!(azure.retry_delays(1).await.is_empty());
        assert!(!azure.is_configured());
    }

    #[tokio::test]
    async fn an_unreadable_queue_is_present_and_unknown_rather_than_absent() {
        // §2.2.2 — THE test of this file.
        //
        // Absent would be worse than useless: a caller writing
        // `depths.get(q).copied().unwrap_or_default()` would read a missing
        // entry as `QueueDepths { active: Some(0) .. }`-shaped zero. Present
        // with `None` makes the "I cannot see" case something a caller has
        // to handle rather than something it can default away.
        let queues = vec!["inbound".to_string(), "outbound".to_string()];
        let depths = Unconfigured.queue_depths(&queues).await;

        assert_eq!(depths.len(), 2);
        for queue in &queues {
            let found = depths.get(queue).expect("every queue asked for is present");
            assert_eq!(found.active, None);
            assert_eq!(found.dead_letter, None);
            assert!(!found.is_known());
        }
    }

    #[tokio::test]
    async fn asking_for_nothing_returns_nothing() {
        assert!(Unconfigured.queue_depths(&[]).await.is_empty());
    }

    #[test]
    fn a_container_state_serialises_as_the_browser_expects() {
        // §2.2 — the JSON shapes are a contract with `dashboard.js`.
        let render = |state| {
            serde_json::to_string(&Container {
                name: "x".to_string(),
                state,
                replicas: None,
                restarts: None,
            })
            .expect("a container serialises")
        };

        assert!(render(ContainerState::Running).contains(r#""state":"running""#));
        assert!(render(ContainerState::NotRunning).contains(r#""state":"not_running""#));
        assert!(render(ContainerState::Unknown).contains(r#""state":"unknown""#));
    }

    #[test]
    fn an_exception_keeps_the_pythons_field_names() {
        // §8.5 — `dashboard.js` reads these keys, so renaming one to
        // something more Rust-shaped would break the page silently.
        let json = serde_json::to_string(&Exception {
            timestamp: "2026-08-21T09:15:00Z".to_string(),
            kind: "ValueError".to_string(),
            outer_message: "boom".to_string(),
            severity_level: Some(3),
            app_name: "phw-hl7-server".to_string(),
            operation_id: "abc123".to_string(),
        })
        .unwrap();

        for key in [
            "timestamp",
            "type",
            "outerMessage",
            "severityLevel",
            "appName",
            "operation_Id",
        ] {
            assert!(
                json.contains(&format!("\"{key}\"")),
                "{key} is missing: {json}"
            );
        }
    }

    /// An event carrying a real NHS number.
    fn event_with(payload: &str) -> MessageEvent {
        MessageEvent {
            timestamp: "2026-08-22T09:15:00Z".to_string(),
            event_type: "MESSAGE_RECEIVED".to_string(),
            workflow_id: "phw-to-mpi".to_string(),
            microservice_id: "phw_hl7_server".to_string(),
            correlation_id: "3f6d".to_string(),
            operation_id: "op-1".to_string(),
            payload: Some(payload.to_string()),
        }
    }

    #[test]
    fn a_payload_is_redacted_before_it_can_be_shown() {
        // spec/15-dashboard.md §1.3 and §8.6, and the patient-safety
        // requirement of spec/04-observability.md §2. The Python renders
        // message content on these pages with no redaction at all; this is
        // the deliberate correction.
        let event = event_with(
            "MSH|^~\\&|252|252|100|100|20260822||ADT^A31|M1|P|2.5\rPID|||8888888^^^252^PI||SURNAME^FORENAME",
        );

        let shown = event.rendered(true).expect("there is a payload");

        assert!(
            !shown.contains("8888888"),
            "an NHS number survived: {shown}"
        );
        assert!(!shown.contains("SURNAME"), "a name survived: {shown}");
        // §2.6's allow-list — the header still identifies the message.
        assert!(
            shown.contains("ADT^A31"),
            "the message type is kept: {shown}"
        );
        assert!(shown.contains("M1"), "the control id is kept: {shown}");
    }

    #[test]
    fn an_unrecognisable_payload_is_masked_whole() {
        // spec/04-observability.md §2.4 — JSON, XML or a fragment is masked
        // entirely, because its content is unknown. A dashboard is exactly
        // where a half-parsed body ends up being looked at.
        for payload in [
            r#"{"nhsNumber": "4444444444"}"#,
            "<Patient><name>Jones</name></Patient>",
            "8888888",
        ] {
            let shown = event_with(payload).rendered(true).unwrap();
            assert_eq!(shown, "***", "{payload} was not masked");
        }
    }

    #[test]
    fn redaction_can_be_turned_off_deliberately() {
        // §1.3 — "unless the deployment explicitly disables redaction".
        let event = event_with("MSH|^~\\&|252\rPID|||8888888");

        assert!(event.rendered(false).unwrap().contains("8888888"));
    }

    #[test]
    fn an_event_never_serialises_its_payload() {
        // §1.3 again, one layer down. An `/api/*` route that returned a
        // `MessageEvent` would put unredacted patient data somewhere the
        // page's redaction cannot reach — so the field does not serialise
        // at all, and a future route cannot leak it by forgetting.
        let json = serde_json::to_string(&event_with("MSH|^~\\&|252\rPID|||8888888")).unwrap();

        assert!(!json.contains("8888888"), "{json}");
        assert!(!json.contains("payload"), "{json}");
        assert!(
            json.contains("MESSAGE_RECEIVED"),
            "the metadata is kept: {json}"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_observatory_says_the_alarm_queries_did_not_run() {
        // §4.5 and §5.1.2 step 3 — THE distinction for Alarm 3. `None`
        // means "the query did not run" and is unknown; `Some(empty)` would
        // mean "it ran and nothing failed", which is a clean bill of health
        // nobody checked.
        let azure = Unconfigured;
        let ids = vec!["phw-to-mpi".to_string()];

        assert!(azure.last_received(&ids).await.is_none());
        assert!(azure.last_sent(&ids).await.is_none());
        assert!(
            azure.failure_counts(&ids, 15).await.is_none(),
            "an unrun query must not read as zero failures"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_observatory_has_no_messages_or_traces() {
        // §1.2 — the two pages that show patient data must render with no
        // Azure just like every other.
        assert!(Unconfigured.messages(None).await.is_empty());
        assert!(Unconfigured.trace("op-1").await.is_empty());
    }

    #[test]
    fn a_missing_severity_is_null_rather_than_zero() {
        // Zero is a real severity level in Application Insights, so an
        // absent one must not become one.
        let json = serde_json::to_string(&Exception {
            timestamp: String::new(),
            kind: String::new(),
            outer_message: String::new(),
            severity_level: None,
            app_name: String::new(),
            operation_id: String::new(),
        })
        .unwrap();

        assert!(json.contains(r#""severityLevel":null"#), "{json}");
    }
}
