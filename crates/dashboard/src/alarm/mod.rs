//! Alarm evaluation.
//!
//! Implements `spec/15-dashboard.md` §5.1.2, §5.2 and §5.3–§5.5.
//!
//! # One lifecycle, three rules
//!
//! §5.1.2's six steps are shared by all three alarms; only the **trip
//! condition** differs, and it is the only thing [`evaluate`] takes as a
//! parameter. The Python triplicated the lifecycle across `alarm1.py`,
//! `alarm2.py` and `alarm3.py` — nearly 1,600 lines with the sequence
//! written out three times — and the three copies do not agree: §5.3.2 is
//! strictly-greater and §5.5.2 is at-or-above.
//!
//! That difference is real and preserved. Everything around it is written
//! once.
//!
//! # The order of the steps is the behaviour
//!
//! §5.1.2 is a numbered list because the cases overlap, and reordering them
//! changes what an operator sees. A paused rule that is also in alarm shows
//! `paused` — the operator asked for silence and got it. A rule with no
//! data shows `unknown` rather than `healthy`, because a query that
//! returned nothing and a flow that is quiet are not the same thing.

use time::OffsetDateTime;

pub mod cosmos_store;
pub mod rules;
pub mod store;

use crate::period::{Period, period_at};

/// What a rule is doing (§5.1.2).
///
/// Ordered as §5.1.4 requires, so [`Ord`] sorts the page: the rows an
/// operator must act on come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// Silenced by hand, and still within the pause window.
    Paused,
    /// In alarm, and the cooldown has expired: this needs attention.
    Critical,
    /// In alarm, but within the re-alarm cooldown.
    Suppressed,
    /// No data at all — see §5.1.2 step 3. **Not** healthy.
    Unknown,
    /// Below the trip condition.
    Healthy,
}

impl Status {
    /// The label the page shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Paused => "Paused",
            Self::Critical => "Critical",
            Self::Suppressed => "Suppressed",
            Self::Unknown => "Unknown",
            Self::Healthy => "Healthy",
        }
    }

    /// Whether this status is an alarm condition at all.
    ///
    /// `Critical` and `Suppressed` both are; they differ only in whether
    /// the cooldown has expired.
    #[must_use]
    pub const fn is_in_alarm(self) -> bool {
        matches!(self, Self::Critical | Self::Suppressed)
    }
}

/// A rule's stored state (§5.1.3).
///
/// Persisted to Cosmos, so a cooldown survives a restart. Without that, a
/// deployment would re-escalate every firing alarm on every restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleState {
    /// When this rule last escalated.
    pub last_alarm_at: Option<OffsetDateTime>,
    /// When a manual pause expires.
    pub paused_until: Option<OffsetDateTime>,
    /// Why it was paused.
    pub pause_reason: Option<String>,
}

impl RuleState {
    /// Whether anything is worth storing.
    ///
    /// An empty state is **deleted** rather than written back, so a rule
    /// that has returned to healthy leaves no document behind to be
    /// misread as a stale cooldown.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.last_alarm_at.is_none() && self.paused_until.is_none()
    }
}

/// The settings every rule shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleConfig {
    /// The rule's key in its Cosmos document.
    pub id: String,
    /// What an operator sees.
    pub display_name: String,
    /// The `WORKFLOW_ID` this rule watches (`flows` §3.1.1).
    pub workflow_id: String,
    /// Whether this rule is evaluated at all.
    pub alarm_enabled: bool,
    /// Whether it has been deleted, which is a soft delete in Cosmos.
    pub deleted: bool,
    /// Minutes before an alarm re-fires.
    pub alerting_gap_minutes: u32,
    /// Whether e-mail escalation is on.
    pub email_alerts_enabled: bool,
    /// Whether e-mail escalation is allowed out of hours (§5.2).
    pub email_ooh_enabled: bool,
}

