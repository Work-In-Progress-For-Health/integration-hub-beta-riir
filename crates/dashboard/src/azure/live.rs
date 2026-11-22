//! The [`Observatory`] that talks to Azure.
//!
//! Implements `spec/15-dashboard.md` §4.2 and §4.5.
//!
//! # This is where a failure becomes an empty answer
//!
//! §4.2: "Every integration MUST degrade to empty rather than raising."
//! The modules underneath return `Result`, and this one is the single place
//! that turns an error into a `Vec::new()` — **and a log line**.
//!
//! Both halves matter. Without the empty answer the page fails; without the
//! log line nobody can find out why a panel is blank, because §4.2 has
//! deliberately hidden the reason from the page.
//!
//! # An unconfigured integration is not an error
//!
//! A dashboard with a Log Analytics workspace and no subscription shows
//! exceptions and no queue depths, and neither is a fault. So each method
//! checks what it needs and returns empty **without** logging when the
//! answer is simply "not configured" — a warning per panel per refresh
//! would bury the real ones.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::azure::arm::{self, Scope};
use crate::azure::http::HttpsClient;
use crate::azure::logs;
use crate::azure::token::{ARM_SCOPE, LOGS_SCOPE, TokenSource};
use crate::config::Config;
use crate::health::QueueDepths;
use crate::kql;
use crate::observe::{
    Container, Exception, MessageEvent, Observatory, RetryDelay, Series, SeriesPoint,
};

/// An [`Observatory`] backed by the Azure REST APIs.
#[derive(Debug)]
pub struct LiveObservatory {
    /// Where tokens come from.
    tokens: TokenSource,
    /// The shared HTTPS client.
    client: HttpsClient,
    /// `AZURE_SUBSCRIPTION_ID` and `AZURE_RESOURCE_GROUP`, when both are set.
    scope: Option<Scope>,
    /// `AZURE_SERVICE_BUS_NAMESPACE`.
    namespace: Option<String>,
    /// `AZURE_LOG_ANALYTICS_WORKSPACE_ID`.
    workspace_id: Option<String>,
    /// `AZURE_APP_INSIGHTS_RESOURCE_ID` (§4.3.3).
    resource_id: Option<String>,
}

impl LiveObservatory {
    /// Build one from the configuration.
    ///
    /// Absent Azure configuration is not a failure: §1.2 means an
    /// incompletely configured dashboard runs, with the panels it cannot
    /// serve reporting unavailable. That is about configuration being
    /// *missing*.
    ///
    /// # Errors
    ///
    /// [`ihb_tls::TrustError`] if a CA bundle is configured and unusable —
    /// configuration being *present and wrong*, which
    /// `spec/18-deployment.md` §4.3 makes fatal. The two cases are
    /// deliberately not treated alike.
    pub fn new(config: &Config) -> Result<Self, ihb_tls::TrustError> {
        let client = HttpsClient::new()?;

        let scope = match (
            config.azure.subscription_id.clone(),
            config.azure.resource_group.clone(),
        ) {
            (Some(subscription_id), Some(resource_group)) => Some(Scope {
                subscription_id,
                resource_group,
            }),
            _ => None,
        };

        Ok(Self {
            tokens: TokenSource::new(config.auth.clone(), client.clone()),
            client,
            scope,
            namespace: config.azure.service_bus_namespace.clone(),
            workspace_id: config.azure.log_analytics_workspace_id.clone(),
            resource_id: config.azure.app_insights_resource_id.clone(),
        })
    }

