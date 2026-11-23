//! The per-table TTL cache.
//!
//! Implements `spec/20-lookup-service.md` §5.3.

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::Row;

/// A monotonic clock in seconds, injectable so a test can move time.
///
/// Seconds as `f64` because the values only ever compare against
/// `now + ttl`; the origin is arbitrary, so never compare it to wall time.
pub type Timer = Arc<dyn Fn() -> f64 + Send + Sync>;

/// The real clock: seconds since the first call.
#[must_use]
pub fn monotonic_timer() -> Timer {
    let start = std::time::Instant::now();
    Arc::new(move || start.elapsed().as_secs_f64())
}

/// One more than `count`, holding at `u64::MAX` (`spec/20-lookup-service.md`
/// §5.3). The hit and miss counts are statistics and `tick` only orders
/// recency, so a count that cannot go higher is not an error: it stays put.
/// Reaching it takes 2^64 lookups.
#[expect(
    clippy::manual_saturating_arithmetic,
    reason = "RCA-INT-4: written as checked_add plus the declared fallback, not saturating_add"
)]
fn counted(count: u64) -> u64 {
    count.checked_add(1).unwrap_or(u64::MAX) // §5.3
}

/// One cached row.
struct Entry {
    /// The row.
    row: Row,
    /// When it stops being served, on the cache's clock.
    expires_at: f64,
    /// A counter, not a time: the least recently used entry has the lowest.
    used: u64,
}

/// A table's cache of found rows.
///
/// A miss is never cached (§1.1, Phase 4). Expired entries are evicted
/// before a live one is, and then the least recently used goes. Eviction
/// scans the entries, which is acceptable at the default 10 000 and only on
/// the insert that finds the cache full.
pub struct TableCache {
    /// How long an entry is served.
    ttl_seconds: u32,
    /// The most entries held.
    max_entries: usize,
    /// The clock.
    timer: Timer,
    /// The entries by canonical key.
    entries: HashMap<Vec<String>, Entry>,
    /// Counts uses, to find the least recent.
    tick: u64,
    /// Hits so far.
    hits: u64,
    /// Misses so far.
    misses: u64,
}

impl TableCache {
    /// An empty cache holding at most `max_entries` rows for `ttl_seconds`
    /// each (`spec/20-lookup-service.md` §5.3).
    ///
    /// With a `max_entries` of zero nothing is ever evicted (there is no
    /// entry to pick), so the cache would hold one row; callers validate the
    /// configured value first.
    #[must_use]
    pub fn new(ttl_seconds: u32, max_entries: usize, timer: Timer) -> Self {
        Self {
            ttl_seconds,
            max_entries,
            timer,
            entries: HashMap::new(),
            tick: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// The TTL this cache was built with.
    #[must_use]
    pub const fn ttl_seconds(&self) -> u32 {
        self.ttl_seconds
    }

    /// Hits so far.
    #[must_use]
    pub const fn hits(&self) -> u64 {
        self.hits
    }

    /// Misses so far.
    #[must_use]
    pub const fn misses(&self) -> u64 {
        self.misses
    }

    /// Look a key up, counting the hit or miss.
    ///
    /// An expired entry is removed here and counted as a miss, so a stale
    /// row is never served and the map does not grow with dead keys. A hit
    /// refreshes recency but **not** the expiry: the TTL bounds how stale a
    /// row may be, however often it is read.
    pub fn get(&mut self, key: &[String]) -> Option<Row> {
        let now = (self.timer)();
        self.tick = counted(self.tick);
        let tick = self.tick;
        match self.entries.get_mut(key) {
            Some(entry) if entry.expires_at > now => {
                entry.used = tick;
                self.hits = counted(self.hits);
                Some(entry.row.clone())
            }
            Some(_) => {
                self.entries.remove(key);
                self.misses = counted(self.misses);
                None
            }
            None => {
                self.misses = counted(self.misses);
                None
            }
        }
    }

    /// Cache a found row. Callers must not cache a miss (§5.3).
    ///
    /// When the cache is full and `key` is new, expired entries go first and
    /// then the least recently used one. Re-inserting an existing key
    /// replaces it and restarts its TTL without evicting anything.
    pub fn put(&mut self, key: Vec<String>, row: Row) {
        let now = (self.timer)();
        self.tick = counted(self.tick);
        if !self.entries.contains_key(&key) && self.entries.len() >= self.max_entries {
            self.entries.retain(|_, entry| entry.expires_at > now);
            if self.entries.len() >= self.max_entries
                && let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        // An expiry that is not a number means the entry is not cached: the
        // next lookup is a miss, which is always correct (RCA-FLT-2).
        let Some(expires_at) = ihb_runtime::float::add(now, f64::from(self.ttl_seconds)) else {
            return;
        };
        self.entries.insert(
            key,
            Entry {
                row,
                expires_at,
                used: self.tick,
            },
        );
    }

    /// Drop everything, as after an upload or settings change rebuilds the
    /// table (`spec/20-lookup-service.md` §5.4). Hit and miss counts are kept.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Live entries; expired ones are purged first, hence `&mut self`.
    #[must_use]
    pub fn len(&mut self) -> usize {
        let now = (self.timer)();
        self.entries.retain(|_, entry| entry.expires_at > now);
        self.entries.len()
    }

    /// Whether no live entry is held.
    #[must_use]
    pub fn is_empty(&mut self) -> bool {
        self.len() == 0
    }
}