impl RuleConfig {
    /// Whether this rule is evaluated (§5.1.2 step 1).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.alarm_enabled && !self.deleted
    }

    /// Whether an escalation now would be sent by e-mail (§5.2.1).
    ///
    /// The guard suppresses the **e-mail**, not the alarm: the rule still
    /// reports `Critical` on the page.
    ///
    /// # Examples
    ///
    /// ```
    /// use dashboard::alarm::RuleConfig;
    /// use dashboard::period::Period;
    ///
    /// let mut rule = RuleConfig::for_test("phw-to-mpi");
    /// rule.email_alerts_enabled = true;
    /// rule.email_ooh_enabled = false;
    ///
    /// assert!(rule.may_email(Period::Day));
    /// assert!(!rule.may_email(Period::Evening), "out of hours, and not allowed");
    /// assert!(!rule.may_email(Period::Weekend));
    ///
    /// rule.email_ooh_enabled = true;
    /// assert!(rule.may_email(Period::Weekend));
    /// ```
    #[must_use]
    pub const fn may_email(&self, period: Period) -> bool {
        if !self.email_alerts_enabled {
            return false;
        }
        // §5.2.1 — and §9.1: the Python stores this toggle and never reads
        // it, so a rule set to stay quiet overnight e-mails overnight.
        self.email_ooh_enabled || !period.is_out_of_hours()
    }

    /// A rule with everything off, for tests and examples.
    #[must_use]
    pub fn for_test(workflow_id: &str) -> Self {
        Self {
            id: workflow_id.to_string(),
            display_name: workflow_id.to_string(),
            workflow_id: workflow_id.to_string(),
            alarm_enabled: true,
            deleted: false,
            alerting_gap_minutes: 60,
            email_alerts_enabled: false,
            email_ooh_enabled: false,
        }
    }
}

/// Whether the rule's measurement is over its threshold.
///
/// Computed by the caller, because the three alarms measure different
/// things: §5.3.2 and §5.4.2 compare an **age** with strictly-greater,
/// §5.5.2 compares a **count** with at-or-above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trip {
    /// Above the trip condition.
    Tripped,
    /// Below it.
    Clear,
    /// No data at all — §5.1.2 step 3.
    NoData,
}

/// What evaluating a rule decided.
///
/// `PartialEq` but not `Eq`: the two remaining-minutes fields are `f64`,
/// because the page shows fractions of a minute.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// What to show.
    pub status: Status,
    /// The state to store, or `None` to delete the rule's entry.
    pub state: Option<RuleState>,
    /// Whether an e-mail should be sent now.
    pub escalate: bool,
    /// Minutes left of the cooldown, when `Suppressed`.
    pub cooldown_remaining: Option<f64>,
    /// Minutes left of the pause, when `Paused`.
    pub pause_remaining: Option<f64>,
}

