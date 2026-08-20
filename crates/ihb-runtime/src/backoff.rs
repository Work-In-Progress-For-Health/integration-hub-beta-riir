//! How long a failing receive waits before trying again.
//!
//! Implements `spec/03-message-bus.md` §5.4.
//!
//! The schedule is the Python's, and it is the Python's for a reason worth
//! knowing: the flows are session-ordered, so a message that cannot be
//! processed blocks every message behind it in its flow. Retrying too
//! eagerly turns one poison message into a hot loop against Service Bus;
//! retrying too lazily stalls a clinical feed. Five seconds, doubling to a
//! fifteen-minute ceiling, is the compromise already in production, and
//! changing it would change how long a stalled flow stays stalled.

use std::time::Duration;

/// The wait before the first retry.
pub const INITIAL_DELAY: Duration = Duration::from_secs(5);

/// The longest this will ever wait between attempts.
///
/// `spec/03-message-bus.md` §5.4.1 states this as 900 seconds; fifteen
/// minutes is the same quantity said the way an operator reads it.
pub const MAX_DELAY: Duration = Duration::from_mins(15);

/// The longest a single receive call blocks waiting for a message.
///
/// Also the longest slice the backoff sleeps in, so a shutdown signal is
/// never more than this away from being noticed
/// (`spec/03-message-bus.md` §5.4.3).
pub const MAX_WAIT: Duration = Duration::from_secs(60);

/// The retry state of one receive loop.
///
/// A loop holds one of these, tells it when a batch succeeded or failed, and
/// asks it how long to wait. Nothing else in the loop needs to know about
/// doubling or ceilings.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use ihb_runtime::backoff::Backoff;
///
/// let mut backoff = Backoff::new();
/// assert_eq!(backoff.delay(), Duration::from_secs(5));
///
/// backoff.failed();
/// assert_eq!(backoff.delay(), Duration::from_secs(10));
/// backoff.failed();
/// assert_eq!(backoff.delay(), Duration::from_secs(20));
///
/// // Any success puts it straight back to the start.
/// backoff.succeeded();
/// assert_eq!(backoff.delay(), Duration::from_secs(5));
/// assert_eq!(backoff.attempt(), 0);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    /// What the next wait will be.
    delay: Duration,
    /// How many consecutive failures there have been.
    attempt: u32,
}

