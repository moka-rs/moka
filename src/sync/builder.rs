use crate::common::builder_utils;
use super::{Cache, SegmentedCache};

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    time::Duration,
};

/// Builds a [`Cache`][cache-struct] or [`SegmentedCache`][seg-cache-struct]
/// with various configuration knobs.
///
/// [cache-struct]: ./struct.Cache.html
/// [seg-cache-struct]: ./struct.SegmentedCache.html
///
/// # Examples
///
/// ```rust
/// use moka::sync::CacheBuilder;
///
/// use std::time::Duration;
///
/// let cache = CacheBuilder::new(10_000) // Max 10,000 elements
///     // Time to live (TTL): 30 minutes
///     .time_to_live(Duration::from_secs(30 * 60))
///     // Time to idle (TTI):  5 minutes
///     .time_to_idle(Duration::from_secs( 5 * 60))
///     // Create the cache.
///     .build();
///
/// // This entry will expire after 5 minutes (TTI) if there is no get().
/// cache.insert(0, "zero");
///
/// // This get() will extend the entry life for another 5 minutes.
/// cache.get(&0);
///
/// // Even though we keep calling get(), the entry will expire
/// // after 30 minutes (TTL) from the insert().
/// ```
///
pub struct CacheBuilder<C> {
    max_capacity: usize,
    initial_capacity: Option<usize>,
    num_segments: Option<usize>,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    invalidator_enabled: bool,
    cache_type: PhantomData<C>,
}

impl<K, V> CacheBuilder<Cache<K, V, RandomState>>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Construct a new `CacheBuilder` that will be used to build a `Cache` or
    /// `SegmentedCache` holding up to `max_capacity` entries.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            max_capacity,
            initial_capacity: None,
            num_segments: None,
            time_to_live: None,
            time_to_idle: None,
            invalidator_enabled: false,
            cache_type: PhantomData::default(),
        }
    }

    /// Sets the number of segments of the cache.
    ///
    /// # Panics
    ///
    /// Panics if `num_segments` is less than or equals to 1.
    pub fn segments(self, num_segments: usize) -> CacheBuilder<SegmentedCache<K, V, RandomState>> {
        assert!(num_segments > 1);

        CacheBuilder {
            max_capacity: self.max_capacity,
            initial_capacity: self.initial_capacity,
            num_segments: Some(num_segments),
            time_to_live: self.time_to_live,
            time_to_idle: self.time_to_idle,
            invalidator_enabled: self.invalidator_enabled,
            cache_type: PhantomData::default(),
        }
    }

    /// Builds a `Cache<K, V>`.
    ///
    /// If you want to build a `SegmentedCache<K, V>`, call `segments` method before
    /// calling this method.
    ///
    /// # Panics
    ///
    /// Panics if configured with either `time_to_live` or `time_to_idle` higher than
    /// 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn build(self) -> Cache<K, V, RandomState> {
        let build_hasher = RandomState::default();
        builder_utils::check_ttl_and_tti(self.time_to_live, self.time_to_idle);
        Cache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            build_hasher,
            self.time_to_live,
            self.time_to_idle,
            self.invalidator_enabled,
        )
    }

    /// Builds a `Cache<K, V, S>`, with the given `hasher`.
    ///
    /// If you want to build a `SegmentedCache<K, V>`, call `segments` method  before
    /// calling this method.
    ///
    /// # Panics
    ///
    /// Panics if configured with either `time_to_live` or `time_to_idle` higher than
    /// 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn build_with_hasher<S>(self, hasher: S) -> Cache<K, V, S>
    where
        S: BuildHasher + Clone + Send + Sync + 'static,
    {
        builder_utils::check_ttl_and_tti(self.time_to_live, self.time_to_idle);
        Cache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            hasher,
            self.time_to_live,
            self.time_to_idle,
            self.invalidator_enabled,
        )
    }
}

