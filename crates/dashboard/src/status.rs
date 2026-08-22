//! The overview payload.
//!
//! Implements `spec/15-dashboard.md` §2.2 and §3.
//!
//! This is what `/api/status` returns and what the overview page renders
//! from. The JSON shape is a contract with `dashboard.js` (§8.5), so the
//! field names here are the Python's even where a Rust name would read
//! better.
//!
//! # The one place the shape is deliberately different
//!
//! The Python computes its KPI totals as
//! `sum(q.get("active_message_count", 0) for q in queues)` — a queue whose
//! depth could not be read contributes **zero**. So an operations centre
//! looking at "0 messages waiting" cannot tell a quiet estate from a
//! management API outage, which is the exact confusion §2.2.2 exists to
//! prevent, appearing in the number an operator looks at first.
//!
//! Here a total is [`Option`]: `None` when any queue in it is unknown, and
//! `queues_unknown` says how many. Recorded as §9.8.

use crate::config::Thresholds;
use crate::flows::{FLOWS, Flow};
use crate::health::{ContainerState, Health, QueueDepths};
use crate::observe::{Container, Exception, RetryDelay};

/// One flow, as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FlowRow {
    /// The `WORKFLOW_ID` (§3.1.1).
    pub id: &'static str,
    /// What an operator reads.
    pub label: &'static str,
    /// The source system.
    pub source: &'static str,
    /// The destination.
    pub destination: &'static str,
    /// The ingress port.
    pub source_port: u16,
    /// The transformer, when there is one.
    pub transformer: Option<&'static str>,
    /// This flow's health (§3.2).
    #[serde(serialize_with = "health_string")]
    pub health: Health,
    /// Messages waiting, or `None` when that could not be read.
    pub active: Option<u64>,
    /// Dead-lettered messages, or `None`.
    pub dlq: Option<u64>,
}

/// Serialise [`Health`] as the lowercase string `dashboard.js` expects.
fn health_string<S: serde::Serializer>(health: &Health, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(health.css_class())
}

/// The headline numbers (§2.2).
///
/// Every total is optional. See this module's documentation: a `0` that
/// might mean "I could not look" is worse than no number at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Kpis {
    /// Messages waiting across every queue, or `None` if any is unknown.
    pub total_active_messages: Option<u64>,
    /// Dead-lettered messages across every queue, or `None`.
    pub total_dlq_messages: Option<u64>,
    /// How many queues could not be read at all.
    ///
    /// The flag §2.2.2 asks for: a browser renders "unavailable" from this
    /// rather than inferring it from a zero.
    pub queues_unknown: usize,
    /// Exceptions in the last hour.
    pub exception_count_1h: usize,
    /// Flows in each state.
    pub flows_healthy: usize,
    /// Flows in each state.
    pub flows_warning: usize,
    /// Flows in each state.
    pub flows_critical: usize,
    /// Flows in each state.
    pub flows_unknown: usize,
}

/// The whole overview payload.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Status {
    /// When this was built, RFC 3339.
    pub refreshed_at: String,
    /// The estate's health, rolled up (§3.2).
    #[serde(serialize_with = "health_string")]
    pub system_health: Health,
    /// Whether Azure was reachable at all (§2.2.2).
    ///
    /// `false` means every number below is a placeholder. The browser
    /// renders "unavailable"; it does not render zeroes.
    pub azure_configured: bool,
    /// The headline numbers.
    pub kpis: Kpis,
    /// One row per flow.
    pub flows: Vec<FlowRow>,
    /// One row per queue (§2.3.1) — `dashboard.js` renders the queue table
    /// from this, so it is part of the payload rather than only
    /// `/api/servicebus-metrics`.
    pub queues: Vec<QueueRow>,
    /// The five most recent exceptions.
    pub recent_exceptions: Vec<Exception>,
    /// The most recent retry delay per flow.
    pub retry_delays: Vec<RetryDelay>,
    /// The retry panel's headline numbers (§2.3.1).
    pub retry_delay_kpis: RetryKpis,
}

