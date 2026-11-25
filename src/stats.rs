//! Cache statistics.
//!
//! Statistics are off by default. Call `record_stats` on a cache builder to turn
//! them on, and `stats` on the cache to take a [`CacheStats`] snapshot:
//!
//! ```rust
//! # #[cfg(feature = "sync")]
//! # {
//! use moka::sync::Cache;
//!
//! let cache = Cache::builder().max_capacity(100).record_stats().build();
//!
//! cache.insert(1, "one");
//! assert_eq!(cache.get(&1), Some("one"));
//! assert_eq!(cache.get(&2), None);
//!
//! let stats = cache.stats();
//! assert_eq!(stats.hit_count(), 1);
//! assert_eq!(stats.miss_count(), 1);
//! assert_eq!(stats.hit_rate(), 0.5);
//! # }
//! ```
//!
//! To feed the events into something else, such as a metrics library, implement
//! [`StatsCounter`] and pass it to the builder's `stats_counter` method.
//!
//! # What is recorded
//!
//! - **Hits and misses**: every lookup that the cache policy sees. These are `get`,
//!   the lookup made by the `get_with` family of methods, and the lookup made by
//!   the entry API. `contains_key` and iterators are not lookups.
//! - **Loads**: every evaluation of an `init` closure or future by `get_with`,
//!   `optionally_get_with`, `try_get_with` and their entry API counterparts. A load
//!   that returned `None` or `Err` is a failure. A caller that waited for another
//!   caller's `init` records no load.
//! - **Evictions**: every entry removed because the cache was over capacity
//!   ([`RemovalCause::Size`]) or because the entry had expired
//!   ([`RemovalCause::Expired`]). Entries that were invalidated or replaced are not
//!   evictions.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use crossbeam_utils::CachePadded;
use portable_atomic::AtomicU64;

use crate::notification::RemovalCause;

/// A snapshot of the statistics of a cache.
///
/// See the [module level documentation](self) for what is recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    hit_count: u64,
    miss_count: u64,
    load_success_count: u64,
    load_failure_count: u64,
    total_load_time_nanos: u64,
    eviction_count: u64,
    eviction_weight: u64,
}

impl CacheStats {
    /// Sets the numbers of lookups that found and did not find a value.
    ///
    /// The `with_` methods are for [`StatsCounter`] implementations, which have
    /// to build a `CacheStats` in `snapshot`.
    #[must_use]
    pub fn with_requests(mut self, hit_count: u64, miss_count: u64) -> Self {
        self.hit_count = hit_count;
        self.miss_count = miss_count;
        self
    }

    /// Sets the numbers of successful and failed loads, and the total time spent
    /// in them.
    #[must_use]
    pub fn with_loads(
        mut self,
        load_success_count: u64,
        load_failure_count: u64,
        total_load_time: Duration,
    ) -> Self {
        self.load_success_count = load_success_count;
        self.load_failure_count = load_failure_count;
        self.total_load_time_nanos = duration_to_nanos(total_load_time);
        self
    }

    /// Sets the number of evicted entries and the sum of their weights.
    #[must_use]
    pub fn with_evictions(mut self, eviction_count: u64, eviction_weight: u64) -> Self {
        self.eviction_count = eviction_count;
        self.eviction_weight = eviction_weight;
        self
    }

    /// Returns the number of lookups, which is the sum of `hit_count` and
    /// `miss_count`.
    pub fn request_count(&self) -> u64 {
        self.hit_count.saturating_add(self.miss_count)
    }

    /// Returns the number of lookups that found a value.
    pub fn hit_count(&self) -> u64 {
        self.hit_count
    }

    /// Returns the ratio of lookups that found a value, or `1.0` when there were
    /// no lookups.
    pub fn hit_rate(&self) -> f64 {
        match self.request_count() {
            0 => 1.0,
            n => self.hit_count as f64 / n as f64,
        }
    }

    /// Returns the number of lookups that did not find a value.
    pub fn miss_count(&self) -> u64 {
        self.miss_count
    }

    /// Returns the ratio of lookups that did not find a value, or `0.0` when there
    /// were no lookups.
    pub fn miss_rate(&self) -> f64 {
        match self.request_count() {
            0 => 0.0,
            n => self.miss_count as f64 / n as f64,
        }
    }

    /// Returns the number of loads, which is the sum of `load_success_count` and
    /// `load_failure_count`.
    pub fn load_count(&self) -> u64 {
        self.load_success_count
            .saturating_add(self.load_failure_count)
    }

    /// Returns the number of loads that produced a value.
    pub fn load_success_count(&self) -> u64 {
        self.load_success_count
    }

    /// Returns the number of loads that returned `None` or `Err`.
    pub fn load_failure_count(&self) -> u64 {
        self.load_failure_count
    }

    /// Returns the ratio of loads that failed, or `0.0` when there were no loads.
    pub fn load_failure_rate(&self) -> f64 {
        match self.load_count() {
            0 => 0.0,
            n => self.load_failure_count as f64 / n as f64,
        }
    }

