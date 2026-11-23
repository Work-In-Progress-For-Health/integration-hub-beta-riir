//! Signals, graceful shutdown, and the retry schedule every worker shares.
//!
//! Implements `spec/01-architecture.md` §1.3, `spec/03-message-bus.md` §5.4,
//! and `spec/04-observability.md` §4.7.
//!
//! Every background worker in the Integration Hub has the same skeleton:
//! receive a batch, process it, dispose of it, repeat until told to stop,
//! and wait a bit longer each time something goes wrong. This crate is that
//! skeleton, minus the bus itself — which lives in `ihb-bus`, so that the
//! parts a test needs to drive are separable from the parts that need a
//! broker.
//!
//! # This crate is a facade
//!
//! The implementation lives in [`graceful_worker`], which was extracted
//! from this workspace and published: cooperative shutdown and an
//! exponential retry schedule are not Integration Hub inventions. What
//! stays here is which clauses they implement, and the schedule
//! `spec/03-message-bus.md` §5.4.1 fixes — five seconds, doubling to
//! fifteen minutes — which the general crate takes as a parameter and this
//! workspace does not.
//!
//! # Modules
//!
//! - [`shutdown`] — noticing `SIGTERM` and stopping between messages rather
//!   than during one.
//! - [`backoff`] — how long a failing receive waits before trying again.
//!
//! # The rule these two exist to protect
//!
//! **A message that has been acknowledged must have been forwarded.** Both
//! modules are shaped by it: shutdown is cooperative so a stop cannot land
//! mid-message, and a worker waits for a retry through
//! [`shutdown::Watcher::sleep`], so a stop cuts the wait short rather than
//! outlasting the platform's grace period.
//!
//! # Examples
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! use ihb_runtime::{backoff::Backoff, shutdown::Shutdown};
//!
//! let shutdown = Shutdown::new();
//! shutdown.listen_for_signals();
//!
//! let watcher = shutdown.watcher();
//! let mut backoff = Backoff::new();
//!
//! # shutdown.stop();
//! while watcher.is_running() {
//!     // ... receive and process a batch ...
//!     # break;
//! }
//! # let _ = backoff.failed();
//! # }
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]

/// Float arithmetic that answers `None` instead of `inf` or `NaN`.
///
/// Implements `rust-checked-arithmetic` RCA-FLT-2 (`spec/17-testing-and-quality.md`
/// §2.4b): a float operation does not panic, it yields `inf` or `NaN`, which
/// then spreads through every later figure. Every float result that is
/// compared, stored or shown goes through one of these, and a `None` is an
/// error or "unknown" at the caller, never a number (RCA-FLT-4).
///
/// # Examples
///
/// ```
/// use ihb_runtime::float;
///
/// assert_eq!(float::add(1.5, 2.0), Some(3.5));
/// assert_eq!(float::div(1.0, 0.0), None);
/// assert_eq!(float::finite(f64::NAN), None);
/// ```
pub mod float {
    /// `value` when it is finite.
    #[must_use]
    pub fn finite(value: f64) -> Option<f64> {
        value.is_finite().then_some(value)
    }

    /// `a + b`, when both are finite and so is the sum.
    #[must_use]
    #[expect(clippy::float_arithmetic, reason = "RCA-FLT-2: the result is checked")]
    pub fn add(a: f64, b: f64) -> Option<f64> {
        finite(a)?;
        finite(b)?;
        finite(a + b)
    }

    /// `a - b`, when both are finite and so is the difference.
    #[must_use]
    #[expect(clippy::float_arithmetic, reason = "RCA-FLT-2: the result is checked")]
    pub fn sub(a: f64, b: f64) -> Option<f64> {
        finite(a)?;
        finite(b)?;
        finite(a - b)
    }

    /// `a * b`, when both are finite and so is the product.
    #[must_use]
    #[expect(clippy::float_arithmetic, reason = "RCA-FLT-2: the result is checked")]
    pub fn mul(a: f64, b: f64) -> Option<f64> {
        finite(a)?;
        finite(b)?;
        finite(a * b)
    }

    /// `a / b`, when both are finite, `b` is not zero, and the quotient is finite.
    #[must_use]
    #[expect(clippy::float_arithmetic, reason = "RCA-FLT-2: the result is checked")]
    pub fn div(a: f64, b: f64) -> Option<f64> {
        finite(a)?;
        finite(b)?;
        if b == 0.0 {
            return None;
        }
        finite(a / b)
    }
}

/// How long a failing receive waits before trying again.
///
/// Implements `spec/03-message-bus.md` §5.4.
///
/// The schedule is the Python's, and it is the Python's for a reason worth
/// knowing: the flows are session-ordered, so a message that cannot be
/// processed blocks every message behind it in its flow. Retrying too
/// eagerly turns one poison message into a hot loop against Service Bus;
/// retrying too lazily stalls a clinical feed. Five seconds, doubling to a
/// fifteen-minute ceiling, is the compromise already in production, and
/// changing it would change how long a stalled flow stays stalled.
pub mod backoff {
    use std::time::Duration;

    pub use graceful_worker::Backoff;

    /// The wait before the first retry.
    pub const INITIAL_DELAY: Duration = graceful_worker::backoff::DEFAULT_INITIAL_DELAY;