    /// Run a Log Analytics query, or return nothing.
    ///
    /// The single place a Log Analytics failure becomes §4.5's "no data".
    async fn rows(&self, what: &str, kql: &str) -> Vec<logs::Row> {
        // Not configured is not a failure, and not worth a log line every
        // refresh.
        let Some(workspace_id) = &self.workspace_id else {
            return Vec::new();
        };

        let token = match self.tokens.token(LOGS_SCOPE).await {
            Ok(token) => token,
            Err(error) => {
                // §4.6.4 — an empty result, and the reason logged, because
                // the page cannot show it.
                tracing::warn!(%error, query = what, "could not get a Log Analytics token");
                return Vec::new();
            }
        };

        match logs::query(&self.client, &token, workspace_id, kql).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, query = what, "the Log Analytics query failed");
                Vec::new()
            }
        }
    }

    /// Run a query, distinguishing "did not run" from "ran and found
    /// nothing".
    ///
    /// [`LiveObservatory::rows`] flattens both to an empty list, which is
    /// right for a panel and wrong for an alarm: §5.5.2 needs a query that
    /// returned no rows to mean **zero failures**, and a query that did not
    /// run to mean **unknown**.
    async fn rows_or_none(&self, what: &str, kql: &str) -> Option<Vec<logs::Row>> {
        let workspace_id = self.workspace_id.as_ref()?;

        let token = match self.tokens.token(LOGS_SCOPE).await {
            Ok(token) => token,
            Err(error) => {
                tracing::warn!(%error, query = what, "could not get a Log Analytics token");
                return None;
            }
        };

        match logs::query(&self.client, &token, workspace_id, kql).await {
            Ok(rows) => Some(rows),
            Err(error) => {
                tracing::warn!(%error, query = what, "the Log Analytics query failed");
                None
            }
        }
    }

    /// An ARM token, or nothing.
    async fn arm_token(&self, what: &str) -> Option<String> {
        match self.tokens.token(ARM_SCOPE).await {
            Ok(token) => Some(token),
            Err(error) => {
                tracing::warn!(%error, call = what, "could not get an ARM token");
                None
            }
        }
    }
}

impl Observatory for LiveObservatory {
    async fn queue_depths(&self, queues: &[String]) -> BTreeMap<String, QueueDepths> {
        // §3.3 — what a caller gets when nothing could be read: every queue
        // present, and every one unknown. Never absent, and never zero.
        let unknown = || -> BTreeMap<String, QueueDepths> {
            queues
                .iter()
                .map(|queue| (queue.clone(), QueueDepths::unknown()))
                .collect()
        };

        let (Some(scope), Some(namespace)) = (&self.scope, &self.namespace) else {
            return unknown();
        };
        let Some(token) = self.arm_token("queue depths").await else {
            return unknown();
        };

        match arm::queue_depths(&self.client, &token, scope, namespace).await {
            Ok(found) => {
                // The caller asked about specific queues. ARM answers for
                // every queue in the namespace, so this keeps what was
                // asked for — and a queue ARM did not mention stays
                // **unknown** rather than being dropped, which is what
                // makes §2.2.3's total absent rather than wrong.
                let mut out = unknown();
                for (name, depths) in found {
                    // A queue whose name contains the asked-for one, so a
                    // caller passing a workflow id gets that flow's queues.
                    for (asked, slot) in &mut out {
                        if name == *asked || name.contains(asked.as_str()) {
                            *slot = depths;
                        }
                    }
                    out.entry(name).or_insert(depths);
                }
                out
            }
            Err(error) => {
                tracing::warn!(%error, %namespace, "could not read queue depths");
                unknown()
            }
        }
    }

    async fn topics(&self) -> Option<Vec<crate::topics::Topic>> {
        let (Some(scope), Some(namespace)) = (&self.scope, &self.namespace) else {
            return None;
        };
        let token = self.arm_token("topics").await?;

        match arm::topics(&self.client, &token, scope, namespace).await {
            Ok(topics) => Some(topics),
            Err(error) => {
                tracing::warn!(%error, %namespace, "could not read the topics");
                None
            }
        }
    }

    async fn containers(&self) -> Vec<Container> {
        let Some(scope) = &self.scope else {
            return Vec::new();
        };
        let Some(token) = self.arm_token("container apps").await else {
            return Vec::new();
        };

        match arm::containers(&self.client, &token, scope).await {
            Ok(containers) => containers,
            Err(error) => {
                tracing::warn!(%error, "could not read the container apps");
                Vec::new()
            }
        }
    }

