use super::{Cache, SegmentedCache};
use crate::{
    common::{builder_utils, concurrent::Weigher},
    notification::{self, EvictionListener, RemovalCause},
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
/// # Example: Eviction Listener
///
/// A `Cache` can be configured with an `eviction_listener`, a closure that is called
/// every time there is a cache eviction. The closure takes the key, value and
/// [`RemovalCause`](../notification/enum.RemovalCause.html) as parameters. It can be
/// used to keep other data structures in sync with the cache.
///
/// The following example demonstrates how to use a cache with an `eviction_listener`
/// and `time_to_live` to manage the lifecycle of temporary files on a filesystem.
/// The cache stores the paths of the files, and when one of them has expired, the
/// eviction lister will be called with the path, so it can remove the file from the
/// filesystem.
///
/// ```rust
/// // Cargo.toml
/// //
/// // [dependencies]
/// // anyhow = "1.0"
/// // uuid = { version = "1.1", features = ["v4"] }
///
/// use moka::{sync::Cache, notification};
///
/// use anyhow::{anyhow, Context};
/// use std::{
///     fs, io,
///     path::{Path, PathBuf},
///     sync::{Arc, RwLock},
///     time::Duration,
/// };
/// use uuid::Uuid;
///
/// /// The DataFileManager writes, reads and removes data files.
/// struct DataFileManager {
///     base_dir: PathBuf,
///     file_count: usize,
/// }
///
/// impl DataFileManager {
///     fn new(base_dir: PathBuf) -> Self {
///         Self {
///             base_dir,
///             file_count: 0,
///         }
///     }
///
///     fn write_data_file(&mut self, contents: String) -> io::Result<PathBuf> {
///         loop {
///             // Generate a unique file path.
///             let mut path = self.base_dir.to_path_buf();
///             path.push(Uuid::new_v4().as_hyphenated().to_string());
///
///             if path.exists() {
///                 continue; // This path is already taken by others. Retry.
///             }
///
///             // We have got a unique file path, so create the file at
///             // the path and write the contents to the file.
///             fs::write(&path, contents)?;
///             self.file_count += 1;
///             println!("Created a data file at {:?} (file count: {})", path, self.file_count);
///
///             // Return the path.
///             return Ok(path);
///         }
///     }
///
///     fn read_data_file(&self, path: impl AsRef<Path>) -> io::Result<String> {
///         // Reads the contents of the file at the path, and return the contents.
///         fs::read_to_string(path)
///     }
///
///     fn remove_data_file(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
///         // Remove the file at the path.
///         fs::remove_file(path.as_ref())?;
///         self.file_count -= 1;
///         println!(
///             "Removed a data file at {:?} (file count: {})",
///             path.as_ref(),
///             self.file_count
///         );
///
///         Ok(())
///     }
/// }
///
/// fn main() -> anyhow::Result<()> {
///     // Create an instance of the DataFileManager and wrap it with
///     // Arc<RwLock<_>> so it can be shared across threads.
///     let file_mgr = DataFileManager::new(std::env::temp_dir());
///     let file_mgr = Arc::new(RwLock::new(file_mgr));
///
///     let file_mgr1 = Arc::clone(&file_mgr);
///
///     // Create an eviction lister closure.
///     let listener = move |k, v: PathBuf, cause| {
///         // Try to remove the data file at the path `v`.
///         println!(
///             "\n== An entry has been evicted. k: {:?}, v: {:?}, cause: {:?}",
///             k, v, cause
///         );
///
///         // Acquire the write lock of the DataFileManager. We must handle
///         // error cases here to prevent the listener from panicking.
///         match file_mgr1.write() {
///             Err(_e) => {
///                 eprintln!("The lock has been poisoned");
///             }
///             Ok(mut mgr) => {
///                 // Remove the data file using the DataFileManager.
///                 if let Err(_e) = mgr.remove_data_file(v.as_path()) {
///                     eprintln!("Failed to remove a data file at {:?}", v);
///                 }
///             }
///         }
///     };
///
///     let listener_conf = notification::Configuration::builder()
///         .delivery_mode(notification::DeliveryMode::Queued)
///         .build();
///
///     // Create the cache. Set time to live for two seconds and set the
///     // eviction listener.
///     let cache = Cache::builder()
///         .max_capacity(100)
///         .time_to_live(Duration::from_secs(2))
///         .eviction_listener_with_conf(listener, listener_conf)
///         .build();
///
///     // Insert an entry to the cache.
///     // This will create and write a data file for the key "user1", store the
///     // path of the file to the cache, and return it.
///     println!("== try_get_with()");
///     let path = cache
///         .try_get_with("user1", || -> anyhow::Result<_> {
///             let mut mgr = file_mgr
///                 .write()
///                 .map_err(|_e| anyhow::anyhow!("The lock has been poisoned"))?;
///             let path = mgr
///                 .write_data_file("user data".into())
///                 .with_context(|| format!("Failed to create a data file"))?;
///             Ok(path)
///         })
///         .map_err(|e| anyhow!("{}", e))?;
///
///     // Read the data file at the path and print the contents.
///     println!("\n== read_data_file()");
///     {
///         let mgr = file_mgr
///             .read()
///             .map_err(|_e| anyhow::anyhow!("The lock has been poisoned"))?;
///         let contents = mgr
///             .read_data_file(path.as_path())
///             .with_context(|| format!("Failed to read data from {:?}", path))?;
///         println!("contents: {}", contents);
///     }
///
///     // Sleep for five seconds. While sleeping, the cache entry for key "user1"
///     // will be expired and evicted, so the eviction lister will be called to
///     // remove the file.
///     std::thread::sleep(Duration::from_secs(5));
///
///     Ok(())
/// }
/// ```
///
#[must_use]
pub struct CacheBuilder<K, V, C> {
    max_capacity: Option<u64>,
    initial_capacity: Option<usize>,
    num_segments: Option<usize>,
    weigher: Option<Weigher<K, V>>,
    eviction_listener: Option<EvictionListener<K, V>>,
    eviction_listener_conf: Option<notification::Configuration>,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    invalidator_enabled: bool,
    cache_type: PhantomData<C>,
}

impl<K, V> Default for CacheBuilder<K, V, Cache<K, V, RandomState>>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self {
            max_capacity: None,
            initial_capacity: None,
            num_segments: None,
            weigher: None,
            eviction_listener: None,
            eviction_listener_conf: None,
            time_to_live: None,
            time_to_idle: None,
            invalidator_enabled: false,
            cache_type: Default::default(),
        }
    }
}

