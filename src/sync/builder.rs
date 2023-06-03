use super::{Cache, SegmentedCache};
use crate::{
    common::{builder_utils, concurrent::Weigher},
    notification::{self, EvictionListener, RemovalCause},
    policy::ExpirationPolicy,
    stats::{
        stats_counter::{
            DefaultStatsCounter, DisabledStatsCounter, StatsCounter, StripedStatsCounter,
        },
        CacheStats,
    },
    Expiry,
};

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    sync::Arc,
    time::Duration,
};

/// Builds a [`Cache`][cache-struct] or [`SegmentedCache`][seg-cache-struct]
/// with various configuration knobs.
///
/// [cache-struct]: ./struct.Cache.html
/// [seg-cache-struct]: ./struct.SegmentedCache.html
///
/// # Example: Expirations
///
/// ```rust
/// use moka::sync::Cache;
/// use std::time::Duration;
///
/// let cache = Cache::builder()
///     // Max 10,000 entries
///     .max_capacity(10_000)
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
#[must_use]
pub struct CacheBuilder<K, V, C, CS> {
    name: Option<String>,
    max_capacity: Option<u64>,
    initial_capacity: Option<usize>,
    num_segments: Option<usize>,
    weigher: Option<Weigher<K, V>>,
    eviction_listener: Option<EvictionListener<K, V>>,
    eviction_listener_conf: Option<notification::Configuration>,
    expiration_policy: ExpirationPolicy<K, V>,
    stats_counter: Arc<dyn StatsCounter<Stats = CS> + Send + Sync>,
    invalidator_enabled: bool,
    thread_pool_enabled: bool,
    cache_type: PhantomData<C>,
}

impl<K, V> Default for CacheBuilder<K, V, Cache<K, V, RandomState, CacheStats>, CacheStats>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self {
            name: None,
            max_capacity: None,
            initial_capacity: None,
            num_segments: None,
            weigher: None,
            eviction_listener: None,
            eviction_listener_conf: None,
            expiration_policy: Default::default(),
            stats_counter: Arc::new(DisabledStatsCounter::default()),
            invalidator_enabled: false,
            // TODO: Change this to `false` in Moka v0.12.0 or v0.13.0.
            thread_pool_enabled: true,
            cache_type: Default::default(),
        }
    }
}

impl<K, V> CacheBuilder<K, V, Cache<K, V, RandomState, CacheStats>, CacheStats>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Construct a new `CacheBuilder` that will be used to build a `Cache` or
    /// `SegmentedCache` holding up to `max_capacity` entries.
    pub fn new(max_capacity: u64) -> Self {
        Self {
            max_capacity: Some(max_capacity),
            ..Default::default()
        }
    }

    pub fn enable_stats(self) -> Self {
        Self {
            stats_counter: Arc::<StripedStatsCounter<DefaultStatsCounter>>::default(),
            ..self
        }
    }
}

