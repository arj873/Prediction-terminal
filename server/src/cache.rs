//! In-memory TTL cache with single-flight.
//!
//! Panels poll — several of them, on their own timers, often for the same
//! ticker. Without coalescing, four panels showing one market means four
//! upstream requests every tick. [`TtlCache::cached`] collapses concurrent
//! misses for the same key onto one in-flight call.
//!
//! One flat string keyspace (`kalshi:{path}`, `fred:series:{id}:{start}:{end}`,
//! `{venue}:corpus`, …) carries about twenty different value shapes, so the
//! stored value is type-erased behind `Arc<dyn Any + Send + Sync>` and
//! downcast on the way out. Each entry carries its own TTL rather than the
//! cache having one: a quote is stale in three seconds, a series catalogue is
//! good for the session.

use std::any::Any;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use moka::future::Cache;
use moka::notification::RemovalCause;
use moka::Expiry;
use terminal_core::types::CacheStats;

use crate::error::{codes, Result, UpstreamError};

/// How many entries the shared cache holds before it starts evicting.
pub const DEFAULT_MAX_ENTRIES: u64 = 1000;

/// A cached value and the TTL it was admitted with.
///
/// The value is type-erased because the keyspace is flat and shared; the TTL
/// rides along because [`PerEntryTtl`] is asked for it after the fact, when the
/// caller that chose it is long gone.
#[derive(Clone)]
struct Entry {
    value: Arc<dyn Any + Send + Sync>,
    ttl: Duration,
}

/// Expiry policy: every entry expires on the TTL it was stored with.
///
/// Reads do not extend it — a cached quote is three seconds old however often
/// it is looked at, which is the property the panels depend on.
struct PerEntryTtl;

impl Expiry<String, Entry> for PerEntryTtl {
    fn expire_after_create(
        &self,
        _key: &String,
        entry: &Entry,
        _created_at: Instant,
    ) -> Option<Duration> {
        Some(entry.ttl)
    }

    fn expire_after_update(
        &self,
        _key: &String,
        entry: &Entry,
        _updated_at: Instant,
        _duration_until_expiry: Option<Duration>,
    ) -> Option<Duration> {
        Some(entry.ttl)
    }
}

#[derive(Debug, Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

/// A TTL cache that coalesces concurrent misses.
///
/// Cheap to clone — every clone shares one store and one set of counters, so
/// this can be held by value in the router's state.
#[derive(Clone)]
pub struct TtlCache {
    inner: Cache<String, Entry>,
    counters: Arc<Counters>,
}