    async fn exceptions(&self, hours: u32) -> Vec<Exception> {
        let query = kql::exceptions(hours, self.resource_id.as_deref());

        self.rows("exceptions", &query)
            .await
            .iter()
            .map(|row| Exception {
                timestamp: logs::text(row, "timestamp").unwrap_or_default(),
                kind: logs::text(row, "type").unwrap_or_default(),
                outer_message: logs::text(row, "outerMessage").unwrap_or_default(),
                severity_level: logs::integer(row, "severityLevel"),
                app_name: logs::text(row, "appName").unwrap_or_default(),
                operation_id: logs::text(row, "operation_Id").unwrap_or_default(),
            })
            .collect()
    }

    async fn throughput(
        &self,
        hours: u32,
        health_board: Option<&str>,
        workflow_id: Option<&str>,
    ) -> Series {
        let bin = bin_for(hours);
        let query = kql::throughput(hours, bin, health_board, workflow_id);

        let mut series = Series {
            timespan: format!("{hours}h"),
            ..Series::default()
        };

        for row in self.rows("throughput", &query).await {
            let Some(time) = logs::text(&row, "TimeGenerated") else {
                continue;
            };
            let point = SeriesPoint {
                time,
                // A bin with no messages is a real zero, so an absent value
                // defaults here rather than dropping the point.
                value: logs::number(&row, "Value").unwrap_or_default(),
            };

            // §2.3.2 — the browser reads two named series, and the query
            // returns both interleaved under `Name`.
            match logs::text(&row, "Name").as_deref() {
                Some("messages_received") => series.incoming.push(point),
                Some("messages_sent") => series.outgoing.push(point),
                // A metric neither chart draws.
                _ => {}
            }
        }

        series
    }

    async fn retry_delays(&self, hours: u32) -> Vec<RetryDelay> {
        // §4.3.1's minimum, which the Python passes as 60 seconds.
        let query = format!(
            "{}\n    | where delay_seconds > 60",
            retry_query(hours, self.resource_id.as_deref())
        );

        self.rows("retry delays", &query)
            .await
            .iter()
            .filter_map(|row| {
                Some(RetryDelay {
                    workflow_id: logs::text(row, "workflow_id")?,
                    delay_seconds: logs::number(row, "delay_seconds"),
                    attempt: logs::text(row, "attempt"),
                    queue: logs::text(row, "queue").unwrap_or_default(),
                    microservice_id: logs::text(row, "microservice_id").unwrap_or_default(),
                    timestamp: logs::text(row, "timestamp").unwrap_or_default(),
                })
            })
            .collect()
    }

    async fn messages(&self, workflow_id: Option<&str>) -> Vec<MessageEvent> {
        let query = kql::messages_today(workflow_id, self.resource_id.as_deref());

        self.rows("messages", &query)
            .await
            .iter()
            .map(event_from)
            .collect()
    }

    async fn trace(&self, operation_id: &str) -> Vec<MessageEvent> {
        // §4.4 — the operation id reaches a query, so it is sanitised like
        // every other interpolated identifier.
        let safe = kql::identifier(operation_id);
        if safe.is_empty() {
            return Vec::new();
        }

        let query = format!(
            r#"
    AppTraces
    | where TimeGenerated > ago(7d){filter}
    | where OperationId == "{safe}"
    | project timestamp=TimeGenerated,
              customDimensions=Properties,
              appName=AppRoleName,
              operation_Id=OperationId
    | order by timestamp asc
    | take 500
    "#,
            filter = kql::resource_filter(self.resource_id.as_deref()),
        );

        self.rows("trace", &query)
            .await
            .iter()
            .map(event_from)
            .collect()
    }