impl<K, V, CS> CacheBuilder<K, V, Cache<K, V, RandomState, CS>, CS>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    CS: 'static,
{
    /// Sets the number of segments of the cache.
    ///
    /// # Panics
    ///
    /// Panics if `num_segments` is zero.
    pub fn segments(
        self,
        num_segments: usize,
    ) -> CacheBuilder<K, V, SegmentedCache<K, V, RandomState>, CS> {
        assert!(num_segments != 0);

        CacheBuilder {
            name: self.name,
            max_capacity: self.max_capacity,
            initial_capacity: self.initial_capacity,
            num_segments: Some(num_segments),
            weigher: self.weigher,
            eviction_listener: self.eviction_listener,
            eviction_listener_conf: self.eviction_listener_conf,
            expiration_policy: self.expiration_policy,
            stats_counter: self.stats_counter,
            invalidator_enabled: self.invalidator_enabled,
            thread_pool_enabled: self.thread_pool_enabled,
            cache_type: PhantomData,
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
    pub fn build(self) -> Cache<K, V, RandomState, CS> {
        let build_hasher = RandomState::default();
        let exp = &self.expiration_policy;
        builder_utils::ensure_expirations_or_panic(exp.time_to_live(), exp.time_to_idle());
        Cache::with_everything(
            self.name,
            self.max_capacity,
            self.initial_capacity,
            build_hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
            self.expiration_policy,
            self.stats_counter,
            self.invalidator_enabled,
            builder_utils::housekeeper_conf(self.thread_pool_enabled),
        )
    }

    /// Builds a `Cache<K, V, S>` with the given `hasher` of type `S`.
    ///
    /// # Examples
    ///
    /// This example uses AHash hasher from [AHash][ahash-crate] crate.
    ///
    /// [ahash-crate]: https://crates.io/crates/ahash
    ///
    /// ```rust
    /// // Cargo.toml
    /// // [dependencies]
    /// // ahash = "0.8"
    /// // moka = ...
    ///
    /// use moka::sync::Cache;
    ///
    /// // The type of this cache is: Cache<i32, String, ahash::RandomState>
    /// let cache = Cache::builder()
    ///     .max_capacity(100)
    ///     .build_with_hasher(ahash::RandomState::default());
    /// cache.insert(1, "one".to_string());
    /// ```
    ///
    /// Note: If you need to add a type annotation to your cache, you must use the
    /// form of `Cache<K, V, S>` instead of `Cache<K, V>`. That `S` is the type of
    /// the build hasher, and its default is the `RandomState` from
    /// `std::collections::hash_map` module . If you use a different build hasher,
    /// you must specify `S` explicitly.
    ///
    /// Here is a good example:
    ///
    /// ```rust
    /// # use moka::sync::Cache;
    /// # let cache = Cache::builder()
    /// #     .build_with_hasher(ahash::RandomState::default());
    /// struct Good {
    ///     // Specifying the type in Cache<K, V, S> format.
    ///     cache: Cache<i32, String, ahash::RandomState>,
    /// }
    ///
    /// // Storing the cache from above example. This should compile.
    /// Good { cache };
    /// ```
    ///
    /// Here is a bad example. This struct cannot store the above cache because it
    /// does not specify `S`:
    ///
    /// ```compile_fail
    /// # use moka::sync::Cache;
    /// # let cache = Cache::builder()
    /// #     .build_with_hasher(ahash::RandomState::default());
    /// struct Bad {
    ///     // Specifying the type in Cache<K, V> format.
    ///     cache: Cache<i32, String>,
    /// }
    ///
    /// // This should not compile.
    /// Bad { cache };
    /// // => error[E0308]: mismatched types
    /// //    expected struct `std::collections::hash_map::RandomState`,
    /// //       found struct `ahash::RandomState`
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if configured with either `time_to_live` or `time_to_idle` higher than
    /// 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn build_with_hasher<S>(self, hasher: S) -> Cache<K, V, S, CS>
    where
        S: BuildHasher + Clone + Send + Sync + 'static,
    {
        let exp = &self.expiration_policy;
        builder_utils::ensure_expirations_or_panic(exp.time_to_live(), exp.time_to_idle());
        Cache::with_everything(
            self.name,
            self.max_capacity,
            self.initial_capacity,
            hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
            self.expiration_policy,
            self.stats_counter,
            self.invalidator_enabled,
            builder_utils::housekeeper_conf(self.thread_pool_enabled),
        )
    }
}

impl<K, V, S, CS> CacheBuilder<K, V, Cache<K, V, S, CS>, CS> {
    pub fn stats_counter<CO, CS1>(self, counter: CO) -> CacheBuilder<K, V, Cache<K, V, S, CS1>, CS1>
    where
        CO: StatsCounter<Stats = CS1> + Send + Sync + 'static,
    {
        let stats_counter = Arc::new(counter);
        CacheBuilder {
            name: self.name,
            max_capacity: self.max_capacity,
            initial_capacity: self.initial_capacity,
            num_segments: self.num_segments,
            weigher: self.weigher,
            eviction_listener: self.eviction_listener,
            eviction_listener_conf: self.eviction_listener_conf,
            expiration_policy: self.expiration_policy,
            stats_counter,
            invalidator_enabled: self.invalidator_enabled,
            thread_pool_enabled: self.thread_pool_enabled,
            cache_type: PhantomData,
        }
    }
}

impl<K, V> CacheBuilder<K, V, SegmentedCache<K, V, RandomState, CacheStats>, CacheStats>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub fn enable_stats(self) -> Self {
        Self {
            stats_counter: Arc::<StripedStatsCounter<DefaultStatsCounter>>::default(),
            ..self
        }
    }
}

impl<K, V, CS> CacheBuilder<K, V, SegmentedCache<K, V, RandomState, CS>, CS>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    CS: 'static,
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
    pub fn build(self) -> SegmentedCache<K, V, RandomState, CS> {
        let build_hasher = RandomState::default();
        let exp = &self.expiration_policy;
        builder_utils::ensure_expirations_or_panic(exp.time_to_live(), exp.time_to_idle());
        SegmentedCache::with_everything(
            self.name,
            self.max_capacity,
            self.initial_capacity,
            self.num_segments.unwrap(),
            build_hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
            self.expiration_policy,
            self.stats_counter,
            self.invalidator_enabled,
            builder_utils::housekeeper_conf(self.thread_pool_enabled),
        )
    }

