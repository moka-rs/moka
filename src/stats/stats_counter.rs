use super::{cache_stats::DetailedCacheStats, CacheStats};
use crate::notification::RemovalCause;

use std::{
    ops::Add,
    sync::atomic::{AtomicUsize, Ordering},
};

use crossbeam_utils::{atomic::AtomicCell, CachePadded};
use once_cell::sync::Lazy;

pub fn saturating_add(counter: &AtomicCell<u64>, value: u64) {
    let mut v0 = counter.load();
    loop {
        let v1 = v0.saturating_add(value);
        match counter.compare_exchange(v0, v1) {
            Ok(_) => break,
            Err(v2) => v0 = v2,
        }
    }
}

pub trait StatsCounter {
    type Stats;

    fn is_load_time_supported(&self) -> bool {
        false
    }

    fn is_recording_evictions_supported(&self) -> bool {
        false
    }

    fn is_write_wait_time_supported(&self) -> bool {
        false
    }

    #[allow(unused_variables)]
    fn record_hits(&self, count: u32) {}

    #[allow(unused_variables)]
    fn record_misses(&self, count: u32) {}

    #[allow(unused_variables)]
    fn record_insertions(&self, count: u32) {}

    #[allow(unused_variables)]
    fn record_load_success(&self, load_time_nanos: u64) {}

    #[allow(unused_variables)]
    fn record_load_failure(&self, load_time_nanos: u64) {}

    #[allow(unused_variables)]
    fn record_eviction(&self, weight: u32, cause: RemovalCause) {}

    #[allow(unused_variables)]
    fn record_read_drop(&self) {}

    #[allow(unused_variables)]
    fn record_write_wait(&self, write_time_nanos: u64) {}

    fn snapshot(&self) -> Self::Stats;
}

/// A `StatsCounter` that does not record any cache events.
#[derive(Default)]
pub struct DisabledStatsCounter;

impl StatsCounter for DisabledStatsCounter {
    type Stats = CacheStats;

    fn snapshot(&self) -> Self::Stats {
        // Return a `CacheStats` with all fields set to 0.
        CacheStats::default()
    }
}

#[derive(Default)]
pub struct DefaultStatsCounter {
    hit_count: AtomicCell<u64>,
    miss_count: AtomicCell<u64>,
    load_success_count: AtomicCell<u64>,
    load_failure_count: AtomicCell<u64>,
    total_load_time: AtomicCell<u64>,
    eviction_by_size_count: AtomicCell<u64>,
    eviction_by_size_weight: AtomicCell<u64>,
    eviction_by_expiration_count: AtomicCell<u64>,
    eviction_by_expiration_weight: AtomicCell<u64>,
}

impl DefaultStatsCounter {
    pub fn striped() -> StripedStatsCounter<Self> {
        Default::default()
    }
}

impl StatsCounter for DefaultStatsCounter {
    type Stats = CacheStats;

    fn is_load_time_supported(&self) -> bool {
        true
    }

    fn is_recording_evictions_supported(&self) -> bool {
        true
    }

    fn record_hits(&self, count: u32) {
        saturating_add(&self.hit_count, count as u64);
    }

    fn record_misses(&self, count: u32) {
        saturating_add(&self.miss_count, count as u64);
    }

    fn record_load_success(&self, load_time_nanos: u64) {
        saturating_add(&self.load_success_count, 1);
        saturating_add(&self.total_load_time, load_time_nanos);
    }

    fn record_load_failure(&self, load_time_nanos: u64) {
        saturating_add(&self.load_failure_count, 1);
        saturating_add(&self.total_load_time, load_time_nanos);
    }

    /// Increments the `eviction_count` and `eviction_weight` only when the `cause`
    /// is `Expired` or `Size`.
    fn record_eviction(&self, weight: u32, cause: RemovalCause) {
        match cause {
            RemovalCause::Size => {
                saturating_add(&self.eviction_by_size_count, 1);
                saturating_add(&self.eviction_by_size_weight, weight as u64);
            }
            RemovalCause::Expired => {
                saturating_add(&self.eviction_by_expiration_count, 1);
                saturating_add(&self.eviction_by_expiration_weight, weight as u64);
            }
            _ => (),
        }
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
        stats
    }
}

