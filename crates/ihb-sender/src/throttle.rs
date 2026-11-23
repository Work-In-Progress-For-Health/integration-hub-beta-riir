//! Rate limiting, and the batch size it forces.
//!
//! Implements `spec/10-senders.md` §2.3 and §2.4.
//!
//! # The subtle part
//!
//! §2.4.2, and it is worth stating before the code: when a sender has been
//! idle, the next message is **not** sent against the overdue schedule. The
//! target is reset to *now*.
//!
//! Without that, a sender idle for ten minutes at sixty messages a minute
//! would owe six hundred messages, and would emit them as fast as it could
//! receive them — a burst into a downstream system that the throttle exists
//! to protect. The recorded time is the **target**, not the actual send
//! time, which is what makes the difference.

use std::time::Duration;

use tokio::time::Instant;

/// The lock-renewal window a batch must fit inside
/// (`spec/03-message-bus.md` §5.1.2, `LOCK_RENEWAL_DURATION_SECONDS`).
pub const LOCK_RENEWAL_DURATION: Duration = Duration::from_secs(300);

/// The margin left inside that window for the work either side of the
/// sends themselves.
pub const RENEWAL_MARGIN: Duration = Duration::from_secs(30);

/// A rate limit, or the absence of one.
///
/// # Examples
///
/// ```
/// use ihb_sender::throttle::Throttle;
///
/// // Sixty a minute is one a second.
/// let throttle = Throttle::per_minute(Some(60)).unwrap();
/// assert_eq!(throttle.interval(), Some(std::time::Duration::from_secs(1)));
///
/// // Absent means unthrottled.
/// assert_eq!(Throttle::per_minute(None).unwrap().interval(), None);
/// ```
///
/// A configured zero is a startup failure (§2.4.3): it asks for no messages
/// at all, which is a mistake rather than an instruction.
///
/// ```
/// use ihb_sender::throttle::Throttle;
///
/// assert!(Throttle::per_minute(Some(0)).is_err());
/// ```
#[derive(Debug)]
pub struct Throttle {
    /// The gap between sends, or `None` when unthrottled.
    interval: Option<Duration>,
    /// When the last message was *scheduled* for — not when it was sent.
    last_target: Option<Instant>,
}

/// A rate that cannot be honoured: `MAX_MESSAGES_PER_MINUTE=0` would mean a
/// division by zero for the interval, and "never send" is not a throttle but
/// a misconfiguration (`spec/10-senders.md` §2.4.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("MAX_MESSAGES_PER_MINUTE must be greater than zero")]
pub struct InvalidRate;

impl Throttle {
    /// A throttle at `messages` a minute, or unthrottled when `None`.
    ///
    /// # Errors
    ///
    /// [`InvalidRate`] for a configured zero (§2.4.3).
    pub fn per_minute(messages: Option<u32>) -> Result<Self, InvalidRate> {
        let interval = match messages {
            None => None,
            Some(0) => return Err(InvalidRate),
            Some(rate) => Some(Duration::from_secs_f64(60.0 / f64::from(rate))),
        };

        Ok(Self {
            interval,
            last_target: None,
        })
    }

    /// The gap between sends.
    #[must_use]
    pub const fn interval(&self) -> Option<Duration> {
        self.interval
    }

    /// Wait for this message's slot.
    ///
    /// The first message is never delayed (§2.4.1).
    pub async fn acquire(&mut self) {
        let Some(interval) = self.interval else {
            return;
        };

        let now = Instant::now();
        // §2.4.1 — the first message goes at once. §2.4.2 — and if a later
        // slot has already passed, the target is *now*, not the overdue
        // moment, which is what stops an idle sender bursting to catch up.
        // A slot too far ahead to represent is not a future slot: it is
        // treated as passed, so the target is now (`§2.4.2`).
        let target = self
            .last_target
            .map_or(now, |last| match last.checked_add(interval) {
                Some(scheduled) if scheduled > now => scheduled,
                _ => now,
            });

        self.last_target = Some(target);

        if target > now {
            tokio::time::sleep_until(target).await;
        }
    }
}

/// The batch size a throttle allows (§2.3).
///
/// A whole batch has to be delivered inside the Service Bus lock-renewal
/// window, because the messages behind the one being sent are locked while
/// they wait. At one message a minute, a batch of a hundred would take an
/// hour and every lock in it would expire — so the batch is reduced to what
/// fits.
///
/// ```text
/// window    = 300 - 30 = 270 seconds
/// allowable = floor(window / interval) + 1
/// batch     = clamp(allowable, 1, MAX_BATCH_SIZE)
/// ```
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use ihb_sender::throttle::batch_size;
///
/// // Unthrottled: the configured size, untouched.
/// assert_eq!(batch_size(100, None), 100);
///
/// // One a second: 270 fit, so the configured 100 still stands.
/// assert_eq!(batch_size(100, Some(Duration::from_secs(1))), 100);
///
/// // One a minute: only five fit inside the window.
/// assert_eq!(batch_size(100, Some(Duration::from_secs(60))), 5);
///
/// // Slower than the window itself: one at a time, never zero.
/// assert_eq!(batch_size(100, Some(Duration::from_secs(600))), 1);
/// ```
#[must_use]
pub fn batch_size(configured: usize, interval: Option<Duration>) -> usize {
    let Some(interval) = interval else {
        return configured;
    };
    if interval.is_zero() {
        return configured;
    }

    let window = LOCK_RENEWAL_DURATION.saturating_sub(RENEWAL_MARGIN);
    // More than a `usize` of messages fit the window: no bound but the
    // configured one, which is the declared ceiling (`§2.4.3`).
    let allowable = whole_floor(window.as_secs_f64() / interval.as_secs_f64())
        .and_then(|whole| whole.checked_add(1))
        .unwrap_or(usize::MAX);

    // At least 1 (`allowable` is), at most what is configured.
    allowable.min(configured.max(1))
}