    /// The longest this will ever wait between attempts.
    ///
    /// `spec/03-message-bus.md` §5.4.1 states this as 900 seconds; fifteen
    /// minutes is the same quantity said the way an operator reads it.
    pub const MAX_DELAY: Duration = graceful_worker::backoff::DEFAULT_MAX_DELAY;

    /// The longest a single receive call blocks waiting for a message.
    ///
    /// Also the longest slice a sliced wait sleeps in, so a shutdown signal
    /// is never more than this away from being noticed
    /// (`spec/03-message-bus.md` §5.4.3).
    pub const MAX_WAIT: Duration = graceful_worker::backoff::DEFAULT_SLICE;
}

/// Noticing `SIGTERM`, and stopping without losing a message.
///
/// Implements `spec/01-architecture.md` §1.3 and `spec/07-hl7-server.md` §8.
///
/// Azure Container Apps stops a container by sending `SIGTERM` and then
/// waiting a fixed grace period before `SIGKILL`. Everything in that window
/// is the process's own responsibility, and for this estate the requirement
/// is sharp: **a message that has been acknowledged must have been
/// forwarded**. Shutting down in the middle of one is how that promise gets
/// broken, so shutdown is cooperative — a signal asks loops to stop between
/// messages, rather than cancelling whatever is in flight.
pub mod shutdown {
    pub use graceful_worker::{Shutdown, Watcher};
}

pub use backoff::Backoff;
pub use shutdown::{Shutdown, Watcher};

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::float;

    /// RCA-FLT-2: a figure that is not finite is `None`, never a number.
    #[test]
    fn float_results_that_are_not_finite_are_refused() {
        assert_eq!(float::add(1.0, 2.0), Some(3.0));
        assert_eq!(float::sub(1.0, 3.0), Some(-2.0));
        assert_eq!(float::mul(2.0, 4.0), Some(8.0));
        assert_eq!(float::div(9.0, 3.0), Some(3.0));
        assert_eq!(float::div(1.0, 0.0), None);
        assert_eq!(float::div(0.0, 0.0), None);
        assert_eq!(float::add(f64::MAX, f64::MAX), None);
        assert_eq!(float::mul(f64::MAX, 2.0), None);
        assert_eq!(float::sub(f64::INFINITY, 1.0), None);
        assert_eq!(float::add(f64::NAN, 1.0), None);
        assert_eq!(float::finite(f64::NEG_INFINITY), None);
    }

    use super::backoff::{Backoff, INITIAL_DELAY, MAX_DELAY, MAX_WAIT};
    use super::shutdown::Shutdown;

    #[test]
    fn the_schedule_is_the_one_the_specification_fixes() {
        // spec/03-message-bus.md §5.4.1. The upstream crate takes these as
        // parameters; this workspace does not, and this is what notices if
        // an upstream default ever moves.
        assert_eq!(INITIAL_DELAY, Duration::from_secs(5));
        // Deliberately the specification's own number and unit, not a
        // restatement of it: §5.4.1 says "900 seconds", and this line is
        // what fails if that changes.
        #[allow(
            clippy::duration_suboptimal_units,
            reason = "citing the specification verbatim is the point"
        )]
        let ceiling = Duration::from_secs(900);
        assert_eq!(MAX_DELAY, ceiling);
        assert_eq!(MAX_WAIT, Duration::from_secs(60));

        assert_eq!(Backoff::new().delay(), INITIAL_DELAY);
    }

    #[test]
    fn the_delay_doubles_and_stops_at_the_ceiling() {
        // spec/03-message-bus.md §5.4.1
        let mut backoff = Backoff::new();
        let seen: Vec<u64> = (0..6).map(|_| backoff.failed().as_secs()).collect();
        assert_eq!(seen, [5, 10, 20, 40, 80, 160]);

        for _ in 0..50 {
            backoff.failed();
        }
        assert_eq!(backoff.delay(), MAX_DELAY);
    }

    #[test]
    fn any_success_resets_it_completely() {
        // spec/03-message-bus.md §5.4.2
        let mut backoff = Backoff::new();
        for _ in 0..10 {
            backoff.failed();
        }
        backoff.succeeded();
        assert_eq!(backoff.delay(), INITIAL_DELAY);
        assert_eq!(backoff.attempt(), 0);
    }

    #[tokio::test]
    async fn dropping_a_shutdown_does_not_stop_anything() {
        // spec/01-architecture.md §1.3 — shutdown must be deliberate, never
        // a scope accident.
        let watcher = {
            let shutdown = Shutdown::new();
            shutdown.watcher()
        };
        assert!(watcher.is_running());
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_wait_is_cut_short_by_a_stop() {
        // spec/03-message-bus.md §5.4.3 — the property the main loops rely
        // on: they wait through `watcher.sleep(backoff.delay())`, so a
        // fifteen-minute backoff cannot outlast the grace period.
        let shutdown = Shutdown::new();
        let watcher = shutdown.watcher();

        let sleeping = tokio::spawn(async move { watcher.sleep(MAX_DELAY).await });
        tokio::task::yield_now().await;
        shutdown.stop();

        assert!(!sleeping.await.expect("the sleeping task"));
    }
}
