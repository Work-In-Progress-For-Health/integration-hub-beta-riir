//! Noticing `SIGTERM`, and stopping without losing a message.
//!
//! Implements `spec/01-architecture.md` §1.3 and `spec/07-hl7-server.md` §8.
//!
//! Azure Container Apps stops a container by sending `SIGTERM` and then
//! waiting a fixed grace period before `SIGKILL`. Everything in that window
//! is the process's own responsibility, and for this estate the requirement
//! is sharp: **a message that has been acknowledged must have been
//! forwarded**. Shutting down in the middle of one is how that promise gets
//! broken, so shutdown is cooperative — a signal asks loops to stop between
//! messages, rather than cancelling whatever is in flight.
//!
//! # Examples
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! use ihb_runtime::shutdown::Shutdown;
//!
//! let shutdown = Shutdown::new();
//! let watcher = shutdown.watcher();
//!
//! assert!(!watcher.is_stopping());
//!
//! // Something decided it is time to stop.
//! shutdown.stop();
//!
//! assert!(watcher.is_stopping());
//! watcher.wait().await; // returns immediately once stopping
//! # }
//! ```

use tokio_util::sync::CancellationToken;

/// The stop signal for a process.
///
/// One of these is created at startup; every loop takes a [`Watcher`] from
/// it. Dropping it does **not** stop anything — a loop's shutdown must be
/// deliberate, not a consequence of where a value happened to go out of
/// scope.
#[derive(Debug, Clone, Default)]
pub struct Shutdown {
    /// The underlying token. Cloneable and cheap.
    token: CancellationToken,
}

impl Shutdown {
    /// A process that is not stopping.
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    /// A handle a loop can watch.
    #[must_use]
    pub fn watcher(&self) -> Watcher {
        Watcher {
            token: self.token.clone(),
        }
    }

    /// Ask every loop to stop at its next opportunity.
    ///
    /// Idempotent: calling it twice is the same as calling it once, which
    /// matters because a `SIGTERM` may be followed by an impatient second
    /// one.
    pub fn stop(&self) {
        if !self.token.is_cancelled() {
            tracing::info!("shutdown requested");
        }
        self.token.cancel();
    }

    /// Whether a stop has been requested.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Stop when `SIGTERM` or `SIGINT` arrives.
    ///
    /// Spawns a task that watches for either and calls [`Shutdown::stop`].
    /// Returns immediately.
    ///
    /// `SIGTERM` is what Container Apps sends; `SIGINT` is Ctrl-C on a
    /// laptop. Both mean the same thing here, as they do in the Python's
    /// `ProcessorManager`.
    ///
    /// # Panics
    ///
    /// Does not panic. If the signal handlers cannot be installed — which on
    /// Unix means the process is in a state where it could not have started
    /// anyway — the failure is logged at error and the process runs without
    /// them rather than aborting.
    pub fn listen_for_signals(&self) {
        let shutdown = self.clone();
        tokio::spawn(async move {
            wait_for_signal().await;
            shutdown.stop();
        });
    }
}

/// A loop's view of the stop signal.
///
/// Cheap to clone, and safe to hold across an await.
#[derive(Debug, Clone)]
pub struct Watcher {
    /// The underlying token.
    token: CancellationToken,
}

impl Watcher {
    /// Whether a stop has been requested.
    ///
    /// The condition a `while` loop tests between messages.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Whether the loop should keep going.
    ///
    /// The inverse of [`Watcher::is_stopping`], spelled the way a loop reads
    /// best: `while watcher.is_running() { … }`.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.token.is_cancelled()
    }

    /// Wait until a stop is requested.
    ///
    /// Returns immediately if one already has been. Used with `select!` to
    /// cut a sleep short.
    pub async fn wait(&self) {
        self.token.cancelled().await;
    }

    /// Sleep, unless a stop is requested first.
    ///
    /// Returns `true` if the sleep completed, `false` if it was cut short.
    /// This is what makes a fifteen-minute backoff compatible with a
    /// thirty-second grace period
    /// (`spec/03-message-bus.md` §5.4.3).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// use std::time::Duration;
    /// use ihb_runtime::shutdown::Shutdown;
    ///
    /// let shutdown = Shutdown::new();
    /// let watcher = shutdown.watcher();
    /// shutdown.stop();
    ///
    /// // Already stopping, so the sleep is abandoned rather than served.
    /// assert!(!watcher.sleep(Duration::from_secs(900)).await);
    /// # }
    /// ```
    pub async fn sleep(&self, duration: std::time::Duration) -> bool {
        tokio::select! {
            () = tokio::time::sleep(duration) => true,
            () = self.token.cancelled() => false,
        }
    }
}

