//! Flow health.
//!
//! Implements `spec/15-dashboard.md` §3.
//!
//! # Unknown is not Healthy, and it is not zero
//!
//! §3.3 and §2.2.2. This is the whole reason the types here look the way
//! they do. A depth that could not be read is [`Option::None`], not `0`;
//! a flow whose data is missing is [`Health::Unknown`], not
//! [`Health::Healthy`].
//!
//! The failure this prevents is specific: Log Analytics has an outage, the
//! dashboard shows every queue at zero and every flow green, and an
//! operator concludes the estate is quiet on the night a feed has stopped.
//! Making the absent case a different value — rather than a default —
//! means that cannot be written by accident.

use crate::config::Thresholds;

/// A flow's health (§3.2).
///
/// Ordered worst-first, so [`Ord`] sorts a page the way an operator wants
/// to read it: the thing to look at is at the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Health {
    /// Something needs attention now.
    Critical,
    /// Something is heading that way.
    Warning,
    /// The data could not be retrieved (§3.3).
    ///
    /// Deliberately worse than [`Health::Healthy`] and better than
    /// [`Health::Warning`]: it is not evidence of a problem, and it is not
    /// evidence of the absence of one.
    Unknown,
    /// Nothing to do.
    Healthy,
}

impl Health {
    /// The label the page shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::Warning => "Warning",
            Self::Unknown => "Unknown",
            Self::Healthy => "Healthy",
        }
    }

    /// A CSS class, so the template does not carry a match.
    #[must_use]
    pub const fn css_class(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warning => "warning",
            Self::Unknown => "unknown",
            Self::Healthy => "healthy",
        }
    }
}

/// What is known about one queue.
///
/// Both depths are optional, and they are optional **separately**: the
/// management API can answer for a queue and not for its dead-letter
/// sub-queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueDepths {
    /// Messages on the queue, or `None` when it could not be read.
    pub active: Option<u64>,
    /// Messages on its dead-letter sub-queue, or `None`.
    pub dead_letter: Option<u64>,
}

impl QueueDepths {
    /// Both depths known.
    #[must_use]
    pub const fn known(active: u64, dead_letter: u64) -> Self {
        Self {
            active: Some(active),
            dead_letter: Some(dead_letter),
        }
    }

    /// Neither depth known — the queue could not be read.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            active: None,
            dead_letter: None,
        }
    }

    /// Whether anything at all is known.
    #[must_use]
    pub const fn is_known(self) -> bool {
        self.active.is_some() || self.dead_letter.is_some()
    }
}

/// What is known about one container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerState {
    /// Running, with the expected replicas.
    Running,
    /// Not running: stopped, failed, or scaled to zero when it should not
    /// be.
    NotRunning,
    /// Could not be read.
    Unknown,
}