    /// Builds a `SegmentedCache<K, V, S>` with the given `hasher`.
    ///
    ///
    /// # Examples
    ///
    /// This example uses AHash hasher from [AHash][ahash-crate] crate.
    ///
    /// [ahash-crate]: https://crates.io/crates/ahash
    ///
    /// ```rust
    /// // Cargo.toml
    /// // [dependencies]
    /// // ahash = "0.8"
    /// // moka = ...
    ///
    /// use moka::sync::SegmentedCache;
    ///
    /// // The type of this cache is: SegmentedCache<i32, String, ahash::RandomState>
    /// let cache = SegmentedCache::builder(4)
    ///     .max_capacity(100)
    ///     .build_with_hasher(ahash::RandomState::default());
    /// cache.insert(1, "one".to_string());
    /// ```
    ///
    /// Note: If you need to add a type annotation to your cache, you must use the
    /// form of `SegmentedCache<K, V, S>` instead of `SegmentedCache<K, V>`. That `S`
    /// is the type of the build hasher, whose default is the `RandomState` from
    /// `std::collections::hash_map` module . If you use a different build hasher,
    /// you must specify `S` explicitly.
    ///
    /// Here is a good example:
    ///
    /// ```rust
    /// # use moka::sync::SegmentedCache;
    /// # let cache = SegmentedCache::builder(4)
    /// #     .build_with_hasher(ahash::RandomState::default());
    /// struct Good {
    ///     // Specifying the type in SegmentedCache<K, V, S> format.
    ///     cache: SegmentedCache<i32, String, ahash::RandomState>,
    /// }
    ///
    /// // Storing the cache from above example. This should compile.
    /// Good { cache };
    /// ```
    ///
    /// Here is a bad example. This struct cannot store the above cache because it
    /// does not specify `S`:
    ///
    /// ```compile_fail
    /// # use moka::sync::SegmentedCache;
    /// # let cache = SegmentedCache::builder(4)
    /// #     .build_with_hasher(ahash::RandomState::default());
    /// struct Bad {
    ///     // Specifying the type in SegmentedCache<K, V> format.
    ///     cache: SegmentedCache<i32, String>,
    /// }
    ///
    /// // This should not compile.
    /// Bad { cache };
    /// // => error[E0308]: mismatched types
    /// //    expected struct `std::collections::hash_map::RandomState`,
    /// //       found struct `ahash::RandomState`
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if configured with either `time_to_live` or `time_to_idle` higher than
    /// 1000 years. This is done to protect against overflow when computing key
    /// expiration.
    pub fn build_with_hasher<S>(self, hasher: S) -> SegmentedCache<K, V, S, CS>
    where
        S: BuildHasher + Clone + Send + Sync + 'static,
    {
        let exp = &self.expiration_policy;
        builder_utils::ensure_expirations_or_panic(exp.time_to_live(), exp.time_to_idle());
        SegmentedCache::with_everything(
            self.name,
            self.max_capacity,
            self.initial_capacity,
            self.num_segments.unwrap(),
            hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
            self.expiration_policy,
            self.stats_counter,
            self.invalidator_enabled,
            builder_utils::housekeeper_conf(true),
        )
    }
}

