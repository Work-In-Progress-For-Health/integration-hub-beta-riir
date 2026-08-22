//! The 30-second status cache.
//!
//! Implements `spec/15-dashboard.md` §2.2.1.
//!
//! # What the cache is actually for
//!
//! Not page speed. `/api/status` fans out to Log Analytics, the Service Bus
//! management API and Container Apps, and `dashboard.js` polls it — from
//! every browser tab an operations centre has open. Without a shared cache,
//! six tabs on a wall display are six times the Azure API calls, and those
//! APIs are rate-limited.
//!
//! So the cache is **shared across requests**, not per-session, and a
//! refresh in one tab is a refresh for all of them.

use std::time::Duration;

use tokio::sync::Mutex;
// `tokio`'s `Instant`, not the standard library's: under
// `tokio::time::pause` it advances with the paused clock, which is what
// lets the TTL boundary below be tested exactly and instantly rather than
// with a real 30-second sleep. Outside a test it is `std::time::Instant`.
use tokio::time::Instant;

/// A value with an expiry, shared between requests.
///
/// Generic over the payload so §2.2's other routes can reuse it, and so the
/// tests here do not need a whole status page to exercise the timing.
#[derive(Debug)]
pub struct Cached<T> {
    /// The value and when it was taken.
    state: Mutex<Option<(T, Instant)>>,
    /// How long a value stays fresh.
    ttl: Duration,
}

impl<T: Clone> Cached<T> {
    /// An empty cache with this lifetime.
    #[must_use]
    pub const fn new(ttl: Duration) -> Self {
        Self {
            state: Mutex::const_new(None),
            ttl,
        }
    }

    /// The cached value if it is still fresh, otherwise `build`'s result.
    ///
    /// `build` runs while the lock is held, so two requests arriving
    /// together produce **one** call to Azure rather than two. That is the
    /// point of the cache (see this module's documentation), and it is why
    /// this is not a read-then-write pair.
    pub async fn get_or_refresh<F, Fut>(&self, build: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let mut state = self.state.lock().await;

        if let Some((value, taken_at)) = state.as_ref()
            && taken_at.elapsed() < self.ttl
        {
            return value.clone();
        }

        let fresh = build().await;
        *state = Some((fresh.clone(), Instant::now()));
        fresh
    }

    /// Rebuild now, whatever the age of what is held (`/api/refresh`).
    pub async fn refresh<F, Fut>(&self, build: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let mut state = self.state.lock().await;
        let fresh = build().await;
        *state = Some((fresh.clone(), Instant::now()));
        fresh
    }

    /// Whether a fresh value is held.
    pub async fn is_fresh(&self) -> bool {
        self.state
            .lock()
            .await
            .as_ref()
            .is_some_and(|(_, taken_at)| taken_at.elapsed() < self.ttl)
    }

    /// Throw away what is held, without building anything.
    pub async fn clear(&self) {
        *self.state.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A builder that counts how many times it ran.
    fn counting() -> (Arc<AtomicUsize>, impl Fn() -> usize + Clone) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        (calls, move || counter.fetch_add(1, Ordering::SeqCst) + 1)
    }

    #[tokio::test]
    async fn a_fresh_value_is_served_without_rebuilding() {
        // spec/15-dashboard.md §2.2.1.
        let cache = Cached::new(Duration::from_secs(30));
        let (calls, build) = counting();

        let first = cache.get_or_refresh(|| async { build() }).await;
        let second = cache.get_or_refresh(|| async { build() }).await;

        assert_eq!(first, 1);
        assert_eq!(second, 1, "the second request was served from the cache");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "Azure was called once");
    }

    #[tokio::test]
    async fn a_stale_value_is_rebuilt() {
        // A zero TTL makes every value stale on arrival, which is the
        // boundary without a sleep in the test.
        let cache = Cached::new(Duration::ZERO);
        let (calls, build) = counting();

        cache.get_or_refresh(|| async { build() }).await;
        cache.get_or_refresh(|| async { build() }).await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn the_ttl_boundary_is_exercised_on_the_clock() {
        // The real boundary, with time paused so the test is instant and
        // not flaky. `tokio::time::advance` moves `Instant::now`.
        tokio::time::pause();

        let cache = Cached::new(Duration::from_secs(30));
        let (calls, build) = counting();

        cache.get_or_refresh(|| async { build() }).await;

        // Just inside.
        tokio::time::advance(Duration::from_secs(29)).await;
        cache.get_or_refresh(|| async { build() }).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "29s is still fresh");

        // And just past.
        tokio::time::advance(Duration::from_secs(2)).await;
        cache.get_or_refresh(|| async { build() }).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "31s is stale");
    }

    #[tokio::test]
    async fn refresh_rebuilds_whatever_the_age() {
        // `/api/refresh` — §2.2's "force a cache refresh".
        let cache = Cached::new(Duration::from_secs(30));
        let (calls, build) = counting();

        cache.get_or_refresh(|| async { build() }).await;
        assert!(cache.is_fresh().await);

        let refreshed = cache.refresh(|| async { build() }).await;

        assert_eq!(refreshed, 2, "rebuilt despite being fresh");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // And what it built is what the next request gets.
        assert_eq!(cache.get_or_refresh(|| async { build() }).await, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_requests_produce_one_call_to_azure() {
        // §2.2.1's "shared across requests" — THE test of this file. An
        // operations centre has this page open in several tabs, all
        // polling; without the lock held across the build, each one is its
        // own round of Azure API calls against a rate limit.
        let cache = Arc::new(Cached::new(Duration::from_secs(30)));
        let calls = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            tasks.push(tokio::spawn(async move {
                cache
                    .get_or_refresh(|| async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Long enough that the others are certainly
                        // waiting on the lock rather than finishing first.
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        7_usize
                    })
                    .await
            }));
        }

        for task in tasks {
            assert_eq!(task.await.expect("the task finishes"), 7);
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "eight concurrent requests must be one call to Azure"
        );
    }

    #[tokio::test]
    async fn clearing_drops_the_value_without_building_one() {
        let cache = Cached::new(Duration::from_secs(30));
        let (calls, build) = counting();

        cache.get_or_refresh(|| async { build() }).await;
        assert!(cache.is_fresh().await);

        cache.clear().await;

        assert!(!cache.is_fresh().await);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "clearing builds nothing");
    }

    #[tokio::test]
    async fn an_empty_cache_is_not_fresh() {
        let cache: Cached<usize> = Cached::new(Duration::from_secs(30));

        assert!(!cache.is_fresh().await);
    }
}
