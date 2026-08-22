//! What a queue looks like at one moment.
//!
//! Implements `spec/14-buswatch.md` §4, and the cache of §3.2.

use std::collections::BTreeMap;

use crate::error::QueueError;

/// One message, as the list page shows it (§4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSummary {
    /// The broker's sequence number — the handle the detail page uses.
    pub sequence: i64,
    /// When the broker accepted it, already formatted for display.
    pub enqueued_at: String,
    /// `MessageId`, when the sender set one.
    pub message_id: Option<String>,
    /// `SessionId`, when the queue is session-enabled.
    pub session_id: Option<String>,
    /// The body's size in bytes.
    pub size: usize,
    /// The first line or so of the body.
    pub preview: String,
}

/// How much of a body the list page shows.
pub const PREVIEW_LENGTH: usize = 160;

impl MessageSummary {
    /// A preview of a body: one line, bounded.
    ///
    /// An ER7 message is one long line with `\r` between segments, so a
    /// preview that kept them would break the table. Bounded by characters
    /// rather than bytes, so a multi-byte character is never cut in half.
    ///
    /// # Examples
    ///
    /// ```
    /// use buswatch::snapshot::MessageSummary;
    ///
    /// let er7 = "MSH|^~\\&|252\rPID|||8888888";
    /// assert_eq!(MessageSummary::preview_of(er7), "MSH|^~\\&|252 ⏎ PID|||8888888");
    /// ```
    #[must_use]
    pub fn preview_of(body: &str) -> String {
        let flattened = body.replace(['\r', '\n'], " ⏎ ");
        let mut preview: String = flattened.chars().take(PREVIEW_LENGTH).collect();
        if flattened.chars().count() > PREVIEW_LENGTH {
            preview.push('…');
        }
        preview
    }
}

/// One queue at one moment (§4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The queue's name.
    pub queue: String,
    /// Active messages, from the management API (§4.3).
    pub active_count: Option<i64>,
    /// Dead-lettered messages.
    pub dead_letter_count: Option<i64>,
    /// Up to `BUSWATCH_PEEK_COUNT` summaries.
    pub messages: Vec<MessageSummary>,
    /// When this was taken.
    pub taken_at: String,
    /// Why this queue could not be read, if it could not (§6.2).
    pub error: Option<QueueError>,
}

impl Snapshot {
    /// A snapshot recording that a queue could not be read.
    ///
    /// §6.2 — the page still renders, with this queue showing why and the
    /// rest showing their contents.
    #[must_use]
    pub fn failed(queue: impl Into<String>, taken_at: impl Into<String>, reason: &str) -> Self {
        Self {
            queue: queue.into(),
            active_count: None,
            dead_letter_count: None,
            messages: Vec::new(),
            taken_at: taken_at.into(),
            error: Some(QueueError::new(reason)),
        }
    }

    /// Whether this queue was read successfully.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Snapshots, one per queue, kept between renders.
///
/// §3.2: a page render serves the cache for every queue and refreshes only
/// those missing from it plus any explicitly requested. That is what keeps
/// the page responsive against an emulator that answers slowly — a full
/// refresh of nine queues takes long enough to be noticeable, and a
/// developer reloading to look at one queue should not pay for the other
/// eight.
#[derive(Debug, Default)]
pub struct Cache {
    /// By queue name, ordered so the page is stable between renders.
    snapshots: BTreeMap<String, Snapshot>,
}

impl Cache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a snapshot, replacing any earlier one for that queue.
    pub fn insert(&mut self, snapshot: Snapshot) {
        self.snapshots.insert(snapshot.queue.clone(), snapshot);
    }

    /// The snapshot for a queue, if one has been taken.
    #[must_use]
    pub fn get(&self, queue: &str) -> Option<&Snapshot> {
        self.snapshots.get(queue)
    }

    /// Every snapshot, in queue-name order.
    #[must_use]
    pub fn all(&self) -> Vec<&Snapshot> {
        self.snapshots.values().collect()
    }

    /// Drop one queue's snapshot.
    ///
    /// Used after a clear (`spec/14-buswatch.md` §3): the snapshot is now
    /// wrong, and showing messages that are gone is worse than showing
    /// nothing until the next render takes a fresh one.
    pub fn forget(&mut self, queue: &str) {
        self.snapshots.remove(queue);
    }

