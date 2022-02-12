use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_utils::{atomic::AtomicCell, CachePadded};
use once_cell::sync::Lazy;

use crate::stats::CacheStats;
use crate::RemovalCause;

pub trait StatsCounter {
    fn record_hits(&self, count: u32);
    fn record_misses(&self, count: u32);
    fn record_load_success(&self, load_time_nanos: u64);
    fn record_load_failure(&self, load_time_nanos: u64);
    fn record_eviction(&self, weight: u32, cause: RemovalCause);
    fn snapshot(&self) -> CacheStats;
}

static NUM_COUNTERS: Lazy<usize> = Lazy::new(|| crate::common::num_cpus() * 2);

pub struct SaturatingStatsCounter {
    request_counters: Box<[CachePadded<RequestCounter>]>,
    eviction_counter: CachePadded<EvictionCounter>,
}

#[derive(Default)]
struct RequestCounter {
    hit_count: AtomicCell<u64>,
    miss_count: AtomicCell<u64>,
    load_success_count: AtomicCell<u64>,
    load_failure_count: AtomicCell<u64>,
    total_load_time: AtomicCell<u64>,
}

#[derive(Default)]
struct EvictionCounter {
    eviction_count: AtomicCell<u64>,
    eviction_weight: AtomicCell<u64>,
}

impl Default for SaturatingStatsCounter {
    fn default() -> Self {
        let request_counters = std::iter::repeat_with(Default::default)
            .take(*NUM_COUNTERS)
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Self {
            request_counters,
            eviction_counter: Default::default(),
        }
    }
}

impl StatsCounter for SaturatingStatsCounter {
    fn record_hits(&self, count: u32) {
        let counter = &self.request_counter().hit_count;
        saturating_add(counter, count as u64);
    }

    fn record_misses(&self, count: u32) {
        let counter = &self.request_counter().miss_count;
        saturating_add(counter, count as u64);
    }

    fn record_load_success(&self, load_time_nanos: u64) {
        let req_counter = self.request_counter();

        let success_count = &req_counter.load_success_count;
        saturating_add(success_count, 1);

        let load_time = &req_counter.total_load_time;
        saturating_add(load_time, load_time_nanos);
    }

    fn record_load_failure(&self, load_time_nanos: u64) {
        let req_counter = self.request_counter();

        let failure_count = &req_counter.load_failure_count;
        saturating_add(failure_count, 1);

        let load_time = &req_counter.total_load_time;
        saturating_add(load_time, load_time_nanos);
    }

    fn record_eviction(&self, weight: u32, _cause: RemovalCause) {
        let ev_counter = &self.eviction_counter;
        saturating_add(&ev_counter.eviction_count, 1);
        saturating_add(&ev_counter.eviction_weight, weight as u64);
    }

    fn snapshot(&self) -> CacheStats {
        let hit_count = self.sum_counters(|c| c.hit_count.load());
        let miss_count = self.sum_counters(|c| c.miss_count.load());
        let load_success_count = self.sum_counters(|c| c.load_success_count.load());
        let load_failure_count = self.sum_counters(|c| c.load_failure_count.load());
        let total_load_time = self.sum_counters(|c| c.total_load_time.load());
        let eviction_count = self.eviction_counter.eviction_count.load();
        let eviction_weight = self.eviction_counter.eviction_weight.load();

        let mut stats = CacheStats::default();
        stats
            .set_req_counts(hit_count, miss_count)
            .set_load_counts(load_success_count, load_failure_count, total_load_time)
            .set_eviction_count(eviction_count, eviction_weight);
        stats
    }
}

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

impl SaturatingStatsCounter {
    fn request_counter(&self) -> &RequestCounter {
        thread_local! { static INDEX: usize = next_index() };
        INDEX.with(|i| &self.request_counters[*i])
    }

    fn sum_counters(&self, mut selector: impl FnMut(&RequestCounter) -> u64) -> u64 {
        self.request_counters
            .iter()
            .fold(0, |acc, counter| acc.saturating_add(selector(counter)))
    }
}

static INDEX: Lazy<AtomicUsize> = Lazy::new(Default::default);

fn next_index() -> usize {
    let mut i0 = INDEX.load(Ordering::Acquire);
    loop {
        let mut i1 = i0 + 1;
        if i1 >= *NUM_COUNTERS {
            i1 = 0;
        }
        match INDEX.compare_exchange_weak(i0, i1, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return i0,
            Err(i2) => i0 = i2,
        }
    }
}