    /// Returns the total time spent in loads, successful or not.
    pub fn total_load_time(&self) -> Duration {
        Duration::from_nanos(self.total_load_time_nanos)
    }

    /// Returns the average time spent in a load, or zero when there were no loads.
    pub fn average_load_penalty(&self) -> Duration {
        match self.load_count() {
            0 => Duration::ZERO,
            n => Duration::from_nanos(self.total_load_time_nanos / n),
        }
    }

    /// Returns the number of evicted entries.
    pub fn eviction_count(&self) -> u64 {
        self.eviction_count
    }

    /// Returns the sum of the weights of the evicted entries.
    pub fn eviction_weight(&self) -> u64 {
        self.eviction_weight
    }
}

/// Accumulates the statistics of a cache.
///
/// The cache calls the `record_` methods from the threads that use it, so they
/// should be cheap and must not call back into the cache.
///
/// `Arc<T>` implements this trait when `T` does, which lets you keep a handle to a
/// counter that you gave to a cache builder.
pub trait StatsCounter: Send + Sync + 'static {
    /// Records lookups that found a value.
    fn record_hits(&self, count: u64);

    /// Records lookups that did not find a value.
    fn record_misses(&self, count: u64);

    /// Records a load that produced a value.
    fn record_load_success(&self, load_time: Duration);

    /// Records a load that returned `None` or `Err`.
    fn record_load_failure(&self, load_time: Duration);

    /// Records the eviction of an entry. The `cause` is either
    /// [`RemovalCause::Size`] or [`RemovalCause::Expired`].
    fn record_eviction(&self, weight: u32, cause: RemovalCause);

    /// Returns a snapshot of the recorded statistics. The cache returns it from
    /// its `stats` method.
    fn snapshot(&self) -> CacheStats;
}

impl<T: StatsCounter + ?Sized> StatsCounter for Arc<T> {
    fn record_hits(&self, count: u64) {
        (**self).record_hits(count);
    }

    fn record_misses(&self, count: u64) {
        (**self).record_misses(count);
    }

    fn record_load_success(&self, load_time: Duration) {
        (**self).record_load_success(load_time);
    }

    fn record_load_failure(&self, load_time: Duration) {
        (**self).record_load_failure(load_time);
    }

    fn record_eviction(&self, weight: u32, cause: RemovalCause) {
        (**self).record_eviction(weight, cause);
    }

    fn snapshot(&self) -> CacheStats {
        (**self).snapshot()
    }
}

/// The [`StatsCounter`] that a cache uses when its builder's `record_stats` method
/// was called.
///
/// It spreads the counters over several cache lines and lets each thread update
/// its own, so that threads recording at the same time do not contend. A snapshot
/// sums them up without stopping the recording threads, so its numbers may be from
/// slightly different points in time.
pub struct ConcurrentStatsCounter {
    // The length is a power of two.
    stripes: Box<[CachePadded<Stripe>]>,
}

#[derive(Default)]
struct Stripe {
    hit_count: AtomicU64,
    miss_count: AtomicU64,
    load_success_count: AtomicU64,
    load_failure_count: AtomicU64,
    total_load_time_nanos: AtomicU64,
    eviction_count: AtomicU64,
    eviction_weight: AtomicU64,
}

impl Default for ConcurrentStatsCounter {
    fn default() -> Self {
        let len = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .next_power_of_two();
        Self {
            stripes: (0..len).map(|_| CachePadded::default()).collect(),
        }
    }
}

impl ConcurrentStatsCounter {
    /// Creates a counter with all statistics at zero.
    pub fn new() -> Self {
        Self::default()
    }

    fn stripe(&self) -> &Stripe {
        static NEXT_THREAD: AtomicUsize = AtomicUsize::new(0);
        thread_local! {
            static THREAD: usize = NEXT_THREAD.fetch_add(1, Ordering::Relaxed);
        }
        // The thread local is gone while the thread's other thread locals are
        // being dropped. Any stripe will do then.
        let thread = THREAD.try_with(|t| *t).unwrap_or(0);
        &self.stripes[thread & (self.stripes.len() - 1)]
    }

    fn sum(&self, counter: impl Fn(&Stripe) -> &AtomicU64) -> u64 {
        self.stripes
            .iter()
            .map(|stripe| counter(stripe).load(Ordering::Relaxed))
            .fold(0, u64::wrapping_add)
    }
}

impl StatsCounter for ConcurrentStatsCounter {
    fn record_hits(&self, count: u64) {
        self.stripe().hit_count.fetch_add(count, Ordering::Relaxed);
    }

    fn record_misses(&self, count: u64) {
        self.stripe().miss_count.fetch_add(count, Ordering::Relaxed);
    }

    fn record_load_success(&self, load_time: Duration) {
        let stripe = self.stripe();
        stripe.load_success_count.fetch_add(1, Ordering::Relaxed);
        stripe
            .total_load_time_nanos
            .fetch_add(duration_to_nanos(load_time), Ordering::Relaxed);
    }