#[derive(Default)]
pub struct DetailedStatsCounter {
    base: DefaultStatsCounter,
    insertion_count: AtomicCell<u64>,
    invalidation_count: AtomicCell<u64>,
    read_drop_count: AtomicCell<u64>,
    write_wait_count: AtomicCell<u64>,
    total_write_wait_time_nanos: AtomicCell<u64>,
}

impl DetailedStatsCounter {
    pub fn striped() -> StripedStatsCounter<Self> {
        Default::default()
    }
}

impl StatsCounter for DetailedStatsCounter {
    type Stats = DetailedCacheStats;

    fn is_load_time_supported(&self) -> bool {
        true
    }

    fn is_recording_evictions_supported(&self) -> bool {
        true
    }

    fn is_write_wait_time_supported(&self) -> bool {
        true
    }

    fn record_hits(&self, count: u32) {
        self.base.record_hits(count);
    }

    fn record_misses(&self, count: u32) {
        self.base.record_misses(count);
    }

    fn record_insertions(&self, count: u32) {
        saturating_add(&self.insertion_count, count as u64);
    }

    fn record_load_success(&self, load_time_nanos: u64) {
        self.base.record_load_success(load_time_nanos);
    }

    fn record_load_failure(&self, load_time_nanos: u64) {
        self.base.record_load_failure(load_time_nanos);
    }

    /// Increments the `eviction_count` and `eviction_weight` only when the `cause`
    /// is `Expired` or `Size`.
    fn record_eviction(&self, weight: u32, cause: RemovalCause) {
        if cause == RemovalCause::Explicit {
            saturating_add(&self.invalidation_count, 1);
        } else {
            self.base.record_eviction(weight, cause);
        }
    }

    fn record_read_drop(&self) {
        saturating_add(&self.read_drop_count, 1);
    }

    fn record_write_wait(&self, write_time_nanos: u64) {
        saturating_add(&self.write_wait_count, 1);
        saturating_add(&self.total_write_wait_time_nanos, write_time_nanos);
    }

    fn snapshot(&self) -> DetailedCacheStats {
        let mut stats: DetailedCacheStats = self.base.snapshot().into();
        stats.set_insertion_and_invalidation_counts(
            self.insertion_count.load(),
            self.invalidation_count.load(),
        );
        stats.set_read_drop_count(self.read_drop_count.load());
        stats.set_write_wait_count(
            self.write_wait_count.load(),
            self.total_write_wait_time_nanos.load(),
        );
        stats
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
        Self::new_with(Default::default)
    }
}

impl<C> StripedStatsCounter<C> {
    pub fn new_with(f: impl FnMut() -> C) -> Self {
        let counters = std::iter::repeat_with(f)
            .map(CachePadded::new)
            .take(*NUM_COUNTERS)
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Self { counters }
    }

    /// Returns the counter `C` for the current thread.
    fn counter(&self) -> &C {
        thread_local! { static MY_INDEX: usize = next_index() };
        MY_INDEX.with(|i| &self.counters[*i])
    }
}

impl<C> StatsCounter for StripedStatsCounter<C>
where
    C: StatsCounter,
    for<'a> &'a C::Stats: Add<Output = C::Stats>,
{
    type Stats = C::Stats;

    fn is_load_time_supported(&self) -> bool {
        self.counter().is_load_time_supported()
    }

    fn is_recording_evictions_supported(&self) -> bool {
        self.counter().is_recording_evictions_supported()
    }

    fn is_write_wait_time_supported(&self) -> bool {
        self.counter().is_write_wait_time_supported()
    }

    fn record_hits(&self, count: u32) {
        self.counter().record_hits(count);
    }

    fn record_misses(&self, count: u32) {
        self.counter().record_misses(count);
    }

    fn record_insertions(&self, count: u32) {
        self.counter().record_insertions(count);
    }

    fn record_load_success(&self, load_time_nanos: u64) {
        self.counter().record_load_success(load_time_nanos);
    }

    fn record_load_failure(&self, load_time_nanos: u64) {
        self.counter().record_load_failure(load_time_nanos)
    }

    fn record_eviction(&self, weight: u32, cause: RemovalCause) {
        self.counter().record_eviction(weight, cause);
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
