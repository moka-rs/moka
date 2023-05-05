use crate::notification::RemovalCause;

use std::{
    fmt::{self, Debug},
    ops::{Add, Sub},
    sync::atomic::{AtomicUsize, Ordering},
};

use crossbeam_utils::{atomic::AtomicCell, CachePadded};
use once_cell::sync::Lazy;

/// Statistics about the performance of a cache.
///
/// Cache statistics are incremented according to the following rules:
///
/// - When a cache lookup encounters an existing cache entry, `hit_count` is
///   incremented.
/// - When a cache lookup first encounters a missing cache entry, `miss_count` is
///   incremented.
///    - If the lookup was made by a `get_with` family method, a new entry will be
///      loaded:
///        - After successfully loading an entry, `load_success_count` is
///          incremented, and the total loading time, in nanoseconds, is added to
///          `total_load_time_nanos`.
///        - When failed to load an entry, `load_failure_count` is incremented, and
///          the total loading time, in nanoseconds, is added to
///          `total_load_time_nanos`.
///        - If another `get_with` family method is already loading the entry, it
///          will wait for the loading to complete (whether successful or not), but
///          it does _not_ modify `load_success_count`, `load_failure_count` and
///          `total_load_time_nanos`.
///-  When an entry is evicted from the cache (with a removal cause `Expired` or
///   `Size`), `eviction_count` is incremented and the weight added to
///   `eviction_weight`.
/// - No stats are modified when a cache entry is manually invalidated, removed or
///   replaced. (Removed with a cause `Explicit` or `Replaced`).

// TODO: Add doc about `read_drop_count`, `write_wait_count` and
// `total_write_wait_time_nanos`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CacheStats {
    hit_count: u64,
    miss_count: u64,
    load_success_count: u64,
    load_failure_count: u64,
    total_load_time_nanos: u64,
    eviction_by_size_count: u64,
    eviction_by_size_weight: u64,
    eviction_by_expiration_count: u64,
    eviction_by_expiration_weight: u64,
    read_drop_count: u64,
    write_wait_count: u64,
    total_write_wait_time_nanos: u64,
}

impl Debug for CacheStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheStats")
            .field("request_count", &self.request_count())
            .field("hit_count", &self.hit_count)
            .field("hit_rate", &self.hit_rate())
            .field("miss_count", &self.miss_count)
            .field("miss_rate", &self.miss_rate())
            .field("load_count", &self.load_count())
            .field("load_success_count", &self.load_success_count)
            .field("load_failure_count", &self.load_failure_count)
            .field("load_failure_rate", &self.load_failure_rate())
            .field("total_load_time_nanos", &self.total_load_time_nanos)
            .field(
                "average_load_penalty_nanos",
                &self.average_load_penalty_nanos(),
            )
            .field("eviction_by_size_count", &self.eviction_by_size_count)
            .field("eviction_by_size_weight", &self.eviction_by_size_weight)
            .field(
                "eviction_by_expiration_count",
                &self.eviction_by_expiration_count,
            )
            .field(
                "eviction_by_expiration_weight",
                &self.eviction_by_expiration_weight,
            )
            .field("read_drop_count", &self.read_drop_count)
            .field("write_wait_count", &self.write_wait_count)
            .field(
                "total_write_wait_time_nanos",
                &self.total_write_wait_time_nanos,
            )
            .field(
                "average_write_wait_time_nanos",
                &self.average_write_wait_time_nanos(),
            )
            .finish()
    }
}

impl CacheStats {
    pub fn set_req_counts(&mut self, hit_count: u64, miss_count: u64) -> &mut Self {
        self.hit_count = hit_count;
        self.miss_count = miss_count;
        self
    }

    pub fn set_load_counts(
        &mut self,
        load_success_count: u64,
        load_failure_count: u64,
        total_load_time_nanos: u64,
    ) -> &mut Self {
        self.load_success_count = load_success_count;
        self.load_failure_count = load_failure_count;
        self.total_load_time_nanos = total_load_time_nanos;
        self
    }

    pub fn set_eviction_counts(
        &mut self,
        eviction_by_size_count: u64,
        eviction_by_size_weight: u64,
        eviction_by_expiration_count: u64,
        eviction_by_expiration_weight: u64,
    ) -> &mut Self {
        self.eviction_by_size_count = eviction_by_size_count;
        self.eviction_by_size_weight = eviction_by_size_weight;
        self.eviction_by_expiration_count = eviction_by_expiration_count;
        self.eviction_by_expiration_weight = eviction_by_expiration_weight;
        self
    }

    pub fn set_read_drop_count(&mut self, count: u64) -> &mut Self {
        self.read_drop_count = count;
        self
    }

