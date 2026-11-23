//! Keeping a session lock alive while a batch is processed.
//!
//! Implements `spec/03-message-bus.md` §5.1.3, §5.1.4 and §8.1.2.
//!
//! # Why this exists at all
//!
//! The Python used `azure.servicebus.AutoLockRenewer`. **`azservicebus` has
//! no equivalent** — renewal is manual, through `renew_session_lock` — so
//! this is the piece that has to be ours (§8.1.2).
//!
//! That turns out to be a small mercy. §5.1.4 requires the renewer to be
//! shut down *before* the receiver is closed, because closing it afterwards
//! races the receiver's teardown and surfaces in the Python as
//! `'NoneType' object has no attribute 'create_receiver_link'` — a nine-line
//! comment in `message_receiver_client.py` exists to explain that. With
//! renewal as our own task, the ordering is a `JoinHandle` to await rather
//! than a library's internal race, and [`LockRenewer::stop`] is the only way
//! to end it.
//!
//! # What it does not do
//!
//! It does not renew *message* locks, only the session's. Within a session
//! the session lock is what governs, and every pipeline queue is
//! session-enabled (`spec/01-architecture.md` §2.3.1). The message-store
//! queue is the exception and has no session to renew (§2.3.3).

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinHandle;

/// How long a Service Bus session lock lasts before it must be renewed.
///
/// The local emulator's `LockDuration` is `PT1M`; Azure's default is the
/// same. `spec/03-message-bus.md` §5.1.3 gives the renewal *budget* as 300
/// seconds, which is how long a batch may take in total — not how long a
/// lock lasts.
pub const LOCK_DURATION: Duration = Duration::from_secs(60);

/// How long before expiry a renewal is attempted.
///
/// A third of the lock duration, so two renewals can fail before the lock is
/// actually lost. Renewing at the last moment would mean a single slow round
/// trip drops the session, and losing a session mid-batch means the whole
/// batch is redelivered.
pub const RENEW_INTERVAL: Duration = Duration::from_secs(20);

/// The longest a batch may hold a session before it is given up on.
///
/// From `message_receiver_client.py :: LOCK_RENEWAL_DURATION_SECONDS`. A
/// batch still running after this has stopped making progress, and holding
/// the session forever would stall the flow behind it
/// (`spec/03-message-bus.md` §5.1.3).
pub const MAX_RENEWAL_DURATION: Duration = Duration::from_secs(300);

/// How many consecutive renewal failures are tolerated.
///
/// One is weather. Three in a row, at [`RENEW_INTERVAL`], means a minute has
/// passed without a successful renewal — by which point the lock is gone
/// anyway and continuing to try only hides that.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Why renewal stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// [`LockRenewer::stop`] was called: the batch finished.
    Stopped,
    /// The batch outlasted [`MAX_RENEWAL_DURATION`].
    Expired,
    /// Renewal failed [`MAX_CONSECUTIVE_FAILURES`] times in a row.
    Failed,
}

/// Renews a session lock in the background while a batch is processed.
///
/// Started when a batch begins and **stopped before the receiver is
/// closed** — that ordering is §5.1.4 and is the whole reason this is a
/// type rather than a loose task.
///
/// # Examples
///
/// ```
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// use std::sync::{Arc, atomic::{AtomicU32, Ordering}};
/// use ihb_bus::renew::{Ended, LockRenewer};
///
/// let renewals = Arc::new(AtomicU32::new(0));
/// let counter = Arc::clone(&renewals);
///
/// let renewer = LockRenewer::start(move || {
///     let counter = Arc::clone(&counter);
///     async move {
///         counter.fetch_add(1, Ordering::SeqCst);
///         true
///     }
/// });
///
/// // ... process the batch ...
///
/// assert_eq!(renewer.stop().await, Ended::Stopped);
/// # }
/// ```
#[derive(Debug)]
pub struct LockRenewer {
    /// The renewal task.
    ///
    /// An `Option` so `stop` can take it, which `Drop` otherwise forbids.
    task: Option<JoinHandle<Ended>>,
    /// Asks the task to finish.
    stop: tokio::sync::watch::Sender<bool>,
}

impl LockRenewer {
    /// Start renewing.
    ///
    /// `renew` is called every [`RENEW_INTERVAL`] and reports whether the
    /// renewal succeeded.
    ///
    /// It is an **async** closure rather than a receiver, because renewing a
    /// session lock is a round trip to the broker and the receiver is owned
    /// by the loop that is busy processing the batch. What the loop shares is
    /// this closure — in practice one that locks the receiver briefly,
    /// renews, and lets go. The handler never holds that lock, so a slow
    /// message does not block renewal, which is the whole point.
    #[must_use]
    pub fn start<F, Fut>(mut renew: F) -> Self
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = bool> + Send,
    {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);

        let task = tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let mut failures = 0_u32;