/// One queue, as `dashboard.js` renders it (§2.3.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QueueRow {
    /// The queue's name.
    pub name: String,
    /// Messages waiting, or `null` when it could not be read.
    pub active_message_count: Option<u64>,
    /// Dead-lettered messages, or `null`.
    pub dead_letter_message_count: Option<u64>,
}

/// The retry panel's numbers (§2.3.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct RetryKpis {
    /// Flows whose most recent retry delay exceeded a minute.
    pub flows_over_1m: usize,
    /// Flows reporting a retry delay at all.
    pub flows_reporting: usize,
}

/// How many exceptions the overview shows.
const RECENT_EXCEPTIONS: usize = 5;

/// Build the payload (§2.2, §3.2).
///
/// Pure: everything it needs has already been fetched, so the whole of §3's
/// health logic is testable without an Azure client or a clock.
///
/// # Examples
///
/// With nothing configured, every flow is Unknown and no total is invented:
///
/// ```
/// use dashboard::config::Thresholds;
/// use dashboard::health::Health;
/// use dashboard::status::build;
///
/// let status = build(
///     &Default::default(),
///     &[],
///     &[],
///     &[],
///     Thresholds::default(),
///     false,
///     "2026-08-21T09:15:00Z".to_string(),
/// );
///
/// assert_eq!(status.system_health, Health::Unknown);
/// assert!(!status.azure_configured);
/// assert_eq!(status.kpis.total_active_messages, None, "not zero");
/// assert_eq!(status.kpis.flows_unknown, 8);
/// ```
#[must_use]
pub fn build(
    depths: &std::collections::BTreeMap<String, QueueDepths>,
    containers: &[Container],
    exceptions: &[Exception],
    retry_delays: &[RetryDelay],
    thresholds: Thresholds,
    azure_configured: bool,
    refreshed_at: String,
) -> Status {
    let rows: Vec<FlowRow> = FLOWS
        .iter()
        .map(|flow| flow_row(flow, depths, containers, thresholds))
        .collect();

    let all: Vec<QueueDepths> = depths.values().copied().collect();

    // No queues reported at all is *unknown*, not an estate with zero
    // messages on it — `total` over an empty iterator is `Some(0)`, which
    // is the right answer to "what is the sum" and the wrong answer to
    // "how many messages are waiting". This is the module's own defect,
    // caught by its own test; see `flow_row` for the same guard one level
    // down.
    let totals = |pick: fn(&QueueDepths) -> Option<u64>| {
        if all.is_empty() {
            None
        } else {
            total(all.iter().map(pick))
        }
    };

    Status {
        refreshed_at,
        // §3.2 rolled up. `Health`'s ordering is worst-first, so the
        // minimum is the worst — which is what an estate's health is.
        system_health: rows
            .iter()
            .map(|row| row.health)
            .min()
            .unwrap_or(Health::Unknown),
        azure_configured,
        kpis: Kpis {
            total_active_messages: totals(|d| d.active),
            total_dlq_messages: totals(|d| d.dead_letter),
            queues_unknown: all.iter().filter(|d| !d.is_known()).count(),
            exception_count_1h: exceptions.len(),
            flows_healthy: count_of(&rows, Health::Healthy),
            flows_warning: count_of(&rows, Health::Warning),
            flows_critical: count_of(&rows, Health::Critical),
            flows_unknown: count_of(&rows, Health::Unknown),
        },
        flows: rows,
        queues: depths
            .iter()
            .map(|(name, depths)| QueueRow {
                name: name.clone(),
                active_message_count: depths.active,
                dead_letter_message_count: depths.dead_letter,
            })
            .collect(),
        recent_exceptions: exceptions.iter().take(RECENT_EXCEPTIONS).cloned().collect(),
        retry_delay_kpis: RetryKpis {
            flows_over_1m: retry_delays
                .iter()
                .filter(|delay| delay.delay_seconds.is_some_and(|seconds| seconds > 60.0))
                .count(),
            flows_reporting: retry_delays.len(),
        },
        retry_delays: retry_delays.to_vec(),
    }
}

/// Sum, or `None` if anything is missing.
///
/// The correction this module's documentation describes: a partial sum
/// presented as a total is a wrong number that looks like a right one.
fn total<I: IntoIterator<Item = Option<u64>>>(values: I) -> Option<u64> {
    let mut sum = 0_u64;
    for value in values {
        sum = sum.checked_add(value?)?;
    }
    Some(sum)
}

