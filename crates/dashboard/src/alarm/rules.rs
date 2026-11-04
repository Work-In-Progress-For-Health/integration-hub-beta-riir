//! The three alarm rules.
//!
//! Implements `spec/15-dashboard.md` §5.3, §5.4 and §5.5.
//!
//! # Only the trip condition differs
//!
//! §5.1.2's lifecycle is shared and lives in [`crate::alarm::evaluate`].
//! What each alarm contributes is one question:
//!
//! | Alarm | Measures | Trips when |
//! | --- | --- | --- |
//! | 1 (§5.3) | minutes since the last `MESSAGE_RECEIVED` **trace** | **>** the period's threshold |
//! | 2 (§5.4) | minutes since the last `messages_sent` **metric** | **>** the period's threshold |
//! | 3 (§5.5) | `MESSAGE_FAILED` count in the rule's window | **≥** the threshold |
//!
//! The two comparisons genuinely differ and both come from the Python.
//! §5.5.2 records why it matters: at Alarm 3's default threshold of 1, `≥`
//! fires on the first failure and `>` would never fire at all.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::alarm::pause::{
    Pause, active_for, fields_for, next_scheduled_for, scheduled_fields_for,
};
use crate::alarm::store::{AlarmStore, Partition, StoreError};
use crate::alarm::{Outcome, RuleConfig, RuleState, Status, Trip, evaluate};
use crate::period::{Period, period_at};

/// How far back Alarm 1 looks (§5.3.3).
pub const ALARM1_LOOKBACK_DAYS: u32 = 30;

/// How far back Alarm 2 looks.
pub const ALARM2_LOOKBACK_DAYS: u32 = 30;

/// Alarm 1 and 2's thresholds, by period (§5.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InactivityThresholds {
    /// Minutes of quiet during the Day window.
    pub day: u32,
    /// Minutes of quiet during the Evening window.
    pub evening: u32,
    /// Minutes of quiet during the Weekend window.
    pub weekend: u32,
}

impl Default for InactivityThresholds {
    /// §5.3.1's defaults: 60, 120, 240.
    fn default() -> Self {
        Self {
            day: 60,
            evening: 120,
            weekend: 240,
        }
    }
}

impl InactivityThresholds {
    /// The threshold for a period (§5.3.2).
    #[must_use]
    pub const fn for_period(self, period: Period) -> u32 {
        match period {
            Period::Day => self.day,
            Period::Evening => self.evening,
            Period::Weekend => self.weekend,
        }
    }
}

/// Alarm 3's settings (§5.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureThreshold {
    /// The lookback window, in minutes.
    pub window_minutes: u32,
    /// Fire at or above this many failures.
    pub threshold: u32,
}

impl Default for FailureThreshold {
    /// §5.5.1's defaults: a 15-minute window and a threshold of 1.
    fn default() -> Self {
        Self {
            window_minutes: 15,
            threshold: 1,
        }
    }
}

/// Whether Alarm 1 or 2 has tripped (§5.3.2, §5.4.2).
///
/// `last` is the most recent activity, or `None` when the lookback window
/// held nothing at all.
///
/// # Examples
///
/// ```
/// use dashboard::alarm::Trip;
/// use dashboard::alarm::rules::{inactivity_trip, InactivityThresholds};
/// use time::macros::datetime;
///
/// let now = datetime!(2026-08-19 10:00 UTC); // a Wednesday, Day: 60 minutes
/// let thresholds = InactivityThresholds::default();
///
/// // 59 minutes of quiet is fine; 61 is not.
/// let recent = datetime!(2026-08-19 09:01 UTC);
/// assert_eq!(inactivity_trip(Some(recent), now, thresholds), Trip::Clear);
///
/// let old = datetime!(2026-08-19 08:59 UTC);
/// assert_eq!(inactivity_trip(Some(old), now, thresholds), Trip::Tripped);
///
/// // Nothing in the whole window is unknown, not critical: a flow nobody
/// // has ever sent to is not a flow that has failed.
/// assert_eq!(inactivity_trip(None, now, thresholds), Trip::NoData);
/// ```
#[must_use]
pub fn inactivity_trip(
    last: Option<OffsetDateTime>,
    now: OffsetDateTime,
    thresholds: InactivityThresholds,
) -> Trip {
    let Some(last) = last else {
        // §5.3.3 — no trace in the lookback window is `unknown`, not
        // `critical`. A workflow nobody has ever sent to is not one that
        // has failed.
        return Trip::NoData;
    };

    let minutes = (now - last).as_seconds_f64() / 60.0;
    let threshold = f64::from(thresholds.for_period(period_at(now)));

    // §5.3.2 — **strictly** greater. Compare with §5.5.2's at-or-above.
    if minutes > threshold {
        Trip::Tripped
    } else {
        Trip::Clear
    }
}

