//! Where snapshots come from.
//!
//! Implements `spec/14-buswatch.md` §4.3 and the seam §8.4 needs.

use std::future::Future;

use crate::snapshot::Snapshot;

/// A source of queue snapshots.
///
/// A trait so the routes can be tested against a reader that answers from a
/// table rather than a broker — `spec/14-buswatch.md` §8.4 asks for the
/// empty-queue and unreachable-queue cases, and neither is convenient to
/// arrange on a real emulator.
///
/// Not dyn-compatible, for the reason in [`ihb_bus::sink::MessageSink`]: a
/// reader is chosen once at startup, so a generic parameter costs nothing
/// and avoids a vtable on every render.
pub trait QueueReader: Send + Sync + 'static {
    /// Peek at one queue.
    ///
    /// Never fails: a queue that cannot be read comes back as a
    /// [`Snapshot`] carrying its error (§6.2), because a failure to reach
    /// one queue must not fail the page.
    fn snapshot(&self, queue: &str, peek: usize) -> impl Future<Output = Snapshot> + Send;

    /// Find one message by sequence number (§5.1).
    ///
    /// `None` means it was not found within `limit` — which §5.2 makes a
    /// 404 that names the limit, so a reader knows to raise it rather than
    /// concluding the message is gone.
    fn detail(
        &self,
        queue: &str,
        sequence: i64,
        limit: usize,
    ) -> impl Future<Output = Option<MessageDetail>> + Send;

    /// Clear a queue (§3).
    ///
    /// Returns how many messages were removed, or why not.
    fn clear(&self, queue: &str) -> impl Future<Output = Result<usize, String>> + Send;
}

/// One message, in full (§5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDetail {
    /// The queue it is on.
    pub queue: String,
    /// Its sequence number.
    pub sequence: i64,
    /// The body, as text.
    pub body: String,
    /// Every application property, in a stable order.
    pub properties: Vec<(String, String)>,
    /// The broker's own properties, in a stable order.
    pub broker_properties: Vec<(String, String)>,
}