    async fn last_received(
        &self,
        workflow_ids: &[String],
    ) -> Option<BTreeMap<String, OffsetDateTime>> {
        let query = kql::alarm1_last_received(workflow_ids, self.resource_id.as_deref())?;

        self.rows_or_none("alarm 1", &query)
            .await
            .map(|rows| timestamps(&rows, "last_message"))
    }

    async fn last_sent(&self, workflow_ids: &[String]) -> Option<BTreeMap<String, OffsetDateTime>> {
        let query = kql::alarm2_last_sent(
            workflow_ids,
            crate::alarm::rules::ALARM2_LOOKBACK_DAYS,
            self.resource_id.as_deref(),
        )?;

        self.rows_or_none("alarm 2", &query)
            .await
            .map(|rows| timestamps(&rows, "last_message"))
    }

    async fn failure_counts(
        &self,
        workflow_ids: &[String],
        window_minutes: u32,
    ) -> Option<BTreeMap<String, u64>> {
        let query =
            kql::alarm3_failures(workflow_ids, window_minutes, self.resource_id.as_deref())?;

        let rows = self.rows_or_none("alarm 3", &query).await?;

        // A workflow absent from the result had **no failures**, which is
        // zero and healthy. The absence is the answer, so this map is
        // returned as-is and the caller defaults a missing key to 0.
        Some(
            rows.iter()
                .filter_map(|row| {
                    let workflow_id = logs::text(row, "workflow_id")?;
                    // A missing or malformed `Total` is unknown, so the row is
                    // left out, not counted as zero (RCA-P-3).
                    let total = logs::count(row, "Total")?;
                    Some((workflow_id, total))
                })
                .collect(),
        )
    }

    fn is_configured(&self) -> bool {
        // §2.2.2's flag. True when *anything* can be read: a dashboard with
        // a workspace and no subscription is configured, and its queue
        // panel is the part that is unavailable.
        self.workspace_id.is_some() || (self.scope.is_some() && self.namespace.is_some())
    }
}