impl Backoff {
    /// A fresh backoff, at [`INITIAL_DELAY`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            delay: INITIAL_DELAY,
            attempt: 0,
        }
    }

    /// How long to wait before the next attempt.
    #[must_use]
    pub const fn delay(&self) -> Duration {
        self.delay
    }

    /// How many consecutive failures there have been.
    ///
    /// Reported as the `attempt` dimension on the `retry_delay_seconds`
    /// metric (`spec/04-observability.md` §3.1).
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Whether anything has failed since the last success.
    ///
    /// A loop uses this to decide whether an empty queue is unremarkable or
    /// worth the log line of `spec/03-message-bus.md` §5.4.5.
    #[must_use]
    pub const fn is_retrying(&self) -> bool {
        self.attempt > 0
    }

    /// Record a failure: double the delay, up to [`MAX_DELAY`].
    ///
    /// Returns the delay that was in force *for this failure*, which is what
    /// the metric records — not the doubled value the next one will use.
    pub const fn failed(&mut self) -> Duration {
        let current = self.delay;

        // Saturating rather than wrapping: at the ceiling this is a no-op,
        // and no arithmetic here can overflow into a tiny delay.
        let doubled = self.delay.saturating_mul(2);
        self.delay = if doubled.as_secs() > MAX_DELAY.as_secs() {
            MAX_DELAY
        } else {
            doubled
        };
        self.attempt = self.attempt.saturating_add(1);

        current
    }

    /// Record a success: back to [`INITIAL_DELAY`], attempt count zero.
    ///
    /// Any success resets it, not just a run of them
    /// (`spec/03-message-bus.md` §5.4.2).
    pub const fn succeeded(&mut self) {
        self.delay = INITIAL_DELAY;
        self.attempt = 0;
    }

    /// The slice to sleep for, given how much of the delay is left.
    ///
    /// Capped at [`MAX_WAIT`] so a fifteen-minute backoff does not make a
    /// container take fifteen minutes to notice `SIGTERM`
    /// (`spec/03-message-bus.md` §5.4.3).
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use ihb_runtime::backoff::Backoff;
    ///
    /// // A short remainder is slept in one go.
    /// assert_eq!(Backoff::sleep_slice(Duration::from_secs(3)), Duration::from_secs(3));
    /// // A long one is sliced, so shutdown stays responsive.
    /// assert_eq!(Backoff::sleep_slice(Duration::from_secs(900)), Duration::from_secs(60));
    /// ```
    #[must_use]
    pub const fn sleep_slice(remaining: Duration) -> Duration {
        if remaining.as_secs() > MAX_WAIT.as_secs() {
            MAX_WAIT
        } else {
            remaining
        }
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_delay_is_five_seconds() {
        // spec/03-message-bus.md §5.4.1
        assert_eq!(Backoff::new().delay(), Duration::from_secs(5));
    }

    #[test]
    fn the_delay_doubles_on_each_failure() {
        // spec/03-message-bus.md §5.4.1
        let mut backoff = Backoff::new();
        let seen: Vec<u64> = (0..6)
            .map(|_| {
                let used = backoff.failed();
                used.as_secs()
            })
            .collect();
        assert_eq!(seen, [5, 10, 20, 40, 80, 160]);
    }

    #[test]
    fn the_delay_stops_at_fifteen_minutes() {
        // spec/03-message-bus.md §5.4.1 — the ceiling.
        let mut backoff = Backoff::new();
        for _ in 0..50 {
            backoff.failed();
        }
        assert_eq!(backoff.delay(), MAX_DELAY);
        // Deliberately the spec's own number and unit, not a restatement of
        // the constant's arithmetic: `spec/03-message-bus.md` §5.4.1 says
        // "900 seconds", and this line is what fails if that changes.
        #[allow(
            clippy::duration_suboptimal_units,
            reason = "citing the specification verbatim is the point"
        )]
        let ceiling = Duration::from_secs(900);
        assert_eq!(backoff.delay(), ceiling);
    }

    #[test]
    fn the_ceiling_is_not_overshot_on_the_way_up() {
        // The delay after each failure: 10, 20, 40, 80, 160, 320, 640,
        // and then a doubling that would reach 1280.
        let mut backoff = Backoff::new();
        for _ in 0..7 {
            backoff.failed();
        }
        assert_eq!(backoff.delay(), Duration::from_secs(640));
        backoff.failed();
        assert_eq!(backoff.delay(), MAX_DELAY, "clamped rather than 1280");
    }

    #[test]
    fn any_success_resets_it_completely() {
        // spec/03-message-bus.md §5.4.2
        let mut backoff = Backoff::new();
        for _ in 0..10 {
            backoff.failed();
        }
        assert!(backoff.is_retrying());

        backoff.succeeded();

        assert_eq!(backoff.delay(), INITIAL_DELAY);
        assert_eq!(backoff.attempt(), 0);
        assert!(!backoff.is_retrying());
    }

    #[test]
    fn the_attempt_count_is_what_the_metric_reports() {
        // spec/04-observability.md §3.1 — the `attempt` dimension.
        let mut backoff = Backoff::new();
        assert_eq!(backoff.attempt(), 0);
        backoff.failed();
        assert_eq!(backoff.attempt(), 1);
        backoff.failed();
        assert_eq!(backoff.attempt(), 2);
    }

    #[test]
    fn failed_reports_the_delay_that_applied_not_the_next_one() {
        // The metric records what this failure waits, so a chart of
        // retry_delay_seconds shows the actual schedule.
        let mut backoff = Backoff::new();
        assert_eq!(backoff.failed(), Duration::from_secs(5));
        assert_eq!(backoff.delay(), Duration::from_secs(10));
    }

    #[test]
    fn a_long_wait_is_slept_in_slices() {
        // spec/03-message-bus.md §5.4.3 — so SIGTERM is noticed promptly.
        assert_eq!(Backoff::sleep_slice(MAX_DELAY), MAX_WAIT);
        assert_eq!(
            Backoff::sleep_slice(Duration::from_secs(61)),
            Duration::from_secs(60)
        );
        assert_eq!(
            Backoff::sleep_slice(Duration::from_secs(60)),
            Duration::from_secs(60)
        );
        assert_eq!(
            Backoff::sleep_slice(Duration::from_secs(1)),
            Duration::from_secs(1)
        );
        assert_eq!(Backoff::sleep_slice(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn a_default_backoff_is_a_new_one() {
        assert_eq!(Backoff::default(), Backoff::new());
    }
}