    pub fn set_write_wait_count(
        &mut self,
        write_wait_count: u64,
        total_write_wait_time_nanos: u64,
    ) -> &mut Self {
        self.write_wait_count = write_wait_count;
        self.total_write_wait_time_nanos = total_write_wait_time_nanos;
        self
    }

    pub fn request_count(&self) -> u64 {
        self.hit_count.saturating_add(self.miss_count)
    }

    pub fn hit_count(&self) -> u64 {
        self.hit_count
    }

    pub fn hit_rate(&self) -> f64 {
        let req_count = self.request_count();
        if req_count == 0 {
            1.0
        } else {
            self.hit_count as f64 / req_count as f64
        }
    }

    pub fn miss_count(&self) -> u64 {
        self.miss_count
    }

    pub fn miss_rate(&self) -> f64 {
        let req_count = self.request_count();
        if req_count == 0 {
            0.0
        } else {
            self.miss_count as f64 / req_count as f64
        }
    }

    pub fn load_count(&self) -> u64 {
        self.load_success_count
            .saturating_add(self.load_failure_count)
    }

    pub fn load_success_count(&self) -> u64 {
        self.load_success_count
    }

    pub fn load_failure_count(&self) -> u64 {
        self.load_failure_count
    }

    pub fn load_failure_rate(&self) -> f64 {
        let load_count = self.load_count();
        if load_count == 0 {
            0.0
        } else {
            self.load_failure_count as f64 / load_count as f64
        }
    }

    pub fn total_load_time_nanos(&self) -> u64 {
        self.total_load_time_nanos
    }

    pub fn average_load_penalty_nanos(&self) -> f64 {
        let load_count = self.load_count();
        if load_count == 0 {
            0.0
        } else {
            self.total_load_time_nanos as f64 / load_count as f64
        }
    }

    pub fn eviction_by_size_count(&self) -> u64 {
        self.eviction_by_size_count
    }

    pub fn eviction_by_size_weight(&self) -> u64 {
        self.eviction_by_size_weight
    }

    pub fn eviction_by_expiration_count(&self) -> u64 {
        self.eviction_by_expiration_count
    }

    pub fn eviction_by_expiration_weight(&self) -> u64 {
        self.eviction_by_expiration_weight
    }

    pub fn read_drop_count(&self) -> u64 {
        self.read_drop_count
    }

    pub fn write_wait_count(&self) -> u64 {
        self.write_wait_count
    }

    pub fn total_write_wait_time_nanos(&self) -> u64 {
        self.total_write_wait_time_nanos
    }

    pub fn average_write_wait_time_nanos(&self) -> f64 {
        let write_wait_count = self.write_wait_count();
        if write_wait_count == 0 {
            0.0
        } else {
            self.total_write_wait_time_nanos as f64 / write_wait_count as f64
        }
    }
}

// NOTES:
// - We are implementing `Add` for `&CacheStats` instead of `CacheStats`. This is
//   because we need `CacheStats` to be object-safe, therefore it cannot have
//   methods with _owned_ `Self` as a parameter. We use `&CacheStats` here to turn
//   the `Self` parameter into a _reference_.
// - By the same reason, we cannot implement `std::iter::Sum` for `CacheStats`.
//   Moreover, we cannot implement `std::iter::Sum trait` for `&CacheStats` because
//   it requires to return `&CacheStats` which is not possible under the ownership
//   rules.
impl Add for &CacheStats {
    type Output = CacheStats;

    fn add(self, rhs: Self) -> Self::Output {
        CacheStats {
            hit_count: self.hit_count.saturating_add(rhs.hit_count),
            miss_count: self.miss_count.saturating_add(rhs.miss_count),
            load_success_count: self
                .load_success_count
                .saturating_add(rhs.load_success_count),
            load_failure_count: self
                .load_failure_count
                .saturating_add(rhs.load_failure_count),
            total_load_time_nanos: self
                .total_load_time_nanos
                .saturating_add(rhs.total_load_time_nanos),
            eviction_by_size_count: self
                .eviction_by_size_count
                .saturating_add(rhs.eviction_by_size_count),
            eviction_by_size_weight: self
                .eviction_by_size_weight
                .saturating_add(rhs.eviction_by_size_weight),
            eviction_by_expiration_count: self
                .eviction_by_expiration_count
                .saturating_add(rhs.eviction_by_expiration_count),
            eviction_by_expiration_weight: self
                .eviction_by_expiration_weight
                .saturating_add(rhs.eviction_by_expiration_weight),
            read_drop_count: self.read_drop_count.saturating_add(rhs.read_drop_count),
            write_wait_count: self.write_wait_count.saturating_add(rhs.write_wait_count),
            total_write_wait_time_nanos: self
                .total_write_wait_time_nanos
                .saturating_add(rhs.total_write_wait_time_nanos),
        }
    }
}