/// How many rows are in this state.
fn count_of(rows: &[FlowRow], health: Health) -> usize {
    rows.iter().filter(|row| row.health == health).count()
}

/// One flow's row.
fn flow_row(
    flow: &'static Flow,
    depths: &std::collections::BTreeMap<String, QueueDepths>,
    containers: &[Container],
    thresholds: Thresholds,
) -> FlowRow {
    // A flow's queues are whichever ones name it. Queue names come from the
    // environment (`spec/01-architecture.md` §2.1.1), so this matches on the
    // workflow id appearing in the name rather than constructing one.
    let mine: Vec<QueueDepths> = depths
        .iter()
        .filter(|(name, _)| name.contains(flow.workflow_id))
        .map(|(_, depths)| *depths)
        .collect();

    let combined = QueueDepths {
        active: total(mine.iter().map(|d| d.active)),
        dead_letter: total(mine.iter().map(|d| d.dead_letter)),
    };

    // A flow with no queues in the map at all knows nothing, rather than
    // summing an empty list to zero.
    let combined = if mine.is_empty() {
        QueueDepths::unknown()
    } else {
        combined
    };

    let states: Vec<ContainerState> = flow
        .transformer
        .into_iter()
        .map(|name| {
            containers
                .iter()
                .find(|c| c.name.contains(name))
                .map_or(ContainerState::Unknown, |c| c.state)
        })
        .collect();

    FlowRow {
        id: flow.workflow_id,
        label: flow.label,
        source: flow.source,
        destination: flow.destination,
        source_port: flow.port,
        transformer: flow.transformer,
        health: crate::health::assess(combined, &states, thresholds),
        active: combined.active,
        dlq: combined.dead_letter,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const NOW: &str = "2026-08-21T09:15:00Z";

    fn depths(entries: &[(&str, QueueDepths)]) -> BTreeMap<String, QueueDepths> {
        entries
            .iter()
            .map(|(name, depths)| ((*name).to_string(), *depths))
            .collect()
    }

    fn built(
        depths: &BTreeMap<String, QueueDepths>,
        containers: &[Container],
        configured: bool,
    ) -> Status {
        build(
            depths,
            containers,
            &[],
            &[],
            Thresholds::default(),
            configured,
            NOW.to_string(),
        )
    }

    #[test]
    fn with_no_azure_every_flow_is_unknown_and_no_total_is_invented() {
        // spec/15-dashboard.md §1.2, §2.2.2 and §8.1.
        let status = built(&BTreeMap::new(), &[], false);

        assert_eq!(status.system_health, Health::Unknown);
        assert!(!status.azure_configured);
        assert_eq!(status.kpis.total_active_messages, None);
        assert_eq!(status.kpis.total_dlq_messages, None);
        assert_eq!(status.kpis.flows_unknown, FLOWS.len());
        assert_eq!(status.kpis.flows_healthy, 0);
        assert_eq!(status.flows.len(), FLOWS.len());
    }

    #[test]
    fn an_unreadable_queue_makes_the_total_absent_rather_than_wrong() {
        // §9.8 — THE test of this file, and the correction to the Python.
        //
        // `sum(q.get("active_message_count", 0) ...)` treats a queue it
        // could not read as empty, so an operations centre sees "3 messages
        // waiting" during a management API outage and cannot tell that from
        // a quiet estate. It is the first number on the page.
        let mixed = depths(&[
            ("local-phw-to-mpi-ingress", QueueDepths::known(3, 0)),
            ("local-pims-to-mpi-ingress", QueueDepths::unknown()),
        ]);

        let status = built(&mixed, &[], true);

        assert_eq!(
            status.kpis.total_active_messages, None,
            "a partial sum must not be presented as a total"
        );
        assert_eq!(status.kpis.queues_unknown, 1, "and the page can say why");
    }

    #[test]
    fn totals_are_summed_when_everything_is_known() {
        let all_known = depths(&[
            ("local-phw-to-mpi-ingress", QueueDepths::known(3, 1)),
            ("local-pims-to-mpi-ingress", QueueDepths::known(4, 0)),
        ]);

        let status = built(&all_known, &[], true);

        assert_eq!(status.kpis.total_active_messages, Some(7));
        assert_eq!(status.kpis.total_dlq_messages, Some(1));
        assert_eq!(status.kpis.queues_unknown, 0);
    }

    #[test]
    fn the_system_health_is_the_worst_flow() {
        // §3.2, rolled up: critical beats warning beats unknown beats
        // healthy, which is the order an operator triages in.
        let one_critical = depths(&[
            ("local-phw-to-mpi-ingress", QueueDepths::known(0, 5)),
            ("local-pims-to-mpi-ingress", QueueDepths::known(0, 0)),
            ("local-paris-to-mpi-ingress", QueueDepths::known(0, 0)),
            ("local-chemocare-to-mpi-ingress", QueueDepths::known(0, 0)),
            ("local-mpi-to-topic-ingress", QueueDepths::known(0, 0)),
            ("local-wds-to-mpi-ingress", QueueDepths::known(0, 0)),
            ("local-mosaiq-to-mpi-ingress", QueueDepths::known(0, 0)),
            ("local-lims-soapserver-ingress", QueueDepths::known(0, 0)),
        ]);

        let status = built(&one_critical, &[], true);

        assert_eq!(status.system_health, Health::Critical);
        assert_eq!(status.kpis.flows_critical, 1, "PHW, on its dead-letters");

        // Five healthy and two unknown, not seven healthy: ChemoCare and
        // PIMS have transformers (`flows` §2.1) and no container was
        // reported for either, so §3.2's container row cannot be decided.
        // PHW has a transformer too, but its dead-letters make it Critical
        // whatever the container is doing.
        assert_eq!(status.kpis.flows_healthy, 5);
        assert_eq!(status.kpis.flows_unknown, 2);
    }

    #[test]
    fn a_healthy_estate_reads_healthy() {
        let quiet: Vec<(String, QueueDepths)> = FLOWS
            .iter()
            .map(|f| {
                (
                    format!("local-{}-ingress", f.workflow_id),
                    QueueDepths::known(0, 0),
                )
            })
            .collect();
        let quiet: BTreeMap<String, QueueDepths> = quiet.into_iter().collect();

        // Every transformer running, so no container row trips.
        let containers: Vec<Container> = FLOWS
            .iter()
            .filter_map(|f| f.transformer)
            .map(|name| Container {
                name: name.to_string(),
                state: ContainerState::Running,
                replicas: Some(1),
                restarts: Some(0),
            })
            .collect();

        let status = built(&quiet, &containers, true);

        assert_eq!(status.system_health, Health::Healthy);
        assert_eq!(status.kpis.flows_healthy, FLOWS.len());
        assert_eq!(status.kpis.total_active_messages, Some(0));
    }

    #[test]
    fn a_flow_with_no_queue_in_the_map_is_unknown_not_zero() {
        // Summing an empty list gives zero, and zero would read as a
        // healthy queue. A flow nobody reported on is a flow nobody knows
        // about.
        let only_phw = depths(&[("local-phw-to-mpi-ingress", QueueDepths::known(0, 0))]);

        let status = built(&only_phw, &[], true);

        let phw = status.flows.iter().find(|f| f.id == "phw-to-mpi").unwrap();
        let pims = status.flows.iter().find(|f| f.id == "pims-to-mpi").unwrap();

        // PHW has a queue but its transformer is unaccounted for.
        assert_eq!(phw.active, Some(0));

        assert_eq!(pims.health, Health::Unknown);
        assert_eq!(pims.active, None, "not Some(0)");
        assert_eq!(pims.dlq, None);
    }

    #[test]
    fn a_flows_queues_are_added_together() {
        // A flow has a pre-transform and a post-transform queue
        // (`spec/01-architecture.md` §2.1), and the row shows the flow.
        let two = depths(&[
            ("local-phw-to-mpi-pre", QueueDepths::known(2, 0)),
            ("local-phw-to-mpi-post", QueueDepths::known(3, 1)),
        ]);

        let status = built(&two, &[], true);
        let phw = status.flows.iter().find(|f| f.id == "phw-to-mpi").unwrap();

        assert_eq!(phw.active, Some(5));
        assert_eq!(phw.dlq, Some(1));
        assert_eq!(phw.health, Health::Critical, "one dead-letter");
    }

    #[test]
    fn a_stopped_transformer_makes_its_flow_critical() {
        // §3.2's container row.
        let quiet = depths(&[("local-phw-to-mpi-ingress", QueueDepths::known(0, 0))]);
        let stopped = vec![Container {
            name: "hl7-phw-transformer".to_string(),
            state: ContainerState::NotRunning,
            replicas: Some(0),
            restarts: Some(4),
        }];

        let status = built(&quiet, &stopped, true);
        let phw = status.flows.iter().find(|f| f.id == "phw-to-mpi").unwrap();

        assert_eq!(phw.health, Health::Critical);
    }

    #[test]
    fn only_five_exceptions_reach_the_overview() {
        // §2.2's payload. The exceptions *page* shows 200; the overview
        // shows five, and the KPI counts them all.
        let many: Vec<Exception> = (0..12)
            .map(|n| Exception {
                timestamp: format!("2026-08-21T09:{n:02}:00Z"),
                kind: "ValueError".to_string(),
                outer_message: format!("boom {n}"),
                severity_level: Some(3),
                app_name: "phw-hl7-server".to_string(),
                operation_id: format!("op{n}"),
            })
            .collect();

        let status = build(
            &BTreeMap::new(),
            &[],
            &many,
            &[],
            Thresholds::default(),
            true,
            NOW.to_string(),
        );

        assert_eq!(status.recent_exceptions.len(), 5);
        assert_eq!(
            status.kpis.exception_count_1h, 12,
            "the count is not capped"
        );
        assert_eq!(status.recent_exceptions[0].outer_message, "boom 0");
    }

    #[test]
    fn the_json_keys_are_the_ones_the_browser_reads() {
        // §2.3.1 and §8.5 — taken from `dashboard.js :: refreshData`, which
        // is the actual contract. A key it reads and this does not emit is
        // an undefined in the browser and a blank panel with nothing in the
        // console.
        let status = built(&BTreeMap::new(), &[], false);
        let json = serde_json::to_value(&status).expect("the status serialises");

        for key in [
            "refreshed_at",
            "system_health",
            "azure_configured",
            "kpis",
            "flows",
            "queues",
            "recent_exceptions",
            "retry_delays",
            "retry_delay_kpis",
        ] {
            assert!(json.get(key).is_some(), "{key} is missing: {json}");
        }

        // The retry panel's two numbers.
        for key in ["flows_over_1m", "flows_reporting"] {
            assert!(
                json["retry_delay_kpis"].get(key).is_some(),
                "{key} is missing: {json}"
            );
        }

        let kpis = json.get("kpis").unwrap();
        for key in [
            "total_active_messages",
            "total_dlq_messages",
            "queues_unknown",
            "exception_count_1h",
            "flows_healthy",
            "flows_warning",
            "flows_critical",
            "flows_unknown",
        ] {
            assert!(kpis.get(key).is_some(), "{key} is missing: {kpis}");
        }

        // §3.2's states render as the lowercase strings the CSS keys on.
        assert_eq!(json["system_health"], "unknown");
    }

    #[test]
    fn an_unknown_total_serialises_as_null_not_zero() {
        // The whole point, at the wire. A browser must be able to tell
        // these apart, and `null` is how.
        let status = built(&BTreeMap::new(), &[], false);
        let json = serde_json::to_string(&status).unwrap();

        assert!(json.contains(r#""total_active_messages":null"#), "{json}");
        assert!(!json.contains(r#""total_active_messages":0"#), "{json}");
    }

    #[test]
    fn a_total_that_would_overflow_is_absent_rather_than_wrapped() {
        // Not reachable with real queue depths, and one line to get right
        // rather than a panic in release and a wrong number in debug.
        let huge = depths(&[
            ("a-phw-to-mpi", QueueDepths::known(u64::MAX, 0)),
            ("b-phw-to-mpi", QueueDepths::known(1, 0)),
        ]);

        let status = built(&huge, &[], true);

        assert_eq!(status.kpis.total_active_messages, None);
    }
}