/// Evaluate one rule (§5.1.2).
///
/// The six steps, in order. `trip` is the only rule-specific input.
///
/// # Examples
///
/// A rule that trips for the first time escalates:
///
/// ```
/// use dashboard::alarm::{evaluate, RuleConfig, RuleState, Status, Trip};
/// use time::macros::datetime;
///
/// let mut rule = RuleConfig::for_test("phw-to-mpi");
/// rule.email_alerts_enabled = true;
/// let now = datetime!(2026-01-14 10:00 UTC); // a Wednesday, in hours
///
/// let outcome = evaluate(&rule, &RuleState::default(), Trip::Tripped, now);
///
/// assert_eq!(outcome.status, Status::Critical);
/// assert!(outcome.escalate);
/// assert_eq!(
///     outcome.state.as_ref().and_then(|s| s.last_alarm_at),
///     Some(now),
///     "the cooldown starts now"
/// );
/// ```
///
/// Returning to healthy clears the state, so the next trip fires at once:
///
/// ```
/// use dashboard::alarm::{evaluate, RuleConfig, RuleState, Status, Trip};
/// use time::macros::datetime;
///
/// let rule = RuleConfig::for_test("phw-to-mpi");
/// let now = datetime!(2026-01-14 10:00 UTC);
/// let held = RuleState {
///     last_alarm_at: Some(datetime!(2026-01-14 09:50 UTC)),
///     ..RuleState::default()
/// };
///
/// let outcome = evaluate(&rule, &held, Trip::Clear, now);
///
/// assert_eq!(outcome.status, Status::Healthy);
/// assert_eq!(outcome.state, None, "the entry is deleted, not kept");
/// ```
#[must_use]
pub fn evaluate(rule: &RuleConfig, state: &RuleState, trip: Trip, now: OffsetDateTime) -> Outcome {
    // Step 2 — a live pause wins over everything below it. The operator
    // asked for silence; giving them a `critical` row instead would be
    // ignoring the request.
    if let Some(until) = state.paused_until
        && until > now
    {
        return Outcome {
            status: Status::Paused,
            state: Some(state.clone()),
            escalate: false,
            cooldown_remaining: None,
            pause_remaining: Some(minutes_between(now, until)),
        };
    }

    // Step 2, second half — a lapsed pause is cleared rather than left to
    // linger. Carrying it forward would leave a `pause_reason` on the page
    // long after the pause ended.
    let mut state = state.clone();
    state.paused_until = None;
    state.pause_reason = None;

    match trip {
        // Step 3 — nothing was measured, so nothing can be concluded. Not
        // `Healthy`: §4.5 makes a failed query look exactly like a quiet
        // flow otherwise.
        Trip::NoData => Outcome {
            status: Status::Unknown,
            state: store(state),
            escalate: false,
            cooldown_remaining: None,
            pause_remaining: None,
        },

        // Step 4 — below the threshold, and the state is **cleared** so the
        // next trip fires immediately rather than being swallowed by a
        // cooldown left over from the last one.
        Trip::Clear => Outcome {
            status: Status::Healthy,
            state: None,
            escalate: false,
            cooldown_remaining: None,
            pause_remaining: None,
        },

        Trip::Tripped => {
            let gap = f64::from(rule.alerting_gap_minutes);

            match state.last_alarm_at {
                // Step 5 — first trip: escalate and start the cooldown.
                None => {
                    state.last_alarm_at = Some(now);
                    Outcome {
                        status: Status::Critical,
                        state: Some(state),
                        escalate: rule.may_email(period_at(now)),
                        cooldown_remaining: None,
                        pause_remaining: None,
                    }
                }
                Some(last) => {
                    let since = minutes_between(last, now);
                    if since >= gap {
                        // Step 6, past the gap — escalate again.
                        state.last_alarm_at = Some(now);
                        Outcome {
                            status: Status::Critical,
                            state: Some(state),
                            escalate: rule.may_email(period_at(now)),
                            cooldown_remaining: None,
                            pause_remaining: None,
                        }
                    } else {
                        // Step 6, within the gap — still in alarm, still
                        // shown, but not e-mailed again.
                        Outcome {
                            status: Status::Suppressed,
                            state: Some(state),
                            escalate: false,
                            cooldown_remaining: Some(gap - since),
                            pause_remaining: None,
                        }
                    }
                }
            }
        }
    }
}

/// The state to store, or `None` when there is nothing worth keeping.
///
/// An empty state is deleted rather than written back as an empty
/// document, so a lapsed pause leaves nothing for the next evaluation to
/// read (§5.1.2 step 2).
fn store(state: RuleState) -> Option<RuleState> {
    (!state.is_empty()).then_some(state)
}

/// Minutes from `from` to `to`, which may be negative.
fn minutes_between(from: OffsetDateTime, to: OffsetDateTime) -> f64 {
    (to - from).as_seconds_f64() / 60.0
}