    fn record_load_failure(&self, load_time: Duration) {
        let stripe = self.stripe();
        stripe.load_failure_count.fetch_add(1, Ordering::Relaxed);
        stripe
            .total_load_time_nanos
            .fetch_add(duration_to_nanos(load_time), Ordering::Relaxed);
    }

    fn record_eviction(&self, weight: u32, _cause: RemovalCause) {
        let stripe = self.stripe();
        stripe.eviction_count.fetch_add(1, Ordering::Relaxed);
        stripe
            .eviction_weight
            .fetch_add(u64::from(weight), Ordering::Relaxed);
    }

    fn snapshot(&self) -> CacheStats {
        CacheStats {
            hit_count: self.sum(|s| &s.hit_count),
            miss_count: self.sum(|s| &s.miss_count),
            load_success_count: self.sum(|s| &s.load_success_count),
            load_failure_count: self.sum(|s| &s.load_failure_count),
            total_load_time_nanos: self.sum(|s| &s.total_load_time_nanos),
            eviction_count: self.sum(|s| &s.eviction_count),
            eviction_weight: self.sum(|s| &s.eviction_weight),
        }
    }
}

fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Records the outcome and the duration of a load, if statistics are on.
pub(crate) struct LoadTimer<'a> {
    stats_counter: &'a dyn StatsCounter,
    started_at: std::time::Instant,
}

impl<'a> LoadTimer<'a> {
    pub(crate) fn start(stats_counter: Option<&'a Arc<dyn StatsCounter>>) -> Option<Self> {
        stats_counter.map(|stats_counter| Self {
            stats_counter: &**stats_counter,
            started_at: std::time::Instant::now(),
        })
    }

    pub(crate) fn finish(self, is_success: bool) {
        let load_time = self.started_at.elapsed();
        if is_success {
            self.stats_counter.record_load_success(load_time);
        } else {
            self.stats_counter.record_load_failure(load_time);
        }
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use super::*;

    /// Keeps the weight and the cause of every eviction.
    #[derive(Default)]
    pub(crate) struct EvictionRecorder {
        pub(crate) evictions: parking_lot::Mutex<Vec<(u32, RemovalCause)>>,
    }

    impl StatsCounter for EvictionRecorder {
        fn record_hits(&self, _count: u64) {}

        fn record_misses(&self, _count: u64) {}

        fn record_load_success(&self, _load_time: Duration) {}

        fn record_load_failure(&self, _load_time: Duration) {}

        fn record_eviction(&self, weight: u32, cause: RemovalCause) {
            self.evictions.lock().push((weight, cause));
        }

        fn snapshot(&self) -> CacheStats {
            let evictions = self.evictions.lock();
            let weight = evictions.iter().map(|(weight, _)| u64::from(*weight)).sum();
            CacheStats::default().with_evictions(evictions.len() as u64, weight)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_of_empty_stats() {
        let stats = CacheStats::default();
        assert_eq!(stats.hit_rate(), 1.0);
        assert_eq!(stats.miss_rate(), 0.0);
        assert_eq!(stats.load_failure_rate(), 0.0);
        assert_eq!(stats.average_load_penalty(), Duration::ZERO);
    }

    #[test]
    fn derived_values() {
        let stats = CacheStats::default()
            .with_requests(3, 1)
            .with_loads(1, 3, Duration::from_secs(8))
            .with_evictions(2, 20);
        assert_eq!(stats.request_count(), 4);
        assert_eq!(stats.hit_rate(), 0.75);
        assert_eq!(stats.miss_rate(), 0.25);
        assert_eq!(stats.load_count(), 4);
        assert_eq!(stats.load_failure_rate(), 0.75);
        assert_eq!(stats.total_load_time(), Duration::from_secs(8));
        assert_eq!(stats.average_load_penalty(), Duration::from_secs(2));
        assert_eq!(stats.eviction_count(), 2);
        assert_eq!(stats.eviction_weight(), 20);
    }

    #[test]
    fn concurrent_counter_sums_all_threads() {
        const THREADS: u64 = 8;
        const ROUNDS: u64 = 1_000;

        let counter = Arc::new(ConcurrentStatsCounter::new());
        let handles = (0..THREADS)
            .map(|_| {
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || {
                    for _ in 0..ROUNDS {
                        counter.record_hits(2);
                        counter.record_misses(1);
                        counter.record_load_success(Duration::from_nanos(5));
                        counter.record_load_failure(Duration::from_nanos(5));
                        counter.record_eviction(3, RemovalCause::Size);
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let n = THREADS * ROUNDS;
        let expected = CacheStats::default()
            .with_requests(2 * n, n)
            .with_loads(n, n, Duration::from_nanos(10 * n))
            .with_evictions(n, 3 * n);
        assert_eq!(counter.snapshot(), expected);
    }
}