impl<K, V> CacheBuilder<SegmentedCache<K, V, RandomState>>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Builds a `SegmentedCache<K, V>`.
    ///
    /// If you want to build a `Cache<K, V>`, do not call `segments` method before
    /// calling this method.
    ///
    /// # Panics
    ///
    /// Panics if configured with either `time_to_live` or `time_to_idle` higher than
    /// 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn build(self) -> SegmentedCache<K, V, RandomState> {
        let build_hasher = RandomState::default();
        builder_utils::check_ttl_and_tti(self.time_to_live, self.time_to_idle);
        SegmentedCache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            self.num_segments.unwrap(),
            build_hasher,
            self.time_to_live,
            self.time_to_idle,
            self.invalidator_enabled,
        )
    }

    /// Builds a `SegmentedCache<K, V, S>`, with the given `hasher`.
    ///
    /// If you want to build a `Cache<K, V>`, do not call `segments` method before
    /// calling this method.
    ///
    /// # Panics
    ///
    /// Panics if configured with either `time_to_live` or `time_to_idle` higher than
    /// 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn build_with_hasher<S>(self, hasher: S) -> SegmentedCache<K, V, S>
    where
        S: BuildHasher + Clone + Send + Sync + 'static,
    {
        builder_utils::check_ttl_and_tti(self.time_to_live, self.time_to_idle);
        SegmentedCache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            self.num_segments.unwrap(),
            hasher,
            self.time_to_live,
            self.time_to_idle,
            self.invalidator_enabled,
        )
    }
}

impl<C> CacheBuilder<C> {
    /// Sets the initial capacity of the cache.
    pub fn initial_capacity(self, capacity: usize) -> Self {
        Self {
            initial_capacity: Some(capacity),
            ..self
        }
    }

    /// Sets the time to live of the cache.
    ///
    /// A cached entry will be expired after the specified duration past from
    /// `insert`.
    ///
    /// # Panics
    ///
    /// `CacheBuilder::build*` methods will panic if the given `duration` is longer
    /// than 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn time_to_live(self, duration: Duration) -> Self {
        Self {
            time_to_live: Some(duration),
            ..self
        }
    }

    /// Sets the time to idle of the cache.
    ///
    /// A cached entry will be expired after the specified duration past from `get`
    /// or `insert`.
    ///
    /// # Panics
    ///
    /// `CacheBuilder::build*` methods will panic if the given `duration` is longer
    /// than 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn time_to_idle(self, duration: Duration) -> Self {
        Self {
            time_to_idle: Some(duration),
            ..self
        }
    }

    /// Enables support for [Cache::invalidate_entries_if][cache-invalidate-if]
    /// method.
    ///
    /// The cache will maintain additional internal data structures to support
    /// `invalidate_entries_if` method.
    ///
    /// [cache-invalidate-if]: ./struct.Cache.html#method.invalidate_entries_if
    pub fn support_invalidation_closures(self) -> Self {
        Self {
            invalidator_enabled: true,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;
    use super::CacheBuilder;

    use std::time::Duration;

    #[test]
    fn build_cache() {
        // Cache<char, String>
        let cache = CacheBuilder::new(100).build();

        assert_eq!(cache.max_capacity(), 100);
        assert_eq!(cache.time_to_live(), None);
        assert_eq!(cache.time_to_idle(), None);
        assert_eq!(cache.num_segments(), 1);

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some("Alice"));

        let cache = CacheBuilder::new(100)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();

        assert_eq!(cache.max_capacity(), 100);
        assert_eq!(cache.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(cache.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(cache.num_segments(), 1);

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some("Alice"));
    }

    #[test]
    fn build_segmented_cache() {
        // SegmentCache<char, String>
        let cache = CacheBuilder::new(100).segments(16).build();

        assert_eq!(cache.max_capacity(), 100);
        assert_eq!(cache.time_to_live(), None);
        assert_eq!(cache.time_to_idle(), None);
        assert_eq!(cache.num_segments(), 16_usize.next_power_of_two());

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some("Bob"));

        let cache = CacheBuilder::new(100)
            .segments(16)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();

        assert_eq!(cache.max_capacity(), 100);
        assert_eq!(cache.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(cache.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(cache.num_segments(), 16_usize.next_power_of_two());

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some("Bob"));
    }

    #[test]
    #[should_panic(expected = "time_to_live is longer than 1000 years")]
    fn build_cache_too_long_ttl() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<Cache<char, String>> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_live(duration + Duration::from_secs(1))
            .build();
    }

    #[test]
    #[should_panic(expected = "time_to_idle is longer than 1000 years")]
    fn build_cache_too_long_tti() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<Cache<char, String>> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_idle(duration + Duration::from_secs(1))
            .build();
    }
}