    /// Which of `queues` need refreshing for this render (§3.2).
    ///
    /// Those not yet in the cache, plus those explicitly asked for. A
    /// caller passes `refresh_all` for `?refresh_all=true` and one name for
    /// `?refresh_queue=…`.
    ///
    /// # Examples
    ///
    /// ```
    /// use buswatch::snapshot::{Cache, Snapshot};
    ///
    /// let mut cache = Cache::new();
    /// cache.insert(Snapshot::failed("known", "now", "x"));
    ///
    /// let queues = ["known".to_string(), "fresh".to_string()];
    ///
    /// // Only the one nobody has looked at yet.
    /// assert_eq!(cache.needing_refresh(&queues, None, false), ["fresh"]);
    ///
    /// // Plus the one asked for.
    /// assert_eq!(
    ///     cache.needing_refresh(&queues, Some("known"), false),
    ///     ["known", "fresh"]
    /// );
    ///
    /// // Or everything.
    /// assert_eq!(cache.needing_refresh(&queues, None, true), ["known", "fresh"]);
    /// ```
    #[must_use]
    pub fn needing_refresh(
        &self,
        queues: &[String],
        refresh_queue: Option<&str>,
        refresh_all: bool,
    ) -> Vec<String> {
        queues
            .iter()
            .filter(|queue| {
                refresh_all
                    || refresh_queue == Some(queue.as_str())
                    || !self.snapshots.contains_key(*queue)
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queues() -> Vec<String> {
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    }

    #[test]
    fn an_uncached_queue_is_always_refreshed() {
        // spec/14-buswatch.md §3.2 — the first render fills the cache.
        let cache = Cache::new();

        assert_eq!(
            cache.needing_refresh(&queues(), None, false),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn a_cached_queue_is_served_from_the_cache() {
        // §3.2 — the point of it. A developer reloading to look at one
        // queue should not pay for the other eight.
        let mut cache = Cache::new();
        for queue in queues() {
            cache.insert(Snapshot::failed(queue, "now", "x"));
        }

        assert!(cache.needing_refresh(&queues(), None, false).is_empty());
    }

    #[test]
    fn an_explicit_refresh_overrides_the_cache() {
        // §3.1 — `?refresh_queue={name}` and `?refresh_all=true`.
        let mut cache = Cache::new();
        for queue in queues() {
            cache.insert(Snapshot::failed(queue, "now", "x"));
        }

        assert_eq!(cache.needing_refresh(&queues(), Some("b"), false), ["b"]);
        assert_eq!(
            cache.needing_refresh(&queues(), None, true),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn a_failed_queue_is_still_a_snapshot() {
        // §6.2 — the page renders, with this queue showing why.
        let snapshot = Snapshot::failed("q", "now", "ConnectionsQuotaExceeded");

        assert!(!snapshot.is_ok());
        assert!(snapshot.messages.is_empty());
        assert!(
            snapshot.error.as_ref().unwrap().hint.is_some(),
            "and the quota hint came with it"
        );
    }

    #[test]
    fn a_preview_is_one_line_and_bounded() {
        // An ER7 message is one long line with `\r` between segments, and
        // a preview that kept them would break the table.
        let er7 = "MSH|^~\\&|252\rPID|||8888888\rPV1||U";
        let preview = MessageSummary::preview_of(er7);

        assert!(!preview.contains('\r'));
        assert!(preview.contains(" ⏎ "));

        let long = "x".repeat(1000);
        let bounded = MessageSummary::preview_of(&long);
        assert_eq!(
            bounded.chars().count(),
            PREVIEW_LENGTH + 1,
            "plus the ellipsis"
        );
    }

    #[test]
    fn a_preview_never_cuts_a_character_in_half() {
        // The shape that broke `ihb_telemetry::redact`: a fixed offset into
        // text that is not ASCII.
        let emoji = "🏥".repeat(500);
        let preview = MessageSummary::preview_of(&emoji);

        assert_eq!(preview.chars().count(), PREVIEW_LENGTH + 1);
        assert!(!preview.contains('\u{fffd}'), "no replacement characters");
    }

    #[test]
    fn the_cache_orders_queues_stably() {
        // A page whose rows move between renders is one nobody can read.
        let mut cache = Cache::new();
        for queue in ["c", "a", "b"] {
            cache.insert(Snapshot::failed(queue, "now", "x"));
        }

        let names: Vec<&str> = cache.all().iter().map(|s| s.queue.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