impl TtlCache {
    /// A cache holding [`DEFAULT_MAX_ENTRIES`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_ENTRIES)
    }

    #[must_use]
    pub fn with_capacity(max_entries: u64) -> Self {
        let counters = Arc::new(Counters::default());
        let on_evict = Arc::clone(&counters);

        let inner = Cache::builder()
            .max_capacity(max_entries)
            .expire_after(PerEntryTtl)
            // Only capacity pressure counts as an eviction. An entry that
            // simply reached its TTL did its job; a `/health` reading that
            // conflated the two would show thousands of "evictions" on a
            // healthy server and tell an operator nothing.
            .eviction_listener(move |_key, _entry, cause| {
                if cause == RemovalCause::Size {
                    on_evict.evictions.fetch_add(1, Ordering::Relaxed);
                }
            })
            .build();

        Self { inner, counters }
    }

    /// Return the cached value for `key`, or run `produce` to fill it.
    ///
    /// Concurrent callers that miss share a single `produce()` call: the first
    /// one in runs it, the rest wait on its result. A failure is *not* cached —
    /// the next caller retries — but it does reach everyone already waiting,
    /// rather than each of them re-asking an upstream that just said no.
    ///
    /// The value comes back as an `Arc<T>` because it is shared with whatever
    /// is still in the cache; nothing here needs to mutate it.
    pub async fn cached<T, F, Fut>(&self, key: &str, ttl: Duration, produce: F) -> Result<Arc<T>>
    where
        T: Send + Sync + 'static,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        // Set by whichever caller wins the race to initialise. Everyone else
        // never runs the closure, and so counts as a hit — including a caller
        // that only joined an in-flight call, which is exactly the request the
        // cache saved.
        let produced = AtomicBool::new(false);

        let entry = self
            .inner
            .try_get_with_by_ref(key, async {
                produced.store(true, Ordering::Relaxed);
                produce().await.map(|value| Entry {
                    value: Arc::new(value) as Arc<dyn Any + Send + Sync>,
                    ttl,
                })
            })
            .await;

        let counters = &*self.counters;
        if produced.load(Ordering::Relaxed) {
            counters.misses.fetch_add(1, Ordering::Relaxed);
        } else {
            counters.hits.fetch_add(1, Ordering::Relaxed);
        }

        // moka hands the same failure to every waiter behind an `Arc`; callers
        // want the plain error, and it is cheap to clone.
        let entry = entry.map_err(|err: Arc<UpstreamError>| (*err).clone())?;
        downcast(key, entry.value)
    }

    /// The live value for `key`, if there is one that has not expired.
    pub async fn get<T: Send + Sync + 'static>(&self, key: &str) -> Option<Arc<T>> {
        let entry = self.inner.get(key).await?;
        downcast(key, entry.value).ok()
    }

    /// Store `value` under `key` for `ttl`, replacing whatever was there.
    pub async fn set<T: Send + Sync + 'static>(&self, key: &str, value: T, ttl: Duration) {
        let entry = Entry {
            value: Arc::new(value) as Arc<dyn Any + Send + Sync>,
            ttl,
        };
        self.inner.insert(key.to_owned(), entry).await;
    }

    /// Drop one key.
    pub async fn delete(&self, key: &str) {
        self.inner.invalidate(key).await;
    }

    /// Drop everything. Counters are left alone — they describe the process,
    /// not the current contents.
    pub async fn clear(&self) {
        self.inner.invalidate_all();
        self.inner.run_pending_tasks().await;
    }

    /// Apply moka's queued maintenance — evictions, expiries, the entry count.
    ///
    /// moka does this work opportunistically on other operations, which is
    /// fine for a server and useless for a test that wants to assert on
    /// [`stats`](Self::stats) right now.
    pub async fn run_pending_tasks(&self) {
        self.inner.run_pending_tasks().await;
    }

    /// What `/health` reports.
    ///
    /// `entries` is moka's estimate: entries that have expired but not yet been
    /// swept still count. Call [`run_pending_tasks`](Self::run_pending_tasks)
    /// first if the exact number matters.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.counters.hits.load(Ordering::Relaxed),
            misses: self.counters.misses.load(Ordering::Relaxed),
            entries: self.inner.entry_count(),
            evictions: self.counters.evictions.load(Ordering::Relaxed),
        }
    }
}

impl Default for TtlCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TtlCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.stats();
        f.debug_struct("TtlCache")
            .field("entries", &stats.entries)
            .field("hits", &stats.hits)
            .field("misses", &stats.misses)
            .field("evictions", &stats.evictions)
            .finish()
    }
}

/// Recover the concrete type an entry was stored as.
///
/// A mismatch means two call sites picked the same key for different shapes.
/// That is a bug in the key, not a runtime condition to paper over, so it is
/// reported loudly rather than treated as a miss — silently re-fetching would
/// turn a wrong key into an invisible cache that never hits.
fn downcast<T: Send + Sync + 'static>(
    key: &str,
    value: Arc<dyn Any + Send + Sync>,
) -> Result<Arc<T>> {
    value.downcast::<T>().map_err(|_| {
        tracing::error!(
            key,
            expected = std::any::type_name::<T>(),
            "cache key holds a different type than the caller expected"
        );
        UpstreamError::new(
            format!("Cache key `{key}` holds a value of another type"),
            codes::INTERNAL,
        )
    })
}

/// TTLs, tuned to how fast each source actually moves.
pub mod ttl {
    use std::time::Duration;