impl<K, V, S, CS> CacheBuilder<K, V, SegmentedCache<K, V, S, CS>, CS> {
    pub fn stats_counter<CO, CS1>(
        self,
        counter: CO,
    ) -> CacheBuilder<K, V, SegmentedCache<K, V, S, CS1>, CS1>
    where
        CO: StatsCounter<Stats = CS1> + Send + Sync + 'static,
    {
        let stats_counter = Arc::new(counter);
        CacheBuilder {
            name: self.name,
            max_capacity: self.max_capacity,
            initial_capacity: self.initial_capacity,
            num_segments: self.num_segments,
            weigher: self.weigher,
            eviction_listener: self.eviction_listener,
            eviction_listener_conf: self.eviction_listener_conf,
            expiration_policy: self.expiration_policy,
            stats_counter,
            invalidator_enabled: self.invalidator_enabled,
            thread_pool_enabled: self.thread_pool_enabled,
            cache_type: PhantomData,
        }
    }
}

impl<K, V, C, CS> CacheBuilder<K, V, C, CS> {
    /// Sets the name of the cache. Currently the name is used for identification
    /// only in logging messages.
    pub fn name(self, name: &str) -> Self {
        Self {
            name: Some(name.to_string()),
            ..self
        }
    }

    /// Sets the max capacity of the cache.
    pub fn max_capacity(self, max_capacity: u64) -> Self {
        Self {
            max_capacity: Some(max_capacity),
            ..self
        }
    }

    /// Sets the initial capacity (number of entries) of the cache.
    pub fn initial_capacity(self, number_of_entries: usize) -> Self {
        Self {
            initial_capacity: Some(number_of_entries),
            ..self
        }
    }

    /// Sets the weigher closure to the cache.
    ///
    /// The closure should take `&K` and `&V` as the arguments and returns a `u32`
    /// representing the relative size of the entry.
    pub fn weigher(self, weigher: impl Fn(&K, &V) -> u32 + Send + Sync + 'static) -> Self {
        Self {
            weigher: Some(Arc::new(weigher)),
            ..self
        }
    }

    /// Sets the eviction listener closure to the cache.
    ///
    /// The closure should take `Arc<K>`, `V` and [`RemovalCause`][removal-cause] as
    /// the arguments. The [immediate delivery mode][immediate-mode] is used for the
    /// listener.
    ///
    /// # Panics
    ///
    /// It is very important to make the listener closure not to panic. Otherwise,
    /// the cache will stop calling the listener after a panic. This is an intended
    /// behavior because the cache cannot know whether is is memory safe or not to
    /// call the panicked lister again.
    ///
    /// [removal-cause]: ../notification/enum.RemovalCause.html
    /// [immediate-mode]: ../notification/enum.DeliveryMode.html#variant.Immediate
    pub fn eviction_listener(
        self,
        listener: impl Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static,
    ) -> Self {
        Self {
            eviction_listener: Some(Arc::new(listener)),
            eviction_listener_conf: Some(Default::default()),
            ..self
        }
    }