            loop {
                let waited = tokio::select! {
                    () = tokio::time::sleep(RENEW_INTERVAL) => true,
                    _ = stopped.changed() => false,
                };

                if !waited {
                    return Ended::Stopped;
                }

                if started.elapsed() >= MAX_RENEWAL_DURATION {
                    // The batch has stopped making progress. Holding the
                    // session any longer stalls every message behind it.
                    tracing::warn!(
                        "a batch has held its session for {:?}; giving up renewal",
                        MAX_RENEWAL_DURATION
                    );
                    return Ended::Expired;
                }

                if renew().await {
                    failures = 0;
                } else {
                    // Any count at or past the threshold ends renewal on the
                    // next line, so a count that cannot go higher is already
                    // at it: the declared hold is exact (`§5.1.2`).
                    failures = failures.checked_add(1).unwrap_or(MAX_CONSECUTIVE_FAILURES); // §5.1.2
                    tracing::warn!(failures, "a session lock renewal failed");
                    if failures >= MAX_CONSECUTIVE_FAILURES {
                        return Ended::Failed;
                    }
                }
            }
        });

        Self {
            task: Some(task),
            stop,
        }
    }

    /// Stop renewing, and wait for the task to finish.
    ///
    /// **Call this before closing the receiver.** `spec/03-message-bus.md`
    /// §5.1.4: a renewal in flight against a closed receiver is the race the
    /// Python spent a nine-line comment on.
    ///
    /// Awaiting the handle is what makes the ordering real rather than
    /// hoped-for.
    pub async fn stop(mut self) -> Ended {
        // The receiver is dropped when the task ends, so a send failing
        // means it already has.
        let _ = self.stop.send(true);

        let Some(task) = self.task.take() else {
            return Ended::Stopped;
        };

        task.await.unwrap_or_else(|error| {
            if error.is_cancelled() {
                Ended::Stopped
            } else {
                tracing::warn!(%error, "the lock renewal task ended badly");
                Ended::Failed
            }
        })
    }
}

impl Drop for LockRenewer {
    fn drop(&mut self) {
        // A renewer dropped without `stop` is a bug — the ordering of §5.1.4
        // was not honoured — but aborting is better than leaving a task
        // renewing a lock nobody holds.
        if let Some(task) = self.task.take()
            && !task.is_finished()
        {
            let _ = self.stop.send(true);
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Advance a paused clock by `intervals` renewal periods, letting the
    /// renewal task run between each.
    ///
    /// A single large `advance` is not enough: the task sleeps again as soon
    /// as it wakes, so it only gets one iteration per yield. Stepping is what
    /// lets it actually loop.
    async fn tick(intervals: u32) {
        // Let the freshly spawned task reach its first sleep, so the first
        // advance actually fires a renewal rather than scheduling one.
        tokio::task::yield_now().await;

        for _ in 0..intervals {
            tokio::time::advance(RENEW_INTERVAL + Duration::from_millis(1)).await;
            tokio::task::yield_now().await;
        }
    }

    /// A renewer that counts calls and answers `outcome`.
    fn counting(outcome: bool) -> (LockRenewer, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let renewer = LockRenewer::start(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                outcome
            }
        });
        (renewer, calls)
    }

    #[test]
    fn the_renewal_interval_leaves_room_for_failures() {
        // spec/03-message-bus.md §5.1.3 — renewing at the last moment means
        // one slow round trip loses the session.
        assert!(
            RENEW_INTERVAL * 2 < LOCK_DURATION,
            "at least two renewals must fit inside a lock"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_renewed_before_the_first_interval() {
        let (renewer, calls) = counting(true);

        tokio::time::advance(RENEW_INTERVAL / 2).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        assert_eq!(renewer.stop().await, Ended::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn a_lock_is_renewed_once_per_interval() {
        let (renewer, calls) = counting(true);

        tick(3).await;

        assert_eq!(calls.load(Ordering::SeqCst), 3, "renewals did not fire");
        assert_eq!(renewer.stop().await, Ended::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_ends_the_task_and_is_awaited() {
        // spec/03-message-bus.md §5.1.4 — the ordering that matters. `stop`
        // returning means no renewal is in flight.
        let (renewer, _) = counting(true);
        assert_eq!(renewer.stop().await, Ended::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_before_any_renewal_is_clean() {
        let (renewer, calls) = counting(true);
        assert_eq!(renewer.stop().await, Ended::Stopped);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_failures_end_renewal() {
        // Three failures at 20s is a minute without a lock; continuing to
        // try would only hide that it is gone.
        let (renewer, calls) = counting(false);

        tick(4).await;

        assert_eq!(renewer.stop().await, Ended::Failed);
        assert_eq!(calls.load(Ordering::SeqCst), MAX_CONSECUTIVE_FAILURES);
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_resets_the_failure_count() {
        // Two failures then a success must not end renewal: transient
        // failures are the normal case on a busy broker.
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);

        let renewer = LockRenewer::start(move || {
            let counter = Arc::clone(&counter);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                // Fail twice, succeed, then fail twice again.
                !matches!(n, 0 | 1 | 3 | 4)
            }
        });

        tick(5).await;

        assert_eq!(
            renewer.stop().await,
            Ended::Stopped,
            "a success should have reset the count"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_that_runs_too_long_gives_up_the_session() {
        // spec/03-message-bus.md §5.1.3 — holding a session forever stalls
        // every message behind it in the flow.
        let (renewer, _) = counting(true);

        // Enough intervals to exceed the maximum a batch may hold a session.
        let intervals =
            u32::try_from(MAX_RENEWAL_DURATION.as_secs() / RENEW_INTERVAL.as_secs()).unwrap() + 1;
        tick(intervals).await;

        assert_eq!(renewer.stop().await, Ended::Expired);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_without_stopping_does_not_leave_a_task_renewing() {
        // Dropping is a bug — §5.1.4 wants `stop` — but it must not leave a
        // task renewing a lock nobody holds.
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);

        drop(LockRenewer::start(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                true
            }
        }));

        tick(3).await;

        assert_eq!(calls.load(Ordering::SeqCst), 0, "the task kept renewing");
    }
}