    /// Quotes and books: short, but long enough to absorb a panel refresh burst.
    pub const QUOTE: Duration = Duration::from_secs(3);
    /// Candles: the newest bucket is still forming, so don't hold it long.
    pub const CANDLES: Duration = Duration::from_secs(20);
    /// Market/event metadata: titles and rules effectively never change.
    pub const META: Duration = Duration::from_secs(60);
    /// Series catalogue: static for the life of a session.
    pub const CATALOGUE: Duration = Duration::from_secs(15 * 60);
    /// FRED observations: revised on a release schedule, never intraday.
    pub const FRED: Duration = Duration::from_secs(30 * 60);
    /// Billboard: refreshes once a week.
    pub const BILLBOARD: Duration = Duration::from_secs(60 * 60);
    /// News: a wire. Short enough to feel live, long enough that N panels are 1 call.
    pub const NEWS: Duration = Duration::from_secs(30);
    /// Rotten Tomatoes: reviews trickle in, and a score can move mid-day.
    pub const ROTTEN_TOMATOES: Duration = Duration::from_secs(15 * 60);
    /// Netflix Top 10: published once a week, on Tuesdays.
    pub const NETFLIX: Duration = Duration::from_secs(6 * 60 * 60);
    /// Spotify and YouTube chart mirrors: rebuilt once a day.
    pub const STREAM_CHARTS: Duration = Duration::from_secs(30 * 60);
    /// Box office: estimates are revised through the day, then finalised.
    pub const BOX_OFFICE: Duration = Duration::from_secs(30 * 60);
    /// Steam concurrents: a live number, and the whole point of the panel.
    pub const STEAM: Duration = Duration::from_secs(120);
    /// TV schedules: fixed a day ahead, occasionally amended.
    pub const TV_SCHEDULE: Duration = Duration::from_secs(30 * 60);

    /// An award's record changes when a ceremony happens, which is a handful of
    /// evenings a year — but a nominee slate fills in over the days after one,
    /// so an hour is short enough to follow that without hammering WDQS.
    pub const AWARDS: Duration = Duration::from_secs(60 * 60);
    /// Google rebuilds the trending list through the day.
    pub const TRENDS: Duration = Duration::from_secs(15 * 60);
    /// A discography gains a row on release day and not otherwise.
    pub const RELEASES: Duration = Duration::from_secs(60 * 60);
    /// Apple recomputes its charts daily; an hour keeps a guest booking fresh
    /// without asking for a chart that has not moved.
    pub const PODCASTS: Duration = Duration::from_secs(60 * 60);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn concurrent_misses_share_one_call() {
        let cache = TtlCache::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..16 {
            let cache = cache.clone();
            let calls = Arc::clone(&calls);
            tasks.push(tokio::spawn(async move {
                cache
                    .cached("kalshi:markets", ttl::META, || async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        // Long enough that every task is waiting on this one.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Ok(format!("value-{n}"))
                    })
                    .await
                    .unwrap()
            }));
        }