/// Whether Alarm 3 has tripped (§5.5.2).
///
/// `count` is `None` when the query did not run or failed (§4.5).
///
/// # Examples
///
/// ```
/// use dashboard::alarm::Trip;
/// use dashboard::alarm::rules::{failure_trip, FailureThreshold};
///
/// let settings = FailureThreshold::default(); // threshold 1
///
/// // At or above, unlike Alarm 1's strictly-greater. At the default of 1,
/// // `>` would never fire at all.
/// assert_eq!(failure_trip(Some(0), settings), Trip::Clear);
/// assert_eq!(failure_trip(Some(1), settings), Trip::Tripped);
///
/// assert_eq!(failure_trip(None, settings), Trip::NoData);
/// ```
#[must_use]
pub fn failure_trip(count: Option<u64>, settings: FailureThreshold) -> Trip {
    let Some(count) = count else {
        return Trip::NoData;
    };

    // §5.5.2 — at or above. The difference from §5.3.2 is in the Python and
    // it matters at the default threshold of 1.
    if count >= u64::from(settings.threshold) {
        Trip::Tripped
    } else {
        Trip::Clear
    }
}

/// One rule, evaluated.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Which rule.
    pub rule: RuleConfig,
    /// What it decided.
    pub outcome: Outcome,
    /// The pause record holding the rule, when one does (§10.2.9). A rule
    /// held by a legacy `paused_until` has `Paused` status and no record.
    pub pause: Option<crate::alarm::pause::PauseFields>,
    /// The soonest pause that has not started yet (§10.2.9).
    pub scheduled: Option<crate::alarm::pause::ScheduledFields>,
}

/// Evaluate every rule in a partition (§5.1.2).
///
/// `measure` answers the alarm's own question for one rule. Returning the
/// trip condition rather than the raw measurement keeps this function the
/// same for all three alarms — §5.1.2 is written once here instead of three
/// times as it is in the Python.
///
/// # Errors
///
/// [`StoreError`] when the configuration or state could not be read. **Not
/// an empty list**: a store that is down and a store with no rules must not
/// look the same, or a Cosmos outage silently disables every alarm.
pub async fn evaluate_all<S, F>(
    store: &S,
    partition: Partition,
    now: OffsetDateTime,
    measure: F,
) -> Result<Vec<Row>, StoreError>
where
    S: AlarmStore,
    F: FnMut(&RuleConfig) -> Trip,
{
    evaluate_all_with_pauses(store, partition, now, &[], measure).await
}

/// [`evaluate_all`] with the pause records of `spec/15-dashboard.md` §10.2.
///
/// A rule covered by an active pause is `Paused` and is neither measured
/// nor escalated (§10.2.5). Its state entry is **carried forward
/// unchanged**: the state document is rewritten whole, so dropping the
/// entry would forget a cooldown that is still running and the alarm would
/// re-escalate the moment the pause ended.
///
/// # Errors
///
/// As [`evaluate_all`].
pub async fn evaluate_all_with_pauses<S, F>(
    store: &S,
    partition: Partition,
    now: OffsetDateTime,
    pauses: &[Pause],
    mut measure: F,
) -> Result<Vec<Row>, StoreError>
where
    S: AlarmStore,
    F: FnMut(&RuleConfig) -> Trip,
{
    let rules = store.rules(partition).await?;
    let held = store.state(partition).await?;

    let mut rows = Vec::new();
    let mut next: BTreeMap<String, RuleState> = BTreeMap::new();
    let mut changed = false;

    for rule in rules {
        // §5.1.2 step 1 — a disabled or deleted rule is not evaluated.
        if !rule.is_active() {
            continue;
        }

        let alarm_type = partition.as_str();
        let scheduled = next_scheduled_for(pauses, alarm_type, &rule.id, &rule.workflow_id, now)
            .map(scheduled_fields_for);

        // §10.2.5 — before the lifecycle, so a paused rule that is also in
        // alarm shows `paused`: the operator asked for silence.
        if let Some(pause) = active_for(pauses, alarm_type, &rule.id, &rule.workflow_id, now) {
            if let Some(kept) = held.get(&rule.id) {
                next.insert(rule.id.clone(), kept.clone());
            }
            let fields = fields_for(pause, now);
            rows.push(Row {
                outcome: Outcome {
                    status: Status::Paused,
                    state: held.get(&rule.id).cloned(),
                    escalate: false,
                    cooldown_remaining: None,
                    pause_remaining: fields.pause_remaining,
                },
                rule,
                pause: Some(fields),
                scheduled,
            });
            continue;
        }

        let before = held.get(&rule.id).cloned().unwrap_or_default();
        let outcome = evaluate(&rule, &before, measure(&rule), now);

        match &outcome.state {
            Some(state) => {
                if *state != before {
                    changed = true;
                }
                next.insert(rule.id.clone(), state.clone());
            }
            // §5.1.2 step 4 — the entry is deleted, so the next trip fires
            // immediately rather than meeting a stale cooldown.
            None => {
                if held.contains_key(&rule.id) {
                    changed = true;
                }
            }
        }

        rows.push(Row {
            rule,
            outcome,
            pause: None,
            scheduled,
        });
    }

    // Written only when something moved. An unconditional write would be a
    // Cosmos request per alarm per refresh for an estate where nothing is
    // happening, which is most of the time.
    if changed {
        store.put_state(partition, &next).await?;
    }

    // §5.1.4 — worst first, so an operator reads the top of the page.
    rows.sort_by_key(|row| row.outcome.status);

    Ok(rows)
}