impl Sub for CacheStats {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            hit_count: self.hit_count.saturating_sub(rhs.hit_count),
            miss_count: self.miss_count.saturating_sub(rhs.miss_count),
            load_success_count: self
                .load_success_count
                .saturating_sub(rhs.load_success_count),
            load_failure_count: self
                .load_failure_count
                .saturating_sub(rhs.load_failure_count),
            total_load_time_nanos: self
                .total_load_time_nanos
                .saturating_sub(rhs.total_load_time_nanos),
            eviction_by_size_count: self
                .eviction_by_size_count
                .saturating_sub(rhs.eviction_by_size_count),
            eviction_by_size_weight: self
                .eviction_by_size_weight
                .saturating_sub(rhs.eviction_by_size_weight),
            eviction_by_expiration_count: self
                .eviction_by_expiration_count
                .saturating_sub(rhs.eviction_by_expiration_count),
            eviction_by_expiration_weight: self
                .eviction_by_expiration_weight
                .saturating_sub(rhs.eviction_by_expiration_weight),
            read_drop_count: self.read_drop_count.saturating_sub(rhs.read_drop_count),
            write_wait_count: self.write_wait_count.saturating_sub(rhs.write_wait_count),
            total_write_wait_time_nanos: self
                .total_write_wait_time_nanos
                .saturating_sub(rhs.total_write_wait_time_nanos),
        }
    }
}
pub trait StatsCounter {
    type Stats;

    fn record_hits(&self, count: u32);
    fn record_misses(&self, count: u32);
    fn record_load_success(&self, load_time_nanos: u64);
    fn record_load_failure(&self, load_time_nanos: u64);
    fn record_eviction(&self, weight: u32, cause: RemovalCause);
    fn record_read_drop(&self);
    fn record_write_wait(&self, write_time_nanos: u64);
    fn snapshot(&self) -> Self::Stats;
}

/// A `StatsCounter` that does not record any cache events.
#[derive(Default)]
pub struct DisabledStatsCounter;

impl StatsCounter for DisabledStatsCounter {
    type Stats = CacheStats;

    fn record_hits(&self, _count: u32) {}
    fn record_misses(&self, _count: u32) {}
    fn record_load_success(&self, _load_time_nanos: u64) {}
    fn record_load_failure(&self, _load_time_nanos: u64) {}
    fn record_eviction(&self, _weight: u32, _cause: RemovalCause) {}
    fn record_read_drop(&self) {}
    fn record_write_wait(&self, _write_time_nanos: u64) {}
    fn snapshot(&self) -> Self::Stats {
        // Return a `CacheStats` with all fields set to 0.
        CacheStats::default()
    }
}

/// A `StatsCounter` that records cache events in a thread-safe way.
#[derive(Default)]
pub struct ConcurrentStatsCounter {
    hit_count: AtomicCell<u64>,
    miss_count: AtomicCell<u64>,
    load_success_count: AtomicCell<u64>,
    load_failure_count: AtomicCell<u64>,
    total_load_time: AtomicCell<u64>,
    eviction_by_size_count: AtomicCell<u64>,
    eviction_by_size_weight: AtomicCell<u64>,
    eviction_by_expiration_count: AtomicCell<u64>,
    eviction_by_expiration_weight: AtomicCell<u64>,
    read_drop_count: AtomicCell<u64>,
    write_wait_count: AtomicCell<u64>,
    total_write_wait_time_nanos: AtomicCell<u64>,
}

impl StatsCounter for ConcurrentStatsCounter {
    type Stats = CacheStats;

    fn record_hits(&self, count: u32) {
        Self::saturating_add(&self.hit_count, count as u64);
    }

    fn record_misses(&self, count: u32) {
        Self::saturating_add(&self.miss_count, count as u64);
    }

    fn record_load_success(&self, load_time_nanos: u64) {
        Self::saturating_add(&self.load_success_count, 1);
        Self::saturating_add(&self.total_load_time, load_time_nanos);
    }

    fn record_load_failure(&self, load_time_nanos: u64) {
        Self::saturating_add(&self.load_failure_count, 1);
        Self::saturating_add(&self.total_load_time, load_time_nanos);
    }

    /// Increments the `eviction_count` and `eviction_weight` only when the `cause`
    /// is `Expired` or `Size`.
    fn record_eviction(&self, weight: u32, cause: RemovalCause) {
        match cause {
            RemovalCause::Size => {
                Self::saturating_add(&self.eviction_by_size_count, 1);
                Self::saturating_add(&self.eviction_by_size_weight, weight as u64);
            }
            RemovalCause::Expired => {
                Self::saturating_add(&self.eviction_by_expiration_count, 1);
                Self::saturating_add(&self.eviction_by_expiration_weight, weight as u64);
            }
            _ => (),
        }
    }

    fn record_read_drop(&self) {
        Self::saturating_add(&self.read_drop_count, 1);
    }