        let values: Vec<Arc<String>> = futures::future::join_all(tasks)
            .await
            .into_iter()
            .map(|t| t.unwrap())
            .collect();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "produce ran more than once"
        );
        assert!(values.iter().all(|v| **v == "value-0"));

        // Everyone who joined the one in-flight call counts as a hit.
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 15);
    }

    #[tokio::test]
    async fn a_failure_is_not_cached() {
        let cache = TtlCache::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let first: Result<Arc<u32>> = cache
            .cached("fred:series:GDP::", ttl::FRED, || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(UpstreamError::blocked("fred.stlouisfed.org hung up"))
                }
            })
            .await;
        assert_eq!(first.unwrap_err().code, "upstream_blocked");

        let second = cache
            .cached("fred:series:GDP::", ttl::FRED, || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(7u32)
                }
            })
            .await
            .unwrap();

        assert_eq!(*second, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the retry was suppressed");
        // Both were misses: nothing was ever served from the store.
        assert_eq!(cache.stats().misses, 2);
    }

    #[tokio::test]
    async fn one_failure_reaches_every_waiter() {
        let cache = TtlCache::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let calls = Arc::clone(&calls);
            tasks.push(tokio::spawn(async move {
                cache
                    .cached("yahoo:quote:AAPL", ttl::QUOTE, || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        Err::<u8, _>(UpstreamError::timeout("query1.finance.yahoo.com"))
                    })
                    .await
                    .map(|v| *v)
            }));
        }

        for task in tasks {
            let err = task.await.unwrap().unwrap_err();
            assert_eq!(err.code, "upstream_timeout");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_entry_stops_being_served_after_its_ttl() {
        let cache = TtlCache::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::from_millis(60);

        for _ in 0..2 {
            let _ = cache
                .cached("quote:INXD", ttl, || {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(1u8)
                    }
                })
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the second read missed");

        tokio::time::sleep(Duration::from_millis(120)).await;

        let _ = cache
            .cached("quote:INXD", ttl, || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(1u8)
                }
            })
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the expired entry was served"
        );
        assert!(cache.get::<u8>("quote:INXD").await.is_some());
    }

    #[tokio::test]
    async fn evicts_once_it_is_over_capacity() {
        let cache = TtlCache::with_capacity(10);
        for i in 0..60u32 {
            cache.set(&format!("key:{i}"), i, ttl::META).await;
        }
        cache.run_pending_tasks().await;

        let stats = cache.stats();
        assert!(stats.entries <= 10, "held {} entries", stats.entries);
        assert!(stats.evictions > 0, "nothing was recorded as evicted");
    }

    #[tokio::test]
    async fn stats_report_hits_misses_and_entries() {
        let cache = TtlCache::new();
        assert_eq!(cache.stats().hits, 0);

        for _ in 0..3 {
            let _ = cache
                .cached("polymarket:corpus", ttl::CATALOGUE, || async { Ok(1u64) })
                .await
                .unwrap();
        }
        cache.run_pending_tasks().await;

        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.evictions, 0);
    }

    #[tokio::test]
    async fn different_shapes_share_one_keyspace() {
        #[derive(Debug, PartialEq)]
        struct Quote {
            price: f64,
        }

        let cache = TtlCache::new();
        let quote = cache
            .cached("kalshi:quote:INXD", ttl::QUOTE, || async {
                Ok(Quote { price: 0.62 })
            })
            .await
            .unwrap();
        let titles = cache
            .cached("kalshi:corpus", ttl::CATALOGUE, || async {
                Ok(vec!["S&P 500".to_string()])
            })
            .await
            .unwrap();

        assert_eq!(*quote, Quote { price: 0.62 });
        assert_eq!(titles.len(), 1);
        assert_eq!(
            *cache.get::<Quote>("kalshi:quote:INXD").await.unwrap(),
            Quote { price: 0.62 }
        );
        assert!(cache
            .get::<Vec<String>>("kalshi:quote:INXD")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn reusing_a_key_for_another_shape_is_an_error() {
        let cache = TtlCache::new();
        let _ = cache
            .cached("steam:app:570", ttl::STEAM, || async { Ok(120u32) })
            .await
            .unwrap();

        let wrong: Result<Arc<String>> = cache
            .cached("steam:app:570", ttl::STEAM, || async {
                Ok("120".to_string())
            })
            .await;
        assert_eq!(wrong.unwrap_err().code, codes::INTERNAL);
    }

    #[tokio::test]
    async fn set_get_delete_and_clear() {
        let cache = TtlCache::new();
        cache.set("news:top", "wire".to_string(), ttl::NEWS).await;
        assert_eq!(*cache.get::<String>("news:top").await.unwrap(), "wire");

        cache.delete("news:top").await;
        assert!(cache.get::<String>("news:top").await.is_none());

        cache.set("a", 1u8, ttl::NEWS).await;
        cache.set("b", 2u8, ttl::NEWS).await;
        cache.clear().await;
        assert_eq!(cache.stats().entries, 0);
        assert!(cache.get::<u8>("a").await.is_none());
    }

    #[test]
    fn ttls_match_the_table_they_were_tuned_from() {
        assert_eq!(ttl::QUOTE, Duration::from_secs(3));
        assert_eq!(ttl::CANDLES, Duration::from_secs(20));
        assert_eq!(ttl::META, Duration::from_secs(60));
        assert_eq!(ttl::CATALOGUE, Duration::from_secs(900));
        assert_eq!(ttl::FRED, Duration::from_secs(1800));
        assert_eq!(ttl::BILLBOARD, Duration::from_secs(3600));
        assert_eq!(ttl::NEWS, Duration::from_secs(30));
        assert_eq!(ttl::ROTTEN_TOMATOES, Duration::from_secs(900));
        assert_eq!(ttl::NETFLIX, Duration::from_secs(21_600));
        assert_eq!(ttl::STREAM_CHARTS, Duration::from_secs(1800));
        assert_eq!(ttl::BOX_OFFICE, Duration::from_secs(1800));
        assert_eq!(ttl::STEAM, Duration::from_secs(120));
        assert_eq!(ttl::TV_SCHEDULE, Duration::from_secs(1800));
    }
}