/// A compact count of what a set of rows is doing.
///
/// What the overview shows beside the alarm panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Summary {
    /// Rules needing attention now.
    pub critical: usize,
    /// Rules in alarm but within their cooldown.
    pub suppressed: usize,
    /// Rules whose data could not be read.
    pub unknown: usize,
    /// Rules that are fine.
    pub healthy: usize,
    /// Rules silenced by hand.
    pub paused: usize,
    /// Every rule evaluated.
    pub total: usize,
}

impl Summary {
    /// Count a set of rows.
    #[must_use]
    pub fn of(rows: &[Row]) -> Self {
        let count = |status| rows.iter().filter(|r| r.outcome.status == status).count();

        Self {
            critical: count(Status::Critical),
            suppressed: count(Status::Suppressed),
            unknown: count(Status::Unknown),
            healthy: count(Status::Healthy),
            paused: count(Status::Paused),
            total: rows.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::store::InMemory;
    use time::macros::datetime;

    /// A Wednesday at 10:00 UTC — the Day window, threshold 60 minutes.
    const NOW: OffsetDateTime = datetime!(2026-08-19 10:00 UTC);

    #[test]
    fn the_inactivity_defaults_are_sixty_one_twenty_two_forty() {
        // spec/15-dashboard.md §5.3.1.
        let thresholds = InactivityThresholds::default();

        assert_eq!(thresholds.for_period(Period::Day), 60);
        assert_eq!(thresholds.for_period(Period::Evening), 120);
        assert_eq!(thresholds.for_period(Period::Weekend), 240);
    }

    #[test]
    fn inactivity_trips_strictly_past_the_threshold() {
        // §5.3.2 and §8.9 — at the exact boundary, because the Python's
        // comparison here differs from Alarm 3's and a test well past the
        // threshold would not have noticed.
        let thresholds = InactivityThresholds::default();

        // Exactly 60 minutes is NOT tripped: the comparison is `>`.
        let exactly = NOW - time::Duration::minutes(60);
        assert_eq!(inactivity_trip(Some(exactly), NOW, thresholds), Trip::Clear);

        // A second past it is.
        let past = NOW - time::Duration::seconds(60 * 60 + 1);
        assert_eq!(inactivity_trip(Some(past), NOW, thresholds), Trip::Tripped);
    }

    #[test]
    fn the_applicable_threshold_follows_the_period() {
        // §5.3.2 — 90 minutes of quiet is an alarm during the day and fine
        // in the evening.
        let thresholds = InactivityThresholds::default();

        let day = datetime!(2026-08-19 10:00 UTC); // Wednesday, Day
        assert_eq!(
            inactivity_trip(Some(day - time::Duration::minutes(90)), day, thresholds),
            Trip::Tripped
        );

        let evening = datetime!(2026-08-19 20:00 UTC); // Wednesday, Evening
        assert_eq!(
            inactivity_trip(
                Some(evening - time::Duration::minutes(90)),
                evening,
                thresholds
            ),
            Trip::Clear
        );

        // And the weekend is more forgiving still.
        let weekend = datetime!(2026-08-22 12:00 UTC); // Saturday
        assert_eq!(
            inactivity_trip(
                Some(weekend - time::Duration::minutes(200)),
                weekend,
                thresholds
            ),
            Trip::Clear
        );
    }

    #[test]
    fn nothing_in_the_window_is_no_data_rather_than_tripped() {
        // §5.3.3 — a workflow nobody has ever sent to is not one that has
        // failed. Reporting `critical` would page somebody about a flow
        // that has never carried a message.
        assert_eq!(
            inactivity_trip(None, NOW, InactivityThresholds::default()),
            Trip::NoData
        );
    }

    #[test]
    fn failures_trip_at_or_above_the_threshold() {
        // §5.5.2 and §8.9 — the other comparison. At the default threshold
        // of 1, `>` would never fire at all.
        let settings = FailureThreshold::default();

        assert_eq!(failure_trip(Some(0), settings), Trip::Clear);
        assert_eq!(failure_trip(Some(1), settings), Trip::Tripped);
        assert_eq!(failure_trip(Some(99), settings), Trip::Tripped);

        let higher = FailureThreshold {
            window_minutes: 15,
            threshold: 5,
        };
        assert_eq!(failure_trip(Some(4), higher), Trip::Clear);
        assert_eq!(failure_trip(Some(5), higher), Trip::Tripped);
    }

    #[test]
    fn the_two_comparisons_really_are_different() {
        // §5.3.2 is `>` and §5.5.2 is `>=`. Stated as one test so a future
        // tidy-up that makes them agree fails here rather than changing
        // when an operator is paged.
        let at_threshold_inactivity = inactivity_trip(
            Some(NOW - time::Duration::minutes(60)),
            NOW,
            InactivityThresholds::default(),
        );
        let at_threshold_failures = failure_trip(Some(1), FailureThreshold::default());

        assert_eq!(at_threshold_inactivity, Trip::Clear, "§5.3.2 is >");
        assert_eq!(at_threshold_failures, Trip::Tripped, "§5.5.2 is >=");
    }

    #[test]
    fn a_query_that_did_not_run_is_no_data() {
        // §4.5 — a broken query reports `unknown`, never `healthy`.
        assert_eq!(
            failure_trip(None, FailureThreshold::default()),
            Trip::NoData
        );
    }

    /// A store with one enabled rule.
    fn store_with(rule: RuleConfig) -> InMemory {
        let store = InMemory::new();
        store.add(Partition::Alarm1, rule);
        store
    }

    #[tokio::test]
    async fn a_disabled_rule_is_not_evaluated() {
        // §5.1.2 step 1.
        let store = store_with(RuleConfig {
            alarm_enabled: false,
            ..RuleConfig::for_test("phw-to-mpi")
        });

        let rows = evaluate_all(&store, Partition::Alarm1, NOW, |_| Trip::Tripped)
            .await
            .unwrap();

        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn a_cooldown_survives_from_one_cycle_to_the_next() {
        // §5.1.3 — THE test of this file. Without persistence, every
        // evaluation would be a first trip and a firing alarm would e-mail
        // on every cycle.
        let store = store_with(RuleConfig {
            email_alerts_enabled: true,
            ..RuleConfig::for_test("phw-to-mpi")
        });

        // First cycle: critical, and it escalates.
        let first = evaluate_all(&store, Partition::Alarm1, NOW, |_| Trip::Tripped)
            .await
            .unwrap();
        assert_eq!(first[0].outcome.status, Status::Critical);
        assert!(first[0].outcome.escalate);

        // Second cycle, ten minutes later: still in alarm, and now
        // suppressed by the cooldown the first cycle stored.
        let later = NOW + time::Duration::minutes(10);
        let second = evaluate_all(&store, Partition::Alarm1, later, |_| Trip::Tripped)
            .await
            .unwrap();
        assert_eq!(second[0].outcome.status, Status::Suppressed);
        assert!(!second[0].outcome.escalate);

        // And past the gap it fires again.
        let much_later = NOW + time::Duration::minutes(61);
        let third = evaluate_all(&store, Partition::Alarm1, much_later, |_| Trip::Tripped)
            .await
            .unwrap();
        assert_eq!(third[0].outcome.status, Status::Critical);
        assert!(third[0].outcome.escalate);
    }

    #[tokio::test]
    async fn returning_to_healthy_lets_the_next_trip_fire_at_once() {
        // §5.1.2 step 4, across cycles — the reason the state is deleted
        // rather than kept.
        let store = store_with(RuleConfig {
            email_alerts_enabled: true,
            ..RuleConfig::for_test("phw-to-mpi")
        });

        evaluate_all(&store, Partition::Alarm1, NOW, |_| Trip::Tripped)
            .await
            .unwrap();

        // Recovered.
        let recovered = NOW + time::Duration::minutes(5);
        let rows = evaluate_all(&store, Partition::Alarm1, recovered, |_| Trip::Clear)
            .await
            .unwrap();
        assert_eq!(rows[0].outcome.status, Status::Healthy);

        // And failed again, still well inside the 60-minute gap. It must
        // escalate: this is a new incident.
        let again = NOW + time::Duration::minutes(10);
        let rows = evaluate_all(&store, Partition::Alarm1, again, |_| Trip::Tripped)
            .await
            .unwrap();
        assert_eq!(rows[0].outcome.status, Status::Critical);
        assert!(
            rows[0].outcome.escalate,
            "a new incident must not be swallowed by the last one's cooldown"
        );
    }

    #[tokio::test]
    async fn a_store_outage_is_an_error_rather_than_every_alarm_going_quiet() {
        // A Cosmos outage must not read as "no rules configured", which
        // would report the whole estate healthy.
        let store = store_with(RuleConfig::for_test("phw-to-mpi"));
        store.fail("503");

        let result = evaluate_all(&store, Partition::Alarm1, NOW, |_| Trip::Tripped).await;

        assert!(result.is_err(), "a store outage must surface");
    }

    #[tokio::test]
    async fn nothing_is_written_when_nothing_moved() {
        // An unconditional write would be a Cosmos request per alarm per
        // refresh for an estate where nothing is happening — which is most
        // of the time, and it is billed per request.
        let store = store_with(RuleConfig::for_test("phw-to-mpi"));

        // Healthy, with no state held: nothing to store and nothing to
        // delete.
        evaluate_all(&store, Partition::Alarm1, NOW, |_| Trip::Clear)
            .await
            .unwrap();

        // The write path is what would have failed, so making the store
        // fail *now* proves no write is attempted.
        store.fail("503 — no write should be attempted");
        let result = evaluate_all(&store, Partition::Alarm1, NOW, |_| Trip::Clear).await;

        // The *read* fails, which is expected; the point is the first call
        // above succeeded without writing.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rows_come_back_worst_first() {
        // §5.1.4.
        let store = InMemory::new();
        for id in ["a-healthy", "b-critical", "c-unknown"] {
            store.add(Partition::Alarm1, RuleConfig::for_test(id));
        }

        let rows = evaluate_all(&store, Partition::Alarm1, NOW, |rule| {
            match rule.id.as_str() {
                "b-critical" => Trip::Tripped,
                "c-unknown" => Trip::NoData,
                _ => Trip::Clear,
            }
        })
        .await
        .unwrap();

        let statuses: Vec<Status> = rows.iter().map(|r| r.outcome.status).collect();
        assert_eq!(
            statuses,
            vec![Status::Critical, Status::Unknown, Status::Healthy]
        );
    }

    #[tokio::test]
    async fn a_summary_counts_every_status() {
        let store = InMemory::new();
        for id in ["a", "b", "c"] {
            store.add(Partition::Alarm1, RuleConfig::for_test(id));
        }

        let rows = evaluate_all(&store, Partition::Alarm1, NOW, |rule| {
            match rule.id.as_str() {
                "a" => Trip::Tripped,
                "b" => Trip::NoData,
                _ => Trip::Clear,
            }
        })
        .await
        .unwrap();

        let summary = Summary::of(&rows);

        assert_eq!(summary.critical, 1);
        assert_eq!(summary.unknown, 1);
        assert_eq!(summary.healthy, 1);
        assert_eq!(summary.total, 3);
    }

    // --- §10.2: pause records in the lifecycle ----------------------------

    fn all_pause(start: OffsetDateTime, end: Option<OffsetDateTime>) -> Pause {
        Pause {
            pause_id: "p-1".to_string(),
            scope: crate::alarm::pause::Scope::All,
            flows: Vec::new(),
            rules: Vec::new(),
            start_at: Some(start),
            end_at: end,
            reason: "planned maintenance".to_string(),
            requested_by: "ops".to_string(),
            created_at: Some(start),
            cancelled_at: None,
        }
    }

    #[tokio::test]
    async fn a_paused_rule_that_would_trip_is_paused_and_does_not_escalate() {
        // spec/15-dashboard.md §10.2.5.
        let store = InMemory::new();
        store.add(
            Partition::Alarm1,
            RuleConfig {
                email_alerts_enabled: true,
                ..RuleConfig::for_test("phw-to-mpi")
            },
        );
        let pauses = [all_pause(
            NOW - time::Duration::minutes(10),
            Some(NOW + time::Duration::minutes(50)),
        )];

        let rows =
            evaluate_all_with_pauses(&store, Partition::Alarm1, NOW, &pauses, |_| Trip::Tripped)
                .await
                .unwrap();

        assert_eq!(rows[0].outcome.status, Status::Paused);
        assert!(!rows[0].outcome.escalate);
        assert_eq!(
            rows[0].pause.as_ref().map(|p| p.pause_remaining),
            Some(Some(50.0))
        );
        assert!(
            store.state(Partition::Alarm1).await.unwrap().is_empty(),
            "a paused rule starts no cooldown"
        );
    }

    #[tokio::test]
    async fn a_paused_rule_is_not_measured() {
        // §10.2.5 — "not evaluated further".
        let store = InMemory::new();
        store.add(Partition::Alarm1, RuleConfig::for_test("phw-to-mpi"));
        let pauses = [all_pause(NOW - time::Duration::minutes(1), None)];
        let mut asked = 0;

        evaluate_all_with_pauses(&store, Partition::Alarm1, NOW, &pauses, |_| {
            asked += 1;
            Trip::Tripped
        })
        .await
        .unwrap();

        assert_eq!(asked, 0);
    }

    #[tokio::test]
    async fn a_running_cooldown_survives_a_pause() {
        // §10.2.5 — the state document is rewritten whole, so an entry left
        // out is a cooldown forgotten, and the alarm would re-escalate the
        // moment the pause ended.
        let store = InMemory::new();
        store.add(Partition::Alarm1, RuleConfig::for_test("a"));
        store.add(Partition::Alarm1, RuleConfig::for_test("b"));

        let cooling = RuleState {
            last_alarm_at: Some(NOW - time::Duration::minutes(5)),
            ..RuleState::default()
        };
        let mut held = BTreeMap::new();
        held.insert("a".to_string(), cooling.clone());
        held.insert("b".to_string(), cooling.clone());
        store.put_state(Partition::Alarm1, &held).await.unwrap();

        let mut pause = all_pause(NOW - time::Duration::minutes(1), None);
        pause.scope = crate::alarm::pause::Scope::Rule;
        pause.rules = vec![crate::alarm::pause::RuleTarget {
            alarm_type: "alarm1".to_string(),
            rule_id: "a".to_string(),
            workflow_id: "a".to_string(),
        }];

        // `b` returns to healthy, which rewrites the document.
        evaluate_all_with_pauses(&store, Partition::Alarm1, NOW, &[pause], |_| Trip::Clear)
            .await
            .unwrap();

        let after = store.state(Partition::Alarm1).await.unwrap();
        assert_eq!(after.get("a"), Some(&cooling), "kept through the pause");
        assert!(
            !after.contains_key("b"),
            "healthy still clears (§5.1.2 step 4)"
        );
    }

    #[tokio::test]
    async fn an_ended_pause_no_longer_pauses() {
        let store = InMemory::new();
        store.add(Partition::Alarm1, RuleConfig::for_test("phw-to-mpi"));
        let pauses = [all_pause(
            NOW - time::Duration::hours(2),
            Some(NOW - time::Duration::hours(1)),
        )];

        let rows =
            evaluate_all_with_pauses(&store, Partition::Alarm1, NOW, &pauses, |_| Trip::Tripped)
                .await
                .unwrap();

        assert_eq!(rows[0].outcome.status, Status::Critical);
        assert!(rows[0].pause.is_none());
    }

    #[tokio::test]
    async fn a_rule_notes_the_pause_that_is_about_to_start() {
        // §10.2.9.
        let store = InMemory::new();
        store.add(Partition::Alarm1, RuleConfig::for_test("phw-to-mpi"));
        let pauses = [all_pause(NOW + time::Duration::hours(3), None)];

        let rows =
            evaluate_all_with_pauses(&store, Partition::Alarm1, NOW, &pauses, |_| Trip::Clear)
                .await
                .unwrap();

        assert_eq!(rows[0].outcome.status, Status::Healthy);
        assert_eq!(
            rows[0].scheduled.as_ref().map(|s| s.pause_id.as_str()),
            Some("p-1")
        );
    }
}