impl<K, V> CacheBuilder<K, V, Cache<K, V, RandomState>>
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

    /// Sets the number of segments of the cache.
    ///
    /// # Panics
    ///
    /// Panics if `num_segments` is zero.
    pub fn segments(
        self,
        num_segments: usize,
    ) -> CacheBuilder<K, V, SegmentedCache<K, V, RandomState>> {
        assert!(num_segments != 0);

        CacheBuilder {
            max_capacity: self.max_capacity,
            initial_capacity: self.initial_capacity,
            num_segments: Some(num_segments),
            weigher: None,
            eviction_listener: None,
            eviction_listener_conf: None,
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
        builder_utils::ensure_expirations_or_panic(self.time_to_live, self.time_to_idle);
        Cache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            build_hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
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
        builder_utils::ensure_expirations_or_panic(self.time_to_live, self.time_to_idle);
        Cache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
            self.time_to_live,
            self.time_to_idle,
            self.invalidator_enabled,
        )
    }
}

impl<K, V> CacheBuilder<K, V, SegmentedCache<K, V, RandomState>>
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
        builder_utils::ensure_expirations_or_panic(self.time_to_live, self.time_to_idle);
        SegmentedCache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            self.num_segments.unwrap(),
            build_hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
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
        builder_utils::ensure_expirations_or_panic(self.time_to_live, self.time_to_idle);
        SegmentedCache::with_everything(
            self.max_capacity,
            self.initial_capacity,
            self.num_segments.unwrap(),
            hasher,
            self.weigher,
            self.eviction_listener,
            self.eviction_listener_conf,
            self.time_to_live,
            self.time_to_idle,
            self.invalidator_enabled,
        )
    }
}

impl<K, V, C> CacheBuilder<K, V, C> {
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

    /// Sets the weigher closure of the cache.
    ///
    /// The closure should take `&K` and `&V` as the arguments and returns a `u32`
    /// representing the relative size of the entry.
    pub fn weigher(self, weigher: impl Fn(&K, &V) -> u32 + Send + Sync + 'static) -> Self {
        Self {
            weigher: Some(Arc::new(weigher)),
            ..self
        }
    }

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
    use super::CacheBuilder;

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
        // SegmentCache<char, String>
        let cache = CacheBuilder::new(100).segments(16).build();
        let policy = cache.policy();

        assert_eq!(policy.max_capacity(), Some(100));
        assert_eq!(policy.time_to_live(), None);
        assert_eq!(policy.time_to_idle(), None);
        assert_eq!(policy.num_segments(), 16_usize.next_power_of_two());

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some("Bob"));

        let cache = CacheBuilder::new(100)
            .segments(16)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();
        let policy = cache.policy();

        assert_eq!(policy.max_capacity(), Some(100));
        assert_eq!(policy.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(policy.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(policy.num_segments(), 16_usize.next_power_of_two());

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some("Bob"));
    }

    #[test]
    #[should_panic(expected = "time_to_live is longer than 1000 years")]
    fn build_cache_too_long_ttl() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<char, String, _> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_live(duration + Duration::from_secs(1))
            .build();
    }

    #[test]
    #[should_panic(expected = "time_to_idle is longer than 1000 years")]
    fn build_cache_too_long_tti() {
        let thousand_years_secs: u64 = 1000 * 365 * 24 * 3600;
        let builder: CacheBuilder<char, String, _> = CacheBuilder::new(100);
        let duration = Duration::from_secs(thousand_years_secs);
        builder
            .time_to_idle(duration + Duration::from_secs(1))
            .build();
    }
}
