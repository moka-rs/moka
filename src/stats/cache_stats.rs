use std::{
    fmt::{self, Debug},
    ops::{Add, Sub},
};

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

#[derive(Clone, Default, PartialEq, Eq)]
pub struct DetailedCacheStats {
    base: CacheStats,
    read_drop_count: u64,
    write_wait_count: u64,
    total_write_wait_time_nanos: u64,
}

impl Debug for DetailedCacheStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DetailedCacheStats")
            .field("request_count", &self.request_count())
            .field("hit_count", &self.hit_count())
            .field("hit_rate", &self.hit_rate())
            .field("miss_count", &self.miss_count())
            .field("miss_rate", &self.miss_rate())
            .field("load_count", &self.load_count())
            .field("load_success_count", &self.load_success_count())
            .field("load_failure_count", &self.load_failure_count())
            .field("load_failure_rate", &self.load_failure_rate())
            .field("total_load_time_nanos", &self.total_load_time_nanos())
            .field(
                "average_load_penalty_nanos",
                &self.average_load_penalty_nanos(),
            )
            .field("eviction_by_size_count", &self.eviction_by_size_count())
            .field("eviction_by_size_weight", &self.eviction_by_size_weight())
            .field(
                "eviction_by_expiration_count",
                &self.eviction_by_expiration_count(),
            )
            .field(
                "eviction_by_expiration_weight",
                &self.eviction_by_expiration_weight(),
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

impl DetailedCacheStats {
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
        self.base.request_count()
    }

    pub fn hit_count(&self) -> u64 {
        self.base.hit_count()
    }

    pub fn hit_rate(&self) -> f64 {
        self.base.hit_rate()
    }

    pub fn miss_count(&self) -> u64 {
        self.base.miss_count()
    }

    pub fn miss_rate(&self) -> f64 {
        self.base.miss_rate()
    }

    pub fn load_count(&self) -> u64 {
        self.base.load_count()
    }

    pub fn load_success_count(&self) -> u64 {
        self.base.load_success_count()
    }

    pub fn load_failure_count(&self) -> u64 {
        self.base.load_failure_count()
    }

    pub fn load_failure_rate(&self) -> f64 {
        self.base.load_failure_rate()
    }

    pub fn total_load_time_nanos(&self) -> u64 {
        self.base.total_load_time_nanos()
    }

    pub fn average_load_penalty_nanos(&self) -> f64 {
        self.base.average_load_penalty_nanos()
    }

    pub fn eviction_by_size_count(&self) -> u64 {
        self.base.eviction_by_size_count()
    }

    pub fn eviction_by_size_weight(&self) -> u64 {
        self.base.eviction_by_size_weight()
    }

    pub fn eviction_by_expiration_count(&self) -> u64 {
        self.base.eviction_by_expiration_count()
    }

    pub fn eviction_by_expiration_weight(&self) -> u64 {
        self.base.eviction_by_expiration_weight()
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

impl From<CacheStats> for DetailedCacheStats {
    fn from(stats: CacheStats) -> Self {
        Self {
            base: stats,
            read_drop_count: 0,
            write_wait_count: 0,
            total_write_wait_time_nanos: 0,
        }
    }
}

impl Add for &DetailedCacheStats {
    type Output = DetailedCacheStats;

    fn add(self, rhs: Self) -> Self::Output {
        DetailedCacheStats {
            base: &self.base + &rhs.base,
            read_drop_count: self.read_drop_count.saturating_add(rhs.read_drop_count),
            write_wait_count: self.write_wait_count.saturating_add(rhs.write_wait_count),
            total_write_wait_time_nanos: self
                .total_write_wait_time_nanos
                .saturating_add(rhs.total_write_wait_time_nanos),
        }
    }
}

impl Sub for DetailedCacheStats {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            base: self.base - rhs.base,
            read_drop_count: self.read_drop_count.saturating_sub(rhs.read_drop_count),
            write_wait_count: self.write_wait_count.saturating_sub(rhs.write_wait_count),
            total_write_wait_time_nanos: self
                .total_write_wait_time_nanos
                .saturating_sub(rhs.total_write_wait_time_nanos),
        }
    }
}