/// Decide a flow's health (§3.2).
///
/// The table, in the order it is written — and the order matters, because
/// the conditions overlap. A queue that is both over the critical threshold
/// and has dead-letters is Critical either way, but a queue over the
/// critical threshold must not be reported as Warning just because the
/// warning row was tested first.
///
/// # Examples
///
/// ```
/// use dashboard::config::Thresholds;
/// use dashboard::health::{assess, ContainerState, Health, QueueDepths};
///
/// let thresholds = Thresholds::default(); // 10 / 50 / 1
///
/// // Quiet.
/// assert_eq!(
///     assess(QueueDepths::known(0, 0), &[ContainerState::Running], thresholds),
///     Health::Healthy
/// );
///
/// // One dead-letter is Critical, whatever else is true.
/// assert_eq!(
///     assess(QueueDepths::known(0, 1), &[ContainerState::Running], thresholds),
///     Health::Critical
/// );
///
/// // A queue that cannot be read is Unknown, NOT Healthy.
/// assert_eq!(
///     assess(QueueDepths::unknown(), &[ContainerState::Running], thresholds),
///     Health::Unknown
/// );
/// ```
#[must_use]
pub fn assess(
    depths: QueueDepths,
    containers: &[ContainerState],
    thresholds: Thresholds,
) -> Health {
    // §3.3 — nothing was retrieved, so nothing can be concluded. Tested
    // first: a flow with no data and a container that is also unreadable
    // is Unknown, not Critical.
    if !depths.is_known() && containers.iter().all(|c| *c == ContainerState::Unknown) {
        return Health::Unknown;
    }

    // §3.2's table, worst first.
    if depths
        .dead_letter
        .is_some_and(|count| count >= thresholds.dlq_warning)
    {
        return Health::Critical;
    }

    if depths
        .active
        .is_some_and(|count| count >= thresholds.queue_critical)
    {
        return Health::Critical;
    }

    if containers.contains(&ContainerState::NotRunning) {
        return Health::Critical;
    }

    if depths
        .active
        .is_some_and(|count| count >= thresholds.queue_warning)
    {
        return Health::Warning;
    }

    // Nothing tripped, but something is missing. Not Healthy: a flow whose
    // dead-letter depth could not be read might have a thousand messages on
    // it, and reporting green would be a claim nobody checked.
    if depths.active.is_none()
        || depths.dead_letter.is_none()
        || containers.contains(&ContainerState::Unknown)
    {
        return Health::Unknown;
    }

    Health::Healthy
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §9.2's defaults: 10 warning, 50 critical, 1 dead-letter.
    fn thresholds() -> Thresholds {
        Thresholds::default()
    }

    const RUNNING: [ContainerState; 1] = [ContainerState::Running];

    #[test]
    fn every_row_of_the_health_table() {
        // spec/15-dashboard.md §3.2 and §8.2.
        let t = thresholds();

        // Dead-letter depth >= DLQ_WARNING_THRESHOLD → Critical.
        assert_eq!(
            assess(QueueDepths::known(0, 1), &RUNNING, t),
            Health::Critical
        );

        // Active depth >= QUEUE_CRITICAL_THRESHOLD → Critical.
        assert_eq!(
            assess(QueueDepths::known(50, 0), &RUNNING, t),
            Health::Critical
        );

        // Active depth >= QUEUE_WARNING_THRESHOLD → Warning.
        assert_eq!(
            assess(QueueDepths::known(10, 0), &RUNNING, t),
            Health::Warning
        );

        // A container not running → Critical.
        assert_eq!(
            assess(QueueDepths::known(0, 0), &[ContainerState::NotRunning], t),
            Health::Critical
        );

        // Otherwise → Healthy.
        assert_eq!(
            assess(QueueDepths::known(0, 0), &RUNNING, t),
            Health::Healthy
        );
    }

    #[test]
    fn each_threshold_is_at_or_above_rather_than_above() {
        // §3.2 says "≥". One off here is the difference between an alarm
        // at 50 and an alarm at 51, and nobody would notice which.
        let t = thresholds();

        assert_eq!(
            assess(QueueDepths::known(9, 0), &RUNNING, t),
            Health::Healthy
        );
        assert_eq!(
            assess(QueueDepths::known(10, 0), &RUNNING, t),
            Health::Warning
        );

        assert_eq!(
            assess(QueueDepths::known(49, 0), &RUNNING, t),
            Health::Warning
        );
        assert_eq!(
            assess(QueueDepths::known(50, 0), &RUNNING, t),
            Health::Critical
        );

        assert_eq!(
            assess(QueueDepths::known(0, 0), &RUNNING, t),
            Health::Healthy
        );
        assert_eq!(
            assess(QueueDepths::known(0, 1), &RUNNING, t),
            Health::Critical
        );
    }

    #[test]
    fn a_flow_with_no_data_is_unknown_not_healthy() {
        // §3.3 and §2.2.2 — THE test of this file.
        //
        // The failure it prevents: Log Analytics has an outage, every queue
        // reads as zero, every flow shows green, and an operator concludes
        // the estate is quiet on the night a clinical feed has stopped.
        let t = thresholds();

        assert_eq!(
            assess(QueueDepths::unknown(), &[ContainerState::Unknown], t),
            Health::Unknown
        );
        assert_eq!(assess(QueueDepths::unknown(), &RUNNING, t), Health::Unknown);
    }

    #[test]
    fn a_half_known_queue_is_unknown_rather_than_judged_on_half_the_data() {
        // The management API can answer for a queue and not for its
        // dead-letter sub-queue. An empty queue with an unreadable
        // dead-letter depth might have a thousand messages parked on it.
        let t = thresholds();

        let no_dlq = QueueDepths {
            active: Some(0),
            dead_letter: None,
        };
        assert_eq!(assess(no_dlq, &RUNNING, t), Health::Unknown);

        let no_active = QueueDepths {
            active: None,
            dead_letter: Some(0),
        };
        assert_eq!(assess(no_active, &RUNNING, t), Health::Unknown);
    }

    #[test]
    fn a_tripped_threshold_beats_missing_data() {
        // Unknown means "cannot conclude", not "ignore what is known". A
        // dead-letter depth of 5 is Critical whether or not the active
        // depth came back.
        let t = thresholds();

        let dlq_only = QueueDepths {
            active: None,
            dead_letter: Some(5),
        };
        assert_eq!(
            assess(dlq_only, &[ContainerState::Unknown], t),
            Health::Critical
        );

        let active_only = QueueDepths {
            active: Some(100),
            dead_letter: None,
        };
        assert_eq!(assess(active_only, &RUNNING, t), Health::Critical);
    }

    #[test]
    fn a_stopped_container_beats_a_warning_depth() {
        // §3.2's order: the container row sits above the warning row, so a
        // flow with 10 messages and a dead container is Critical.
        let t = thresholds();

        assert_eq!(
            assess(
                QueueDepths::known(10, 0),
                &[ContainerState::Running, ContainerState::NotRunning],
                t
            ),
            Health::Critical
        );
    }

    #[test]
    fn custom_thresholds_are_honoured() {
        let t = Thresholds {
            queue_warning: 2,
            queue_critical: 4,
            dlq_warning: 10,
        };

        assert_eq!(
            assess(QueueDepths::known(1, 0), &RUNNING, t),
            Health::Healthy
        );
        assert_eq!(
            assess(QueueDepths::known(2, 0), &RUNNING, t),
            Health::Warning
        );
        assert_eq!(
            assess(QueueDepths::known(4, 0), &RUNNING, t),
            Health::Critical
        );

        // And a dead-letter depth below this deployment's threshold is not
        // Critical, though it would be at the default of 1.
        assert_eq!(
            assess(QueueDepths::known(0, 9), &RUNNING, t),
            Health::Healthy
        );
    }

    #[test]
    fn a_flow_with_no_containers_at_all_is_judged_on_its_queues() {
        // Some flows have no transformer (`spec/01-architecture.md` §2.1),
        // so an empty container list is normal and must not read as
        // Unknown.
        let t = thresholds();

        assert_eq!(assess(QueueDepths::known(0, 0), &[], t), Health::Healthy);
        assert_eq!(assess(QueueDepths::known(0, 1), &[], t), Health::Critical);
        assert_eq!(assess(QueueDepths::unknown(), &[], t), Health::Unknown);
    }

    #[test]
    fn health_sorts_worst_first() {
        // §5.1.4's ordering idea, applied to the flow table: an operator
        // reads the top of the page.
        let mut states = vec![
            Health::Healthy,
            Health::Unknown,
            Health::Critical,
            Health::Warning,
        ];
        states.sort_unstable();

        assert_eq!(
            states,
            vec![
                Health::Critical,
                Health::Warning,
                Health::Unknown,
                Health::Healthy
            ]
        );
    }

    #[test]
    fn every_state_has_a_label_and_a_class() {
        for state in [
            Health::Critical,
            Health::Warning,
            Health::Unknown,
            Health::Healthy,
        ] {
            assert!(!state.label().is_empty());
            assert_eq!(state.css_class(), state.label().to_lowercase());
        }
    }
}