/// Resolve when `SIGTERM` or `SIGINT` arrives.
#[cfg(unix)]
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!(%error, "could not listen for SIGTERM");
            return;
        }
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!(%error, "could not listen for SIGINT");
            return;
        }
    };

    tokio::select! {
        _ = terminate.recv() => tracing::info!("SIGTERM received"),
        _ = interrupt.recv() => tracing::info!("SIGINT received"),
    }
}

/// Resolve when Ctrl-C arrives.
///
/// The estate runs on Linux; this exists so the workspace builds and tests
/// on a developer's machine whatever it is.
#[cfg(not(unix))]
async fn wait_for_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "could not listen for Ctrl-C");
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn a_new_shutdown_is_not_stopping() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.is_stopping());
        assert!(shutdown.watcher().is_running());
    }

    #[tokio::test]
    async fn stopping_is_visible_to_every_watcher() {
        // spec/01-architecture.md §1.3 — one signal stops every loop.
        let shutdown = Shutdown::new();
        let first = shutdown.watcher();
        let second = shutdown.watcher();

        shutdown.stop();

        assert!(first.is_stopping());
        assert!(second.is_stopping());
        assert!(!first.is_running());
    }

    #[tokio::test]
    async fn stopping_twice_is_harmless() {
        // A second, impatient SIGTERM must not change anything.
        let shutdown = Shutdown::new();
        shutdown.stop();
        shutdown.stop();
        assert!(shutdown.is_stopping());
    }

    #[tokio::test]
    async fn dropping_a_shutdown_does_not_stop_anything() {
        // Shutdown must be deliberate, never a scope accident.
        let watcher = {
            let shutdown = Shutdown::new();
            shutdown.watcher()
        };
        assert!(watcher.is_running());
    }

    #[tokio::test(start_paused = true)]
    async fn a_sleep_runs_to_completion_when_nothing_stops_it() {
        // spec/17-testing-and-quality.md §5.2 — a paused clock, not a real
        // sleep.
        let shutdown = Shutdown::new();
        let watcher = shutdown.watcher();
        assert!(watcher.sleep(crate::backoff::MAX_DELAY).await);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sleep_is_cut_short_by_a_stop() {
        // spec/03-message-bus.md §5.4.3 — the reason `sleep` takes a watcher
        // at all: a 15-minute backoff must not outlast the grace period.
        //
        // Named rather than spelled as a literal, so that raising the
        // ceiling in `backoff` cannot quietly leave this test exercising a
        // duration the backoff no longer produces.
        let shutdown = Shutdown::new();
        let watcher = shutdown.watcher();

        let sleeping =
            tokio::spawn(async move { watcher.sleep(crate::backoff::MAX_DELAY).await });

        tokio::task::yield_now().await;
        shutdown.stop();

        assert!(
            !sleeping.await.expect("the sleeping task"),
            "the sleep should report having been cut short"
        );
    }

    #[tokio::test]
    async fn waiting_returns_at_once_when_already_stopping() {
        let shutdown = Shutdown::new();
        let watcher = shutdown.watcher();
        shutdown.stop();
        watcher.wait().await;
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_resolves_when_the_stop_arrives() {
        let shutdown = Shutdown::new();
        let watcher = shutdown.watcher();

        let waiting = tokio::spawn(async move { watcher.wait().await });
        tokio::task::yield_now().await;
        shutdown.stop();

        waiting.await.expect("the waiting task");
    }

    #[tokio::test]
    async fn installing_signal_handlers_does_not_stop_anything_by_itself() {
        let shutdown = Shutdown::new();
        shutdown.listen_for_signals();
        tokio::task::yield_now().await;
        assert!(!shutdown.is_stopping());
    }
}