/// One row of §4.3.1's "Messages today" as an event.
///
/// The interesting part is `customDimensions`, which arrives as the
/// `Properties` blob — a JSON **string**, not an object, so it has to be
/// parsed again.
fn event_from(row: &logs::Row) -> MessageEvent {
    let properties = logs::text(row, "customDimensions")
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or(serde_json::Value::Null);

    let field = |name: &str| {
        properties
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    MessageEvent {
        timestamp: logs::text(row, "timestamp").unwrap_or_default(),
        event_type: field("event_type"),
        workflow_id: field("workflow_id"),
        microservice_id: field("microservice_id"),
        correlation_id: field("correlation_id"),
        operation_id: logs::text(row, "operation_Id").unwrap_or_default(),
        // Whatever the event carried. §1.3 redacts it before display; it
        // is deliberately not filtered here, so the redaction lives in one
        // place rather than being half-done in two.
        payload: properties
            .get("message_content")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
    }
}

/// Read a `workflow_id` → timestamp map out of query rows.
fn timestamps(rows: &[logs::Row], column: &str) -> BTreeMap<String, OffsetDateTime> {
    rows.iter()
        .filter_map(|row| {
            let workflow_id = logs::text(row, "workflow_id")?;
            let raw = logs::text(row, column)?;
            // Log Analytics returns a `datetime` as RFC 3339. A value that
            // will not parse is dropped rather than defaulted: a timestamp
            // guessed as "now" would suppress a real alarm, and one guessed
            // as the epoch would raise a false one.
            let at =
                OffsetDateTime::parse(&raw, &time::format_description::well_known::Rfc3339).ok()?;
            Some((workflow_id, at))
        })
        .collect()
}

/// §4.3.1's retry-delay query, without its threshold.
fn retry_query(hours: u32, resource_id: Option<&str>) -> String {
    format!(
        r#"
    AppMetrics
    | where TimeGenerated > ago({hours}h){filter}
    | where Name == "retry_delay_seconds"
    | extend workflow_id = tostring(Properties["workflow_id"])
    | extend microservice_id = tostring(Properties["microservice_id"])
    | extend queue = tostring(Properties["queue"])
    | extend attempt = tostring(Properties["attempt"])
    | where isnotempty(workflow_id)
    | summarize arg_max(TimeGenerated, Sum, microservice_id, queue, attempt) by workflow_id
    | project workflow_id, timestamp=TimeGenerated, delay_seconds=todouble(Sum),
              microservice_id, queue, attempt
    | order by workflow_id asc
    "#,
        filter = kql::resource_filter(resource_id),
    )
}

/// The bin width for a window of `hours` (§4.3.1's throughput query).
///
/// Chosen so a chart has a useful number of points whatever the window: a
/// day binned by minute is 1,440 points nobody can read, and a month binned
/// by minute is a query that times out.
#[must_use]
pub const fn bin_for(hours: u32) -> &'static str {
    match hours {
        0..=2 => "5m",
        3..=12 => "15m",
        13..=48 => "1h",
        _ => "1d",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pairs: &[(&str, &str)]) -> Config {
        Config::from_pairs(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        )
        .expect("a valid configuration")
    }

    #[test]
    fn an_unconfigured_observatory_reports_itself_unconfigured() {
        // §1.2 and §2.2.2 — this must be constructible with nothing set,
        // because `main` builds it before it knows whether Azure answers.
        let azure =
            LiveObservatory::new(&config(&[])).expect("no CA bundle is configured in tests");

        assert!(!azure.is_configured());
    }

    #[test]
    fn either_integration_alone_counts_as_configured() {
        // §4 — each integration is gated on its own settings. A dashboard
        // with a workspace and no subscription shows exceptions, and that
        // is not an unconfigured dashboard.
        let logs_only = LiveObservatory::new(&config(&[("AZURE_LOG_ANALYTICS_WORKSPACE_ID", "w")]))
            .expect("no CA bundle is configured in tests");
        assert!(logs_only.is_configured());

        let arm_only = LiveObservatory::new(&config(&[
            ("AZURE_SUBSCRIPTION_ID", "s"),
            ("AZURE_RESOURCE_GROUP", "g"),
            ("AZURE_SERVICE_BUS_NAMESPACE", "n"),
        ]))
        .expect("no CA bundle is configured in tests");
        assert!(arm_only.is_configured());
    }

    #[test]
    fn a_partial_arm_configuration_is_not_configured() {
        // Two of the three cannot form a resource id, so the panel is
        // unavailable rather than half-working.
        let partial = LiveObservatory::new(&config(&[
            ("AZURE_SUBSCRIPTION_ID", "s"),
            ("AZURE_RESOURCE_GROUP", "g"),
        ]))
        .expect("no CA bundle is configured in tests");

        assert!(!partial.is_configured());
    }

    #[tokio::test]
    async fn an_unconfigured_observatory_answers_everything_with_nothing() {
        // §4.2 — and without reaching the network, which is what makes
        // §1.2 hold on a laptop.
        let azure =
            LiveObservatory::new(&config(&[])).expect("no CA bundle is configured in tests");

        assert!(azure.exceptions(1).await.is_empty());
        assert!(azure.containers().await.is_empty());
        let series = azure.throughput(1, None, None).await;
        assert!(series.incoming.is_empty());
        assert!(series.outgoing.is_empty());
        assert!(azure.retry_delays(1).await.is_empty());
        assert!(azure.messages(None).await.is_empty());
        assert!(azure.trace("op-1").await.is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_queue_is_unknown_rather_than_missing() {
        // §3.3 and §2.2.3 — the caller must get every queue it asked about,
        // each saying it could not be read. An absent entry lets a caller
        // default it to zero.
        let azure =
            LiveObservatory::new(&config(&[])).expect("no CA bundle is configured in tests");

        let queues = vec!["a".to_string(), "b".to_string()];
        let depths = azure.queue_depths(&queues).await;

        assert_eq!(depths.len(), 2);
        for queue in &queues {
            assert!(!depths[queue].is_known(), "{queue}");
        }
    }

    #[tokio::test]
    async fn a_trace_id_that_sanitises_away_runs_no_query() {
        // §4.4 — the operation id comes straight off the URL path.
        let azure = LiveObservatory::new(&config(&[("AZURE_LOG_ANALYTICS_WORKSPACE_ID", "w")]))
            .expect("no CA bundle is configured in tests");

        assert!(azure.trace("🏥").await.is_empty());
        assert!(azure.trace("\"; drop").await.is_empty());
    }

    #[test]
    fn the_bin_width_keeps_a_chart_readable() {
        // A day binned by minute is 1,440 points nobody can read; a month
        // binned by minute is a query that times out.
        assert_eq!(bin_for(1), "5m");
        assert_eq!(bin_for(6), "15m");
        assert_eq!(bin_for(24), "1h");
        assert_eq!(bin_for(24 * 30), "1d");
    }

    /// A "Messages today" row, with the `Properties` blob as the string it
    /// really arrives as.
    fn message_row(properties: serde_json::Value) -> logs::Row {
        let mut row = logs::Row::new();
        row.insert(
            "timestamp".to_string(),
            serde_json::json!("2026-08-22T09:15:00Z"),
        );
        row.insert("operation_Id".to_string(), serde_json::json!("op-1"));
        row.insert(
            "customDimensions".to_string(),
            serde_json::json!(properties.to_string()),
        );
        row
    }

    #[test]
    fn an_event_reads_its_fields_out_of_the_properties_string() {
        // `Properties` arrives as a JSON **string**, not an object, so it
        // has to be parsed again. Treating it as an object gives an event
        // with every field empty and no error anywhere.
        let row = message_row(serde_json::json!({
            "event_type": "MESSAGE_RECEIVED",
            "workflow_id": "phw-to-mpi",
            "microservice_id": "phw_hl7_server",
            "correlation_id": "3f6d",
            "message_content": "MSH|^~\\&|252\rPID|||8888888"
        }));

        let event = event_from(&row);

        assert_eq!(event.event_type, "MESSAGE_RECEIVED");
        assert_eq!(event.workflow_id, "phw-to-mpi");
        assert_eq!(event.correlation_id, "3f6d");
        assert_eq!(event.operation_id, "op-1");
        assert!(event.payload.is_some());

        // §1.3 — carried unredacted here and redacted at the page, so the
        // redaction lives in one place rather than half-done in two.
        assert!(event.rendered(true).unwrap().contains("***"));
    }

    #[test]
    fn an_event_with_an_unreadable_properties_blob_is_still_an_event() {
        // A row whose `Properties` is not JSON, or is absent. The
        // timestamp and operation id are still worth showing, and a page
        // that dropped the row would hide that something arrived at all.
        for properties in [
            serde_json::json!("not json"),
            serde_json::json!({}),
            serde_json::json!(null),
        ] {
            let event = event_from(&message_row(properties.clone()));

            assert_eq!(event.timestamp, "2026-08-22T09:15:00Z", "{properties}");
            assert_eq!(event.operation_id, "op-1");
            assert!(event.payload.is_none());
            assert!(event.workflow_id.is_empty());
        }
    }

    #[test]
    fn an_event_with_no_columns_at_all_does_not_panic() {
        let event = event_from(&logs::Row::new());

        assert!(event.timestamp.is_empty());
        assert!(event.payload.is_none());
    }

    #[test]
    fn the_retry_query_carries_the_threshold_and_the_resource_filter() {
        let query = retry_query(1, Some("/subs/x"));

        assert!(
            query.contains(r#"Name == "retry_delay_seconds""#),
            "{query}"
        );
        assert!(query.contains("_ResourceId =~ '/subs/x'"), "{query}");
        assert!(query.contains("arg_max(TimeGenerated, Sum"), "{query}");
    }
}