/// Format a duration in minutes the way the Python does.
///
/// Used in the page and in the escalation e-mail, so the wording is part of
/// what an operator reads at 3am.
///
/// # Examples
///
/// ```
/// use dashboard::alarm::format_duration;
///
/// assert_eq!(format_duration(0.5), "< 1 minute");
/// assert_eq!(format_duration(1.0), "1 minute");
/// assert_eq!(format_duration(45.0), "45 minutes");
/// assert_eq!(format_duration(150.0), "2.5 hours");
/// assert_eq!(format_duration(60.0), "1.0 hour");
/// assert_eq!(format_duration(2880.0), "2.0 days");
/// ```
#[must_use]
pub fn format_duration(minutes: f64) -> String {
    if minutes < 1.0 {
        return "< 1 minute".to_string();
    }
    if minutes < 60.0 {
        // Truncated, as the Python's `int()` is.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = minutes as u64;
        return format!("{whole} minute{}", if whole == 1 { "" } else { "s" });
    }

    let hours = minutes / 60.0;
    if hours < 24.0 {
        let rendered = format!("{hours:.1}");
        return format!(
            "{rendered} hour{}",
            if rendered == "1.0" { "" } else { "s" }
        );
    }

    let days = hours / 24.0;
    let rendered = format!("{days:.1}");
    format!("{rendered} day{}", if rendered == "1.0" { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    /// A Wednesday at 10:00 UTC — inside the Day window, so the
    /// out-of-hours guard is not what these tests are measuring.
    const IN_HOURS: OffsetDateTime = datetime!(2026-01-14 10:00 UTC);

    fn rule() -> RuleConfig {
        RuleConfig::for_test("phw-to-mpi")
    }

    fn emailing_rule() -> RuleConfig {
        RuleConfig {
            email_alerts_enabled: true,
            ..rule()
        }
    }

    #[test]
    fn a_first_trip_is_critical_and_escalates() {
        // spec/15-dashboard.md §5.1.2 step 5.
        let outcome = evaluate(
            &emailing_rule(),
            &RuleState::default(),
            Trip::Tripped,
            IN_HOURS,
        );

        assert_eq!(outcome.status, Status::Critical);
        assert!(outcome.escalate);
        assert_eq!(
            outcome.state.and_then(|s| s.last_alarm_at),
            Some(IN_HOURS),
            "the cooldown starts now"
        );
    }

    #[test]
    fn a_second_trip_within_the_gap_is_suppressed() {
        // §5.1.2 step 6. Still in alarm and still on the page — only the
        // e-mail is held back.
        let state = RuleState {
            last_alarm_at: Some(IN_HOURS - time::Duration::minutes(30)),
            ..RuleState::default()
        };

        let outcome = evaluate(&emailing_rule(), &state, Trip::Tripped, IN_HOURS);

        assert_eq!(outcome.status, Status::Suppressed);
        assert!(!outcome.escalate);
        assert!(outcome.status.is_in_alarm());
        assert_eq!(outcome.cooldown_remaining, Some(30.0));
        assert_eq!(
            outcome.state.and_then(|s| s.last_alarm_at),
            state.last_alarm_at,
            "the original alarm time is kept, not refreshed"
        );
    }

    #[test]
    fn the_cooldown_boundary_is_at_or_past_the_gap() {
        // §5.1.2 step 6 — "past that gap". The default is 60 minutes.
        let at_gap = RuleState {
            last_alarm_at: Some(IN_HOURS - time::Duration::minutes(60)),
            ..RuleState::default()
        };
        assert_eq!(
            evaluate(&emailing_rule(), &at_gap, Trip::Tripped, IN_HOURS).status,
            Status::Critical,
            "exactly 60 minutes re-fires"
        );

        let just_inside = RuleState {
            last_alarm_at: Some(IN_HOURS - time::Duration::seconds(59 * 60 + 59)),
            ..RuleState::default()
        };
        assert_eq!(
            evaluate(&emailing_rule(), &just_inside, Trip::Tripped, IN_HOURS).status,
            Status::Suppressed
        );
    }

    #[test]
    fn a_re_fire_restamps_the_cooldown() {
        let state = RuleState {
            last_alarm_at: Some(IN_HOURS - time::Duration::minutes(90)),
            ..RuleState::default()
        };

        let outcome = evaluate(&emailing_rule(), &state, Trip::Tripped, IN_HOURS);

        assert_eq!(outcome.status, Status::Critical);
        assert!(outcome.escalate);
        assert_eq!(outcome.state.and_then(|s| s.last_alarm_at), Some(IN_HOURS));
    }

    #[test]
    fn returning_to_healthy_clears_the_state() {
        // §5.1.2 step 4 — THE clause that makes the next incident fire
        // immediately. Keeping the cooldown would swallow the first alarm
        // of the next outage if it happened within the hour.
        let state = RuleState {
            last_alarm_at: Some(IN_HOURS - time::Duration::minutes(5)),
            ..RuleState::default()
        };

        let outcome = evaluate(&rule(), &state, Trip::Clear, IN_HOURS);

        assert_eq!(outcome.status, Status::Healthy);
        assert_eq!(outcome.state, None, "the entry is deleted");

        // And so the next trip is a first trip.
        let next = evaluate(
            &emailing_rule(),
            &RuleState::default(),
            Trip::Tripped,
            IN_HOURS,
        );
        assert_eq!(next.status, Status::Critical);
        assert!(next.escalate);
    }

    #[test]
    fn no_data_is_unknown_and_never_escalates() {
        // §5.1.2 step 3 and §4.5. A broken query must not page anybody, and
        // must not read as healthy either.
        let outcome = evaluate(
            &emailing_rule(),
            &RuleState::default(),
            Trip::NoData,
            IN_HOURS,
        );

        assert_eq!(outcome.status, Status::Unknown);
        assert!(!outcome.escalate);
        assert!(!outcome.status.is_in_alarm());
    }

    #[test]
    fn a_live_pause_wins_over_everything() {
        // §5.1.2 step 2. The operator asked for silence.
        let state = RuleState {
            paused_until: Some(IN_HOURS + time::Duration::minutes(45)),
            pause_reason: Some("planned maintenance".to_string()),
            last_alarm_at: Some(IN_HOURS - time::Duration::hours(3)),
        };

        let outcome = evaluate(&emailing_rule(), &state, Trip::Tripped, IN_HOURS);

        assert_eq!(outcome.status, Status::Paused);
        assert!(!outcome.escalate);
        assert_eq!(outcome.pause_remaining, Some(45.0));
        assert_eq!(
            outcome
                .state
                .as_ref()
                .and_then(|s| s.pause_reason.as_deref()),
            Some("planned maintenance"),
            "the reason is kept while the pause is live"
        );
    }

    #[test]
    fn a_lapsed_pause_is_cleared_and_evaluation_continues() {
        // §5.1.2 step 2's second half. A pause that lingered would leave a
        // stale reason on the page, and — worse — an operator believing a
        // rule was silenced when it was not.
        let state = RuleState {
            paused_until: Some(IN_HOURS - time::Duration::minutes(1)),
            pause_reason: Some("expired".to_string()),
            last_alarm_at: None,
        };

        let outcome = evaluate(&emailing_rule(), &state, Trip::Tripped, IN_HOURS);

        assert_eq!(outcome.status, Status::Critical, "evaluation continued");
        assert!(outcome.escalate);

        let stored = outcome.state.expect("a firing rule stores its cooldown");
        assert_eq!(stored.paused_until, None);
        assert_eq!(stored.pause_reason, None);
    }

    #[test]
    fn a_lapsed_pause_on_a_healthy_rule_leaves_nothing_behind() {
        let state = RuleState {
            paused_until: Some(IN_HOURS - time::Duration::hours(2)),
            pause_reason: Some("expired".to_string()),
            last_alarm_at: None,
        };

        let outcome = evaluate(&rule(), &state, Trip::Clear, IN_HOURS);

        assert_eq!(outcome.status, Status::Healthy);
        assert_eq!(outcome.state, None);
    }

    #[test]
    fn the_pause_boundary_is_strictly_in_the_future() {
        // A pause expiring exactly now is over.
        let exactly_now = RuleState {
            paused_until: Some(IN_HOURS),
            ..RuleState::default()
        };
        assert_ne!(
            evaluate(&rule(), &exactly_now, Trip::Tripped, IN_HOURS).status,
            Status::Paused
        );

        let a_second_away = RuleState {
            paused_until: Some(IN_HOURS + time::Duration::seconds(1)),
            ..RuleState::default()
        };
        assert_eq!(
            evaluate(&rule(), &a_second_away, Trip::Tripped, IN_HOURS).status,
            Status::Paused
        );
    }

    #[test]
    fn the_out_of_hours_guard_suppresses_the_email_not_the_alarm() {
        // §5.2.1 and §9.1 — the clause the Python configures and never
        // enforces.
        let quiet_overnight = RuleConfig {
            email_alerts_enabled: true,
            email_ooh_enabled: false,
            ..rule()
        };

        // A Wednesday at 20:00 UTC: Evening.
        let evening = datetime!(2026-01-14 20:00 UTC);
        let outcome = evaluate(
            &quiet_overnight,
            &RuleState::default(),
            Trip::Tripped,
            evening,
        );

        assert_eq!(
            outcome.status,
            Status::Critical,
            "the alarm still shows on the page"
        );
        assert!(!outcome.escalate, "but no e-mail is sent");

        // And the cooldown still starts, so it does not e-mail the instant
        // office hours begin either.
        assert_eq!(outcome.state.and_then(|s| s.last_alarm_at), Some(evening));
    }

    #[test]
    fn a_rule_allowed_out_of_hours_escalates_at_any_time() {
        let always = RuleConfig {
            email_alerts_enabled: true,
            email_ooh_enabled: true,
            ..rule()
        };

        for at in [
            datetime!(2026-01-14 10:00 UTC), // Wednesday, Day
            datetime!(2026-01-14 20:00 UTC), // Wednesday, Evening
            datetime!(2026-01-17 12:00 UTC), // Saturday, Weekend
        ] {
            let outcome = evaluate(&always, &RuleState::default(), Trip::Tripped, at);
            assert!(outcome.escalate, "{at}");
        }
    }

    #[test]
    fn email_off_beats_the_out_of_hours_toggle() {
        // §5.2 — `email_ooh_enabled` is forced false when alerts are off,
        // and this holds even if a stale document says otherwise.
        let stale = RuleConfig {
            email_alerts_enabled: false,
            email_ooh_enabled: true,
            ..rule()
        };

        for period in [Period::Day, Period::Evening, Period::Weekend] {
            assert!(!stale.may_email(period), "{period:?}");
        }
    }

    #[test]
    fn a_disabled_or_deleted_rule_is_not_active() {
        // §5.1.2 step 1.
        assert!(rule().is_active());

        assert!(
            !RuleConfig {
                alarm_enabled: false,
                ..rule()
            }
            .is_active()
        );
        assert!(
            !RuleConfig {
                deleted: true,
                ..rule()
            }
            .is_active()
        );
    }

    #[test]
    fn statuses_sort_the_way_the_page_reads() {
        // §5.1.4.
        let mut statuses = vec![
            Status::Healthy,
            Status::Unknown,
            Status::Suppressed,
            Status::Critical,
            Status::Paused,
        ];
        statuses.sort_unstable();

        assert_eq!(
            statuses,
            vec![
                Status::Paused,
                Status::Critical,
                Status::Suppressed,
                Status::Unknown,
                Status::Healthy,
            ]
        );
    }

    #[test]
    fn durations_are_worded_the_way_the_python_words_them() {
        // The escalation e-mail and the page both use this, so it is what
        // an operator reads at 3am.
        assert_eq!(format_duration(0.0), "< 1 minute");
        assert_eq!(format_duration(0.9), "< 1 minute");
        assert_eq!(format_duration(1.0), "1 minute");
        assert_eq!(format_duration(1.9), "1 minute", "truncated, not rounded");
        assert_eq!(format_duration(2.0), "2 minutes");
        assert_eq!(format_duration(59.9), "59 minutes");
        assert_eq!(format_duration(60.0), "1.0 hour");
        assert_eq!(format_duration(90.0), "1.5 hours");
        assert_eq!(format_duration(150.0), "2.5 hours");
        assert_eq!(format_duration(1439.0), "24.0 hours");
        assert_eq!(format_duration(1440.0), "1.0 day");
        assert_eq!(format_duration(2880.0), "2.0 days");
    }

    #[test]
    fn a_zero_alerting_gap_re_fires_every_evaluation() {
        // A legal configuration, and the one an operator picks when they
        // want to be told every cycle. It must not divide by anything.
        let eager = RuleConfig {
            alerting_gap_minutes: 0,
            email_alerts_enabled: true,
            ..rule()
        };
        let state = RuleState {
            last_alarm_at: Some(IN_HOURS),
            ..RuleState::default()
        };

        let outcome = evaluate(&eager, &state, Trip::Tripped, IN_HOURS);

        assert_eq!(outcome.status, Status::Critical);
        assert!(outcome.escalate);
    }
}