    /// Sets the eviction listener closure to the cache with a custom
    /// [`Configuration`][conf]. Use this method if you want to change the delivery
    /// mode to the queued mode.
    ///
    /// The closure should take `Arc<K>`, `V` and [`RemovalCause`][removal-cause] as
    /// the arguments.
    ///
    /// # Panics
    ///
    /// It is very important to make the listener closure not to panic. Otherwise,
    /// the cache will stop calling the listener after a panic. This is an intended
    /// behavior because the cache cannot know whether is is memory safe or not to
    /// call the panicked lister again.
    ///
    /// [removal-cause]: ../notification/enum.RemovalCause.html
    /// [conf]: ../notification/struct.Configuration.html
    pub fn eviction_listener_with_conf(
        self,
        listener: impl Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static,
        conf: notification::Configuration,
    ) -> Self {
        Self {
            eviction_listener: Some(Arc::new(listener)),
            eviction_listener_conf: Some(conf),
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
        let mut builder = self;
        builder.expiration_policy.set_time_to_live(duration);
        builder
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
        let mut builder = self;
        builder.expiration_policy.set_time_to_idle(duration);
        builder
    }

    /// Sets the given `expiry` to the cache.
    ///
    /// See [the example][per-entry-expiration-example] for per-entry expiration
    /// policy in the `Cache` documentation.
    ///
    /// [per-entry-expiration-example]:
    ///     ./struct.Cache.html#per-entry-expiration-policy
    pub fn expire_after(self, expiry: impl Expiry<K, V> + Send + Sync + 'static) -> Self {
        let mut builder = self;
        builder.expiration_policy.set_expiry(Arc::new(expiry));
        builder
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

    /// Specify whether or not to enable the thread pool for housekeeping tasks.
    /// These tasks include removing expired entries and updating the LRU queue and
    /// LFU filter. `true` to enable and `false` to disable. (Default: `true`)
    ///
    /// If disabled, the housekeeping tasks will be executed by a client thread when
    /// necessary.
    ///
    /// NOTE: The default value will be changed to `false` in a future release
    /// (v0.12.0 or v0.13.0).
    pub fn thread_pool_enabled(self, v: bool) -> Self {
        Self {
            thread_pool_enabled: v,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_utils::atomic::AtomicCell;

    use super::CacheBuilder;
    use crate::{
        stats::{
            cache_stats::DetailedCacheStats,
            stats_counter::{DetailedStatsCounter, StatsCounter, StripedStatsCounter},
        },
        sync::{Cache, ConcurrentCacheExt, SegmentedCache},
    };

    use std::time::Duration;

    #[test]
    fn build_cache() {
        // Cache<char, String>
        let cache = CacheBuilder::new(100).build();
        let policy = cache.policy();

        assert_eq!(policy.max_capacity(), Some(100));
        assert_eq!(policy.time_to_live(), None);
        assert_eq!(policy.time_to_idle(), None);
        assert_eq!(policy.num_segments(), 1);

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some("Alice"));

        let cache = CacheBuilder::new(100)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();
        let config = cache.policy();

        assert_eq!(config.max_capacity(), Some(100));
        assert_eq!(config.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(config.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(config.num_segments(), 1);

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some("Alice"));
    }

    #[test]
    fn build_segmented_cache() {
        use crate::notification;

        // SegmentCache<char, String>
        let cache = CacheBuilder::new(100).segments(15).build();
        let policy = cache.policy();

        assert_eq!(policy.max_capacity(), Some(100));
        assert!(policy.time_to_live().is_none());
        assert!(policy.time_to_idle().is_none());
        assert_eq!(policy.num_segments(), 16_usize.next_power_of_two());

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some("Bob"));

        let notification_conf = notification::Configuration::builder()
            .delivery_mode(notification::DeliveryMode::Queued)
            .build();

        let listener = move |_key, _value, _cause| ();

        let builder = CacheBuilder::new(400)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .eviction_listener_with_conf(listener, notification_conf)
            .name("tracked_sessions")
            // Call segments() at the end to check all field values in the current
            // builder struct are copied to the new builder:
            // https://github.com/moka-rs/moka/issues/207
            .segments(24);

        assert!(builder.eviction_listener.is_some());
        assert!(builder.eviction_listener_conf.is_some());

        let cache = builder.build();
        let policy = cache.policy();

        assert_eq!(policy.max_capacity(), Some(400));
        assert_eq!(policy.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(policy.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(policy.num_segments(), 24_usize.next_power_of_two());
        assert_eq!(cache.name(), Some("tracked_sessions"));

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some("Bob"));
    }

    #[test]
    fn build_cache_with_stats_counter() {
        // A cache with stats counter disabled.
        let cache: Cache<i32, ()> = Cache::builder().build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.read_request_count(), 0);
        assert_eq!(stats.miss_count(), 0);
        assert_eq!(stats, Default::default());

        // A cache with stats counter enabled.
        let cache: Cache<i32, ()> = Cache::builder().enable_stats().build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.read_request_count(), 1);
        assert_eq!(stats.miss_count(), 1);

        // A cache with a non-default stats counter.
        let cache: Cache<i32, (), _, DetailedCacheStats> = Cache::builder()
            .stats_counter(StripedStatsCounter::<DetailedStatsCounter>::default())
            .build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.read_request_count(), 1);
        assert_eq!(stats.miss_count(), 1);
        assert_eq!(stats.read_drop_count(), 0);

        // A cache with a custom stats counter.
        let cache: Cache<i32, (), _, MyCacheStats> = Cache::builder()
            .stats_counter(MyStatsCounter::default())
            .build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.request_count, 10);
        assert_eq!(stats.miss_count, 20);
    }