/// `value` floored, as a `usize`, if it is finite, not negative and exact in
/// an `f64` (below 2^53); otherwise `None` (RCA-CONV-3).
fn whole_floor(value: f64) -> Option<usize> {
    const EXACT_LIMIT: f64 = 9_007_199_254_740_992.0;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let floored = value.floor();
    if floored >= EXACT_LIMIT {
        return None;
    }
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "RCA-CONV-3: finite, non-negative, integral and below 2^53, checked above"
    )]
    Some(floored as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RCA-CONV-3 / the ceiling: a window that fits more messages than a
    /// `usize` can count is bounded by the configured size, never wrapped.
    #[test]
    fn a_ratio_that_does_not_fit_is_bounded_by_the_configuration() {
        assert_eq!(batch_size(500, Some(Duration::from_nanos(1))), 500);
        assert_eq!(batch_size(0, Some(Duration::from_nanos(1))), 1);
        assert_eq!(whole_floor(f64::NAN), None);
        assert_eq!(whole_floor(-1.0), None);
        assert_eq!(whole_floor(1e300), None);
        assert_eq!(whole_floor(7.9), Some(7));
    }

    #[test]
    fn the_interval_is_a_minute_divided_by_the_rate() {
        // spec/10-senders.md §2.4.
        assert_eq!(
            Throttle::per_minute(Some(60)).unwrap().interval(),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            Throttle::per_minute(Some(30)).unwrap().interval(),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            Throttle::per_minute(Some(1)).unwrap().interval(),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn zero_is_a_startup_failure_and_absent_is_unthrottled() {
        // §2.4.3. Zero asks for no messages at all, which is a mistake
        // rather than an instruction; absent asks for no limit.
        assert_eq!(Throttle::per_minute(Some(0)).unwrap_err(), InvalidRate);
        assert!(Throttle::per_minute(None).unwrap().interval().is_none());
    }

    #[test]
    fn every_boundary_of_the_batch_sizing_rule() {
        // §2.3 and §6.2.
        assert_eq!(batch_size(100, None), 100, "unthrottled");

        // A fast interval: more fit than are configured, so the
        // configuration wins.
        assert_eq!(batch_size(100, Some(Duration::from_millis(100))), 100);

        // 270 / 60 = 4, plus one.
        assert_eq!(batch_size(100, Some(Duration::from_secs(60))), 5);

        // Exactly the window: one, plus one.
        assert_eq!(batch_size(100, Some(Duration::from_secs(270))), 2);

        // Slower than the window: clamped to one, never zero — a batch of
        // zero would stall the sender entirely.
        assert_eq!(batch_size(100, Some(Duration::from_secs(600))), 1);
        assert_eq!(batch_size(100, Some(Duration::from_secs(10_000))), 1);
    }

    #[test]
    fn the_batch_is_never_zero_even_for_a_zero_configuration() {
        // A configured zero would otherwise clamp to zero and the sender
        // would receive nothing, forever, with no error.
        assert_eq!(batch_size(0, Some(Duration::from_secs(60))), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_message_is_not_delayed() {
        // §2.4.1.
        let mut throttle = Throttle::per_minute(Some(60)).unwrap();
        let started = Instant::now();

        throttle.acquire().await;

        assert_eq!(Instant::now(), started);
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_message_waits_the_remainder_of_the_interval() {
        // §2.4 and §6.3, on a virtual clock.
        let mut throttle = Throttle::per_minute(Some(60)).unwrap();
        let started = Instant::now();

        throttle.acquire().await;
        // A quarter of the interval spent working.
        tokio::time::sleep(Duration::from_millis(250)).await;
        throttle.acquire().await;

        assert_eq!(
            Instant::now().duration_since(started),
            Duration::from_secs(1),
            "the second send lands one interval after the first, not one \
             interval after the work finished"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_sender_does_not_burst_to_catch_up() {
        // §2.4.2 — THE test of this module. Without it, a sender idle for
        // ten minutes at one a second owes six hundred messages and emits
        // them as fast as it can receive them.
        let mut throttle = Throttle::per_minute(Some(60)).unwrap();

        throttle.acquire().await;
        tokio::time::sleep(Duration::from_secs(600)).await;

        // The slot is long overdue, so this one goes at once.
        let before = Instant::now();
        throttle.acquire().await;
        assert_eq!(Instant::now(), before, "the overdue message is not delayed");

        // And the one after it waits a full interval — the schedule has
        // been reset to now, not carried forward from ten minutes ago.
        throttle.acquire().await;
        assert_eq!(
            Instant::now().duration_since(before),
            Duration::from_secs(1),
            "the next one is spaced from now, not from the overdue schedule"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unthrottled_sender_never_waits() {
        let mut throttle = Throttle::per_minute(None).unwrap();
        let started = Instant::now();

        for _ in 0..1000 {
            throttle.acquire().await;
        }

        assert_eq!(Instant::now(), started);
    }
}