    fn record_write_wait(&self, write_time_nanos: u64) {
        Self::saturating_add(&self.write_wait_count, 1);
        Self::saturating_add(&self.total_write_wait_time_nanos, write_time_nanos);
    }

    fn snapshot(&self) -> CacheStats {
        let mut stats = CacheStats::default();
        stats.set_req_counts(self.hit_count.load(), self.miss_count.load());
        stats.set_load_counts(
            self.load_success_count.load(),
            self.load_failure_count.load(),
            self.total_load_time.load(),
        );
        stats.set_eviction_counts(
            self.eviction_by_size_count.load(),
            self.eviction_by_size_weight.load(),
            self.eviction_by_expiration_count.load(),
            self.eviction_by_expiration_weight.load(),
        );
        stats.set_read_drop_count(self.read_drop_count.load());
        stats.set_write_wait_count(
            self.write_wait_count.load(),
            self.total_write_wait_time_nanos.load(),
        );
        stats
    }
}

impl ConcurrentStatsCounter {
    fn saturating_add(counter: &AtomicCell<u64>, value: u64) {
        let mut v0 = counter.load();
        loop {
            let v1 = v0.saturating_add(value);
            match counter.compare_exchange(v0, v1) {
                Ok(_) => break,
                Err(v2) => v0 = v2,
            }
        }
    }
}

/// A `StatsCounter` that wraps an array of another `StatsCounter` type to improve
/// concurrency.
pub struct StripedStatsCounter<C> {
    // In order to reduce the chances that processors invalidate the cache line of
    // each other on every modifications, we pad each counter with enough bytes
    // calculated by `crossbeam_utils::CachePadded`.
    counters: Box<[CachePadded<C>]>,
}

// NOTE:
// - We use a fixed number of counters here, which is the number of processors.
// - We might want to learn from the implementation of Java JDK `LongAdder` and its
//   super class `Striped64`:
//    - They use a dynamically sized array of counters. And each client threads will
//      search a slot in the array, which will not likely to collide with updates
//      from other threads.
//    - See the source code comments in `Striped64`.
//    - https://minddotout.wordpress.com/2013/05/11/java-8-concurrency-longadder/
//    - https://hg.openjdk.org/jdk8/jdk8/jdk/file/7b4721e4edb4/src/share/classes/java/util/concurrent/atomic/LongAdder.java
//    - https://hg.openjdk.org/jdk8/jdk8/jdk/file/7b4721e4edb4/src/share/classes/java/util/concurrent/atomic/Striped64.java

static NUM_COUNTERS: Lazy<usize> = Lazy::new(crate::common::available_parallelism);

impl<C> Default for StripedStatsCounter<C>
where
    C: Default,
{
    fn default() -> Self {
        let counters = std::iter::repeat_with(Default::default)
            .take(*NUM_COUNTERS)
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Self { counters }
    }
}

impl<C> StatsCounter for StripedStatsCounter<C>
where
    C: StatsCounter,
    for<'a> &'a C::Stats: Add<Output = C::Stats>,
{
    type Stats = C::Stats;

    fn record_hits(&self, count: u32) {
        self.counter().record_hits(count);
    }

    fn record_misses(&self, count: u32) {
        self.counter().record_misses(count);
    }

    fn record_load_success(&self, load_time_nanos: u64) {
        self.counter().record_load_success(load_time_nanos);
    }

    fn record_load_failure(&self, load_time_nanos: u64) {
        self.counter().record_load_failure(load_time_nanos)
    }

    fn record_eviction(&self, weight: u32, _cause: RemovalCause) {
        self.counter().record_eviction(weight, _cause);
    }

    fn record_read_drop(&self) {
        self.counter().record_read_drop();
    }

    fn record_write_wait(&self, write_time_nanos: u64) {
        self.counter().record_write_wait(write_time_nanos);
    }

    fn snapshot(&self) -> Self::Stats {
        let mut iter = self.counters.iter();
        let first = iter.next().expect("There is no counter").snapshot();
        iter.fold(first, |acc, counter| &acc + &counter.snapshot())
    }
}

impl<C> StripedStatsCounter<C> {
    // fn with_new_fn(f: impl Fn() -> C) -> Self {}

    /// Returns the counter `C` for the current thread.
    fn counter(&self) -> &C {
        thread_local! { static MY_INDEX: usize = next_index() };
        MY_INDEX.with(|i| &self.counters[*i])
    }
}

fn next_index() -> usize {
    static INDEX: Lazy<AtomicUsize> = Lazy::new(Default::default);

    let mut i0 = INDEX.load(Ordering::Acquire);
    loop {
        let i1 = (i0 + 1) % *NUM_COUNTERS;
        match INDEX.compare_exchange_weak(i0, i1, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return i0,
            Err(i2) => i0 = i2,
        }
    }
}