    #[test]
    fn build_segmented_cache_with_stats_counter() {
        // A cache with stats counter disabled.
        let cache: SegmentedCache<i32, ()> = SegmentedCache::builder(4).build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.read_request_count(), 0);
        assert_eq!(stats.miss_count(), 0);
        assert_eq!(stats, Default::default());

        // A cache with stats counter enabled.
        let cache: SegmentedCache<i32, ()> = SegmentedCache::builder(4).enable_stats().build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.read_request_count(), 1);
        assert_eq!(stats.miss_count(), 1);

        // A cache with a non-default stats counter.
        let cache: SegmentedCache<i32, (), _, DetailedCacheStats> = SegmentedCache::builder(4)
            .stats_counter(StripedStatsCounter::<DetailedStatsCounter>::default())
            .build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.read_request_count(), 1);
        assert_eq!(stats.miss_count(), 1);
        assert_eq!(stats.read_drop_count(), 0);

        // A cache with a custom stats counter.
        let cache: SegmentedCache<i32, (), _, MyCacheStats> = SegmentedCache::builder(4)
            .stats_counter(MyStatsCounter::default())
            .build();
        assert!(cache.get(&1).is_none());
        cache.sync();
        let stats = cache.stats();
        assert_eq!(stats.request_count, 10);
        assert_eq!(stats.miss_count, 20);
    }

    #[test]
    #[should_panic(expected = "time_to_live is longer than 1000 years")]
    fn build_cache_too_long_ttl() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<char, String, _, _> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_live(duration + Duration::from_secs(1))
            .build();
    }

    #[test]
    #[should_panic(expected = "time_to_idle is longer than 1000 years")]
    fn build_cache_too_long_tti() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<char, String, _, _> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_idle(duration + Duration::from_secs(1))
            .build();
    }

    /// A custom cache stats.
    struct MyCacheStats {
        request_count: u64,
        miss_count: u64,
    }

    /// A custom stats counter.
    #[derive(Default)]
    struct MyStatsCounter {
        request_count: AtomicCell<u64>,
        miss_count: AtomicCell<u64>,
    }

    impl StatsCounter for MyStatsCounter {
        type Stats = MyCacheStats;

        fn record_misses(&self, _count: u32) {
            self.request_count.fetch_add(1);
            self.miss_count.fetch_add(1);
        }

        fn snapshot(&self) -> Self::Stats {
            MyCacheStats {
                request_count: self.request_count.load() * 10,
                miss_count: self.miss_count.load() * 20,
            }
        }
    }
}
