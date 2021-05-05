use super::Cache;

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    time::Duration,
};

/// Builds a [`Cache`][cache-struct] with various configuration knobs.
///
/// [cache-struct]: ./struct.Cache.html
///
/// # Examples
///
/// ```rust
/// use moka::future::CacheBuilder;
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
    // num_segments: Option<usize>,
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
    /// Construct a new `CacheBuilder` that will be used to build a `Cache` holding
    /// up to `max_capacity` entries.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            max_capacity,
            initial_capacity: None,
            // num_segments: None,
            time_to_live: None,
            time_to_idle: None,
            invalidator_enabled: false,
            cache_type: PhantomData::default(),
        }
    }

    /// Builds a `Cache<K, V>`.
    pub fn build(self) -> Cache<K, V, RandomState> {
        let build_hasher = RandomState::default();
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
    pub fn build_with_hasher<S>(self, hasher: S) -> Cache<K, V, S>
    where
        S: BuildHasher + Clone + Send + Sync + 'static,
    {
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
    pub fn time_to_idle(self, duration: Duration) -> Self {
        Self {
            time_to_idle: Some(duration),
            ..self
        }
    }

    pub fn support_invalidation_closures(self) -> Self {
        Self {
            invalidator_enabled: true,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CacheBuilder;

    use std::time::Duration;

    #[tokio::test]
    async fn build_cache() {
        // Cache<char, String>
        let cache = CacheBuilder::new(100).build();

        assert_eq!(cache.max_capacity(), 100);
        assert_eq!(cache.time_to_live(), None);
        assert_eq!(cache.time_to_idle(), None);
        assert_eq!(cache.num_segments(), 1);

        cache.insert('a', "Alice").await;
        assert_eq!(cache.get(&'a'), Some("Alice"));

        let cache = CacheBuilder::new(100)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();

        assert_eq!(cache.max_capacity(), 100);
        assert_eq!(cache.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(cache.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(cache.num_segments(), 1);

        cache.insert('a', "Alice").await;
        assert_eq!(cache.get(&'a'), Some("Alice"));
    }
}
