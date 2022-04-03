use super::{
    value_initializer::{InitResult, ValueInitializer},
    CacheBuilder, ConcurrentCacheExt,
};
use crate::{
    sync::{
        base_cache::{BaseCache, HouseKeeperArc, MAX_SYNC_REPEATS, WRITE_RETRY_INTERVAL_MICROS},
        housekeeper::InnerSync,
        PredicateId, Weigher, WriteOp,
    },
    Policy, PredicateError,
};

#[cfg(feature = "unstable-debug-counters")]
use crate::sync::debug_counters::CacheDebugStats;

use crossbeam_channel::{Sender, TrySendError};
use std::{
    any::TypeId,
    borrow::Borrow,
    collections::hash_map::RandomState,
    future::Future,
    hash::{BuildHasher, Hash},
    sync::Arc,
    time::Duration,
};

/// A thread-safe, futures-aware concurrent in-memory cache.
///
/// `Cache` supports full concurrency of retrievals and a high expected concurrency
/// for updates. It can be accessed inside and outside of asynchronous contexts.
///
/// `Cache` utilizes a lock-free concurrent hash table as the central key-value
/// storage. `Cache` performs a best-effort bounding of the map using an entry
/// replacement algorithm to determine which entries to evict when the capacity is
/// exceeded.
///
/// To use this cache, enable a crate feature called "future".
///
/// # Examples
///
/// Cache entries are manually added using an insert method, and are stored in the
/// cache until either evicted or manually invalidated:
///
/// - Inside an async context (`async fn` or `async` block), use
///   [`insert`](#method.insert), [`get_with`](#method.get_with)
///   or [`invalidate`](#method.invalidate) methods for updating the cache and `await`
///   them.
/// - Outside any async context, use [`blocking_insert`](#method.blocking_insert) or
///   [`blocking_invalidate`](#method.blocking_invalidate) methods. They will block
///   for a short time under heavy updates.
///
/// Here's an example of reading and updating a cache by using multiple asynchronous
/// tasks with [Tokio][tokio-crate] runtime:
///
/// [tokio-crate]: https://crates.io/crates/tokio
///
///```rust
/// // Cargo.toml
/// //
/// // [dependencies]
/// // moka = { version = "0.8", features = ["future"] }
/// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
/// // futures-util = "0.3"
///
/// use moka::future::Cache;
///
/// #[tokio::main]
/// async fn main() {
///     const NUM_TASKS: usize = 16;
///     const NUM_KEYS_PER_TASK: usize = 64;
///
///     fn value(n: usize) -> String {
///         format!("value {}", n)
///     }
///
///     // Create a cache that can store up to 10,000 entries.
///     let cache = Cache::new(10_000);
///
///     // Spawn async tasks and write to and read from the cache.
///     let tasks: Vec<_> = (0..NUM_TASKS)
///         .map(|i| {
///             // To share the same cache across the async tasks, clone it.
///             // This is a cheap operation.
///             let my_cache = cache.clone();
///             let start = i * NUM_KEYS_PER_TASK;
///             let end = (i + 1) * NUM_KEYS_PER_TASK;
///
///             tokio::spawn(async move {
///                 // Insert 64 entries. (NUM_KEYS_PER_TASK = 64)
///                 for key in start..end {
///                     // insert() is an async method, so await it.
///                     my_cache.insert(key, value(key)).await;
///                     // get() returns Option<String>, a clone of the stored value.
///                     assert_eq!(my_cache.get(&key), Some(value(key)));
///                 }
///
///                 // Invalidate every 4 element of the inserted entries.
///                 for key in (start..end).step_by(4) {
///                     // invalidate() is an async method, so await it.
///                     my_cache.invalidate(&key).await;
///                 }
///             })
///         })
///         .collect();
///
///     // Wait for all tasks to complete.
///     futures_util::future::join_all(tasks).await;
///
///     // Verify the result.
///     for key in 0..(NUM_TASKS * NUM_KEYS_PER_TASK) {
///         if key % 4 == 0 {
///             assert_eq!(cache.get(&key), None);
///         } else {
///             assert_eq!(cache.get(&key), Some(value(key)));
///         }
///     }
/// }
/// ```
///
/// If you want to atomically initialize and insert a value when the key is not
/// present, you might want to check other insertion methods
/// [`get_with`](#method.get_with) and
/// [`try_get_with`](#method.try_get_with).
///
/// # Avoiding to clone the value at `get`
///
/// The return type of `get` method is `Option<V>` instead of `Option<&V>`. Every
/// time `get` is called for an existing key, it creates a clone of the stored value
/// `V` and returns it. This is because the `Cache` allows concurrent updates from
/// threads so a value stored in the cache can be dropped or replaced at any time by
/// any other thread. `get` cannot return a reference `&V` as it is impossible to
/// guarantee the value outlives the reference.
///
/// If you want to store values that will be expensive to clone, wrap them by
/// `std::sync::Arc` before storing in a cache. [`Arc`][rustdoc-std-arc] is a
/// thread-safe reference-counted pointer and its `clone()` method is cheap.
///
/// [rustdoc-std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
///
/// # Size-based Eviction
///
/// ```rust
/// // Cargo.toml
/// //
/// // [dependencies]
/// // moka = { version = "0.8", features = ["future"] }
/// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
/// // futures-util = "0.3"
///
/// use std::convert::TryInto;
/// use moka::future::Cache;
///
/// #[tokio::main]
/// async fn main() {
///     // Evict based on the number of entries in the cache.
///     let cache = Cache::builder()
///         // Up to 10,000 entries.
///         .max_capacity(10_000)
///         // Create the cache.
///         .build();
///     cache.insert(1, "one".to_string()).await;
///
///     // Evict based on the byte length of strings in the cache.
///     let cache = Cache::builder()
///         // A weigher closure takes &K and &V and returns a u32
///         // representing the relative size of the entry.
///         .weigher(|_key, value: &String| -> u32 {
///             value.len().try_into().unwrap_or(u32::MAX)
///         })
///         // This cache will hold up to 32MiB of values.
///         .max_capacity(32 * 1024 * 1024)
///         .build();
///     cache.insert(2, "two".to_string()).await;
/// }
/// ```
///
/// If your cache should not grow beyond a certain size, use the `max_capacity`
/// method of the [`CacheBuilder`][builder-struct] to set the upper bound. The cache
/// will try to evict entries that have not been used recently or very often.
///
/// At the cache creation time, a weigher closure can be set by the `weigher` method
/// of the `CacheBuilder`. A weigher closure takes `&K` and `&V` as the arguments and
/// returns a `u32` representing the relative size of the entry:
///
/// - If the `weigher` is _not_ set, the cache will treat each entry has the same
///   size of `1`. This means the cache will be bounded by the number of entries.
/// - If the `weigher` is set, the cache will call the weigher to calculate the
///   weighted size (relative size) on an entry. This means the cache will be bounded
///   by the total weighted size of entries.
///
/// Note that weighted sizes are not used when making eviction selections.
///
/// [builder-struct]: ./struct.CacheBuilder.html
///
/// # Time-based Expirations
///
/// `Cache` supports the following expiration policies:
///
/// - **Time to live**: A cached entry will be expired after the specified duration
///   past from `insert`.
/// - **Time to idle**: A cached entry will be expired after the specified duration
///   past from `get` or `insert`.
///
/// ```rust
/// // Cargo.toml
/// //
/// // [dependencies]
/// // moka = { version = "0.8", features = ["future"] }
/// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
/// // futures-util = "0.3"
///
/// use moka::future::Cache;
/// use std::time::Duration;
///
/// #[tokio::main]
/// async fn main() {
///     let cache = Cache::builder()
///         // Time to live (TTL): 30 minutes
///         .time_to_live(Duration::from_secs(30 * 60))
///         // Time to idle (TTI):  5 minutes
///         .time_to_idle(Duration::from_secs( 5 * 60))
///         // Create the cache.
///         .build();
///
///     // This entry will expire after 5 minutes (TTI) if there is no get().
///     cache.insert(0, "zero").await;
///
///     // This get() will extend the entry life for another 5 minutes.
///     cache.get(&0);
///
///     // Even though we keep calling get(), the entry will expire
///     // after 30 minutes (TTL) from the insert().
/// }
/// ```
///
/// # Thread Safety
///
/// All methods provided by the `Cache` are considered thread-safe, and can be safely
/// accessed by multiple concurrent threads.
///
/// - `Cache<K, V, S>` requires trait bounds `Send`, `Sync` and `'static` for `K`
///   (key), `V` (value) and `S` (hasher state).
/// - `Cache<K, V, S>` will implement `Send` and `Sync`.
///
/// # Sharing a cache across asynchronous tasks
///
/// To share a cache across async tasks (or OS threads), do one of the followings:
///
/// - Create a clone of the cache by calling its `clone` method and pass it to other
///   task.
/// - Wrap the cache by a `sync::OnceCell` or `sync::Lazy` from
///   [once_cell][once-cell-crate] create, and set it to a `static` variable.
///
/// Cloning is a cheap operation for `Cache` as it only creates thread-safe
/// reference-counted pointers to the internal data structures.
///
/// [once-cell-crate]: https://crates.io/crates/once_cell
///
/// # Hashing Algorithm
///
/// By default, `Cache` uses a hashing algorithm selected to provide resistance
/// against HashDoS attacks. It will be the same one used by
/// `std::collections::HashMap`, which is currently SipHash 1-3.
///
/// While SipHash's performance is very competitive for medium sized keys, other
/// hashing algorithms will outperform it for small keys such as integers as well as
/// large keys such as long strings. However those algorithms will typically not
/// protect against attacks such as HashDoS.
///
/// The hashing algorithm can be replaced on a per-`Cache` basis using the
/// [`build_with_hasher`][build-with-hasher-method] method of the
/// `CacheBuilder`. Many alternative algorithms are available on crates.io, such
/// as the [aHash][ahash-crate] crate.
///
/// [build-with-hasher-method]: ./struct.CacheBuilder.html#method.build_with_hasher
/// [ahash-crate]: https://crates.io/crates/ahash
///
#[derive(Clone)]
pub struct Cache<K, V, S = RandomState> {
    base: BaseCache<K, V, S>,
    value_initializer: Arc<ValueInitializer<K, V, S>>,
}

// TODO: https://github.com/moka-rs/moka/issues/54
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<K, V, S> Send for Cache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Send,
{
}

unsafe impl<K, V, S> Sync for Cache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Sync,
{
}

impl<K, V> Cache<K, V, RandomState>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Constructs a new `Cache<K, V>` that will store up to the `max_capacity`.
    ///
    /// To adjust various configuration knobs such as `initial_capacity` or
    /// `time_to_live`, use the [`CacheBuilder`][builder-struct].
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    pub fn new(max_capacity: u64) -> Self {
        let build_hasher = RandomState::default();
        Self::with_everything(
            Some(max_capacity),
            None,
            build_hasher,
            None,
            None,
            None,
            false,
        )
    }

    /// Returns a [`CacheBuilder`][builder-struct], which can builds a `Cache` with
    /// various configuration knobs.
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    pub fn builder() -> CacheBuilder<K, V, Cache<K, V, RandomState>> {
        CacheBuilder::default()
    }
}

impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn with_everything(
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
        invalidator_enabled: bool,
    ) -> Self {
        Self {
            base: BaseCache::new(
                max_capacity,
                initial_capacity,
                build_hasher.clone(),
                weigher,
                time_to_live,
                time_to_idle,
                invalidator_enabled,
            ),
            value_initializer: Arc::new(ValueInitializer::with_hasher(build_hasher)),
        }
    }

    /// Returns `true` if the cache contains a value for the key.
    ///
    /// Unlike the `get` method, this method is not considered a cache read operation,
    /// so it does not update the historic popularity estimator or reset the idle
    /// timer for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.contains_key_with_hash(key, self.base.hash(key))
    }

    /// Returns a _clone_ of the value corresponding to the key.
    ///
    /// If you want to store values that will be expensive to clone, wrap them by
    /// `std::sync::Arc` before storing in a cache. [`Arc`][rustdoc-std-arc] is a
    /// thread-safe reference-counted pointer and its `clone()` method is cheap.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    ///
    /// [rustdoc-std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.get_with_hash(key, self.base.hash(key))
    }

    /// Deprecated, replaced with [`get_with`](#method.get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `get_with`")]
    pub async fn get_or_insert_with(&self, key: K, init: impl Future<Output = V>) -> V {
        self.get_with(key, init).await
    }

    /// Deprecated, replaced with [`try_get_with`](#method.try_get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `try_get_with`")]
    pub async fn get_or_try_insert_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        self.try_get_with(key, init).await
    }

    /// Ensures the value of the key exists by inserting the output of the init
    /// future if not exist, and returns a _clone_ of the value.
    ///
    /// This method prevents to resolve the init future multiple times on the same
    /// key even if the method is concurrently called by many async tasks; only one
    /// of the calls resolves its future, and other calls wait for that future to
    /// complete.
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.8", features = ["future"] }
    /// // futures-util = "0.3"
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    /// use moka::future::Cache;
    /// use std::sync::Arc;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     const TEN_MIB: usize = 10 * 1024 * 1024; // 10MiB
    ///     let cache = Cache::new(100);
    ///
    ///     // Spawn four async tasks.
    ///     let tasks: Vec<_> = (0..4_u8)
    ///         .map(|task_id| {
    ///             let my_cache = cache.clone();
    ///             tokio::spawn(async move {
    ///                 println!("Task {} started.", task_id);
    ///
    ///                 // Insert and get the value for key1. Although all four async tasks
    ///                 // will call `get_with` at the same time, the `init` async
    ///                 // block must be resolved only once.
    ///                 let value = my_cache
    ///                     .get_with("key1", async move {
    ///                         println!("Task {} inserting a value.", task_id);
    ///                         Arc::new(vec![0u8; TEN_MIB])
    ///                     })
    ///                     .await;
    ///
    ///                 // Ensure the value exists now.
    ///                 assert_eq!(value.len(), TEN_MIB);
    ///                 assert!(my_cache.get(&"key1").is_some());
    ///
    ///                 println!("Task {} got the value. (len: {})", task_id, value.len());
    ///             })
    ///         })
    ///         .collect();
    ///
    ///     // Run all tasks concurrently and wait for them to complete.
    ///     futures_util::future::join_all(tasks).await;
    /// }
    /// ```
    ///
    /// **A Sample Result**
    ///
    /// - The `init` future (async black) was resolved exactly once by task 3.
    /// - Other tasks were blocked until task 3 inserted the value.
    ///
    /// ```console
    /// Task 0 started.
    /// Task 3 started.
    /// Task 1 started.
    /// Task 2 started.
    /// Task 3 inserting a value.
    /// Task 3 got the value. (len: 10485760)
    /// Task 0 got the value. (len: 10485760)
    /// Task 1 got the value. (len: 10485760)
    /// Task 2 got the value. (len: 10485760)
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics when the `init` future has been panicked. When it happens,
    /// only the caller whose `init` future panicked will get the panic (e.g. only
    /// task 3 in the above sample). If there are other calls in progress (e.g. task
    /// 0, 1 and 2 above), this method will restart and resolve one of the remaining
    /// `init` futures.
    ///
    pub async fn get_with(&self, key: K, init: impl Future<Output = V>) -> V {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.get_or_insert_with_hash_and_fun(key, hash, init).await
    }

    /// Try to ensure the value of the key exists by inserting an `Ok` output of the
    /// init future if not exist, and returns a _clone_ of the value or the `Err`
    /// produced by the future.
    ///
    /// This method prevents to resolve the init future multiple times on the same
    /// key even if the method is concurrently called by many async tasks; only one
    /// of the calls resolves its future (as long as these futures return the same
    /// error type), and other calls wait for that future to complete.
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.8", features = ["future"] }
    /// // futures-util = "0.3"
    /// // reqwest = "0.11"
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    /// use moka::future::Cache;
    ///
    /// // This async function tries to get HTML from the given URI.
    /// async fn get_html(task_id: u8, uri: &str) -> Result<String, reqwest::Error> {
    ///     println!("get_html() called by task {}.", task_id);
    ///     Ok(reqwest::get(uri).await?.text().await?)
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache = Cache::new(100);
    ///
    ///     // Spawn four async tasks.
    ///     let tasks: Vec<_> = (0..4_u8)
    ///         .map(|task_id| {
    ///             let my_cache = cache.clone();
    ///             tokio::spawn(async move {
    ///                 println!("Task {} started.", task_id);
    ///
    ///                 // Try to insert and get the value for key1. Although
    ///                 // all four async tasks will call `try_get_with`
    ///                 // at the same time, get_html() must be called only once.
    ///                 let value = my_cache
    ///                     .try_get_with(
    ///                         "key1",
    ///                         get_html(task_id, "https://www.rust-lang.org"),
    ///                     ).await;
    ///
    ///                 // Ensure the value exists now.
    ///                 assert!(value.is_ok());
    ///                 assert!(my_cache.get(&"key1").is_some());
    ///
    ///                 println!(
    ///                     "Task {} got the value. (len: {})",
    ///                     task_id,
    ///                     value.unwrap().len()
    ///                 );
    ///             })
    ///         })
    ///         .collect();
    ///
    ///     // Run all tasks concurrently and wait for them to complete.
    ///     futures_util::future::join_all(tasks).await;
    /// }
    /// ```
    ///
    /// **A Sample Result**
    ///
    /// - `get_html()` was called exactly once by task 2.
    /// - Other tasks were blocked until task 2 inserted the value.
    ///
    /// ```console
    /// Task 1 started.
    /// Task 0 started.
    /// Task 2 started.
    /// Task 3 started.
    /// get_html() called by task 2.
    /// Task 2 got the value. (len: 19419)
    /// Task 1 got the value. (len: 19419)
    /// Task 0 got the value. (len: 19419)
    /// Task 3 got the value. (len: 19419)
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics when the `init` future has been panicked. When it happens,
    /// only the caller whose `init` future panicked will get the panic (e.g. only
    /// task 2 in the above sample). If there are other calls in progress (e.g. task
    /// 0, 1 and 3 above), this method will restart and resolve one of the remaining
    /// `init` futures.
    ///
    pub async fn try_get_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.get_or_try_insert_with_hash_and_fun(key, hash, init)
            .await
    }

    /// Inserts a key-value pair into the cache.
    ///
    /// If the cache has this key present, the value is updated.
    pub async fn insert(&self, key: K, value: V) {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.insert_with_hash(key, hash, value).await
    }

    fn do_blocking_insert(&self, key: K, value: V) {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        let op = self.base.do_insert_with_hash(key, hash, value);
        let hk = self.base.housekeeper.as_ref();
        Self::blocking_schedule_write_op(&self.base.write_op_ch, op, hk).expect("Failed to insert");
    }

    /// Discards any cached value for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub async fn invalidate<Q>(&self, key: &Q)
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.base.hash(key);
        if let Some(kv) = self.base.remove_entry(key, hash) {
            let op = WriteOp::Remove(kv);
            let hk = self.base.housekeeper.as_ref();
            Self::schedule_write_op(&self.base.write_op_ch, op, hk)
                .await
                .expect("Failed to remove");
        }
    }

    fn do_blocking_invalidate<Q>(&self, key: &Q)
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.base.hash(key);
        if let Some(kv) = self.base.remove_entry(key, hash) {
            let op = WriteOp::Remove(kv);
            let hk = self.base.housekeeper.as_ref();
            Self::blocking_schedule_write_op(&self.base.write_op_ch, op, hk)
                .expect("Failed to remove");
        }
    }

    /// Discards all cached values.
    ///
    /// This method returns immediately and a background thread will evict all the
    /// cached values inserted before the time when this method was called. It is
    /// guaranteed that the `get` method must not return these invalidated values
    /// even if they have not been evicted.
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    pub fn invalidate_all(&self) {
        self.base.invalidate_all();
    }

    /// Discards cached values that satisfy a predicate.
    ///
    /// `invalidate_entries_if` takes a closure that returns `true` or `false`. This
    /// method returns immediately and a background thread will apply the closure to
    /// each cached value inserted before the time when `invalidate_entries_if` was
    /// called. If the closure returns `true` on a value, that value will be evicted
    /// from the cache.
    ///
    /// Also the `get` method will apply the closure to a value to determine if it
    /// should have been invalidated. Therefore, it is guaranteed that the `get`
    /// method must not return invalidated values.
    ///
    /// Note that you must call
    /// [`CacheBuilder::support_invalidation_closures`][support-invalidation-closures]
    /// at the cache creation time as the cache needs to maintain additional internal
    /// data structures to support this method. Otherwise, calling this method will
    /// fail with a
    /// [`PredicateError::InvalidationClosuresDisabled`][invalidation-disabled-error].
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    ///
    /// [support-invalidation-closures]: ./struct.CacheBuilder.html#method.support_invalidation_closures
    /// [invalidation-disabled-error]: ../enum.PredicateError.html#variant.InvalidationClosuresDisabled
    pub fn invalidate_entries_if<F>(&self, predicate: F) -> Result<PredicateId, PredicateError>
    where
        F: Fn(&K, &V) -> bool + Send + Sync + 'static,
    {
        self.base.invalidate_entries_if(Arc::new(predicate))
    }

    /// Returns a `BlockingOp` for this cache. It provides blocking
    /// [insert](#method.insert) and [invalidate](#method.invalidate) methods, which
    /// can be called outside of asynchronous contexts.
    pub fn blocking(&self) -> BlockingOp<'_, K, V, S> {
        BlockingOp(self)
    }

    /// Returns a read-only cache policy of this cache.
    ///
    /// At this time, cache policy cannot be modified after cache creation.
    /// A future version may support to modify it.
    pub fn policy(&self) -> Policy {
        self.base.policy()
    }

    #[cfg(feature = "unstable-debug-counters")]
    pub fn debug_stats(&self) -> CacheDebugStats {
        self.base.debug_stats()
    }

    #[cfg(test)]
    fn estimated_entry_count(&self) -> u64 {
        self.base.estimated_entry_count()
    }

    #[cfg(test)]
    fn weighted_size(&self) -> u64 {
        self.base.weighted_size()
    }
}

impl<K, V, S> ConcurrentCacheExt<K, V> for Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn sync(&self) {
        self.base.inner.sync(MAX_SYNC_REPEATS);
    }
}

// private methods
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    async fn get_or_insert_with_hash_and_fun(
        &self,
        key: Arc<K>,
        hash: u64,
        init: impl Future<Output = V>,
    ) -> V {
        if let Some(v) = self.base.get_with_hash(&key, hash) {
            return v;
        }

        match self
            .value_initializer
            .init_or_read(Arc::clone(&key), init)
            .await
        {
            InitResult::Initialized(v) => {
                self.insert_with_hash(Arc::clone(&key), hash, v.clone())
                    .await;
                self.value_initializer
                    .remove_waiter(&key, TypeId::of::<()>());
                v
            }
            InitResult::ReadExisting(v) => v,
            InitResult::InitErr(_) => unreachable!(),
        }
    }

    async fn get_or_try_insert_with_hash_and_fun<F, E>(
        &self,
        key: Arc<K>,
        hash: u64,
        init: F,
    ) -> Result<V, Arc<E>>
    where
        F: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        if let Some(v) = self.base.get_with_hash(&key, hash) {
            return Ok(v);
        }

        match self
            .value_initializer
            .try_init_or_read(Arc::clone(&key), init)
            .await
        {
            InitResult::Initialized(v) => {
                let hash = self.base.hash(&key);
                self.insert_with_hash(Arc::clone(&key), hash, v.clone())
                    .await;
                self.value_initializer
                    .remove_waiter(&key, TypeId::of::<E>());
                Ok(v)
            }
            InitResult::ReadExisting(v) => Ok(v),
            InitResult::InitErr(e) => Err(e),
        }
    }

    async fn insert_with_hash(&self, key: Arc<K>, hash: u64, value: V) {
        let op = self.base.do_insert_with_hash(key, hash, value);
        let hk = self.base.housekeeper.as_ref();
        Self::schedule_write_op(&self.base.write_op_ch, op, hk)
            .await
            .expect("Failed to insert");
    }

    #[inline]
    async fn schedule_write_op(
        ch: &Sender<WriteOp<K, V>>,
        op: WriteOp<K, V>,
        housekeeper: Option<&HouseKeeperArc<K, V, S>>,
    ) -> Result<(), TrySendError<WriteOp<K, V>>> {
        let mut op = op;

        // TODO: Try to replace the timer with an async event listener to see if it
        // can provide better performance.
        loop {
            BaseCache::apply_reads_writes_if_needed(ch, housekeeper);
            match ch.try_send(op) {
                Ok(()) => break,
                Err(TrySendError::Full(op1)) => {
                    op = op1;
                    async_io::Timer::after(Duration::from_micros(WRITE_RETRY_INTERVAL_MICROS))
                        .await;
                }
                Err(e @ TrySendError::Disconnected(_)) => return Err(e),
            }
        }
        Ok(())
    }

    #[inline]
    fn blocking_schedule_write_op(
        ch: &Sender<WriteOp<K, V>>,
        op: WriteOp<K, V>,
        housekeeper: Option<&HouseKeeperArc<K, V, S>>,
    ) -> Result<(), TrySendError<WriteOp<K, V>>> {
        let mut op = op;

        loop {
            BaseCache::apply_reads_writes_if_needed(ch, housekeeper);
            match ch.try_send(op) {
                Ok(()) => break,
                Err(TrySendError::Full(op1)) => {
                    op = op1;
                    std::thread::sleep(Duration::from_micros(WRITE_RETRY_INTERVAL_MICROS));
                }
                Err(e @ TrySendError::Disconnected(_)) => return Err(e),
            }
        }
        Ok(())
    }
}

// For unit tests.
#[cfg(test)]
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn is_table_empty(&self) -> bool {
        self.estimated_entry_count() == 0
    }

    fn invalidation_predicate_count(&self) -> usize {
        self.base.invalidation_predicate_count()
    }

    fn reconfigure_for_testing(&mut self) {
        self.base.reconfigure_for_testing();
    }

    fn set_expiration_clock(&self, clock: Option<crate::common::time::Clock>) {
        self.base.set_expiration_clock(clock);
    }
}

pub struct BlockingOp<'a, K, V, S>(&'a Cache<K, V, S>);

impl<'a, K, V, S> BlockingOp<'a, K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    /// Blocking [insert](./struct.Cache.html#method.insert) to call outside of
    /// asynchronous contexts.
    ///
    /// This method is intended for use cases where you are inserting from
    /// synchronous code.
    pub fn insert(&self, key: K, value: V) {
        self.0.do_blocking_insert(key, value)
    }

    /// Blocking [invalidate](./struct.Cache.html#method.invalidate) to call outside
    /// of asynchronous contexts.
    ///
    /// This method is intended for use cases where you are invalidating from
    /// synchronous code.
    pub fn invalidate<Q>(&self, key: &Q)
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.do_blocking_invalidate(key)
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{Cache, ConcurrentCacheExt};
    use crate::common::time::Clock;

    use async_io::Timer;
    use std::{convert::Infallible, sync::Arc, time::Duration};

    #[tokio::test]
    async fn basic_single_async_task() {
        let mut cache = Cache::new(3);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice").await;
        cache.insert("b", "bob").await;
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy").await;
        assert_eq!(cache.get(&"c"), Some("cindy"));
        assert!(cache.contains_key(&"c"));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert!(cache.contains_key(&"a"));
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(cache.contains_key(&"b"));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david").await; //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", "david").await;
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher than c's.
        cache.insert("d", "dennis").await;
        cache.sync();
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some("dennis"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"c"));
        assert!(cache.contains_key(&"d"));

        cache.invalidate(&"b").await;
        assert_eq!(cache.get(&"b"), None);
        assert!(!cache.contains_key(&"b"));
    }

    #[test]
    fn basic_single_blocking_api() {
        let mut cache = Cache::new(3);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.blocking().insert("a", "alice");
        cache.blocking().insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.blocking().insert("c", "cindy");
        assert_eq!(cache.get(&"c"), Some("cindy"));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.blocking().insert("d", "david"); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1

        cache.blocking().insert("d", "david");
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher than c's.
        cache.blocking().insert("d", "dennis");
        cache.sync();
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some("dennis"));

        cache.blocking().invalidate(&"b");
        assert_eq!(cache.get(&"b"), None);
    }

    #[tokio::test]
    async fn size_aware_eviction() {
        let weigher = |_k: &&str, v: &(&str, u32)| v.1;

        let alice = ("alice", 10);
        let bob = ("bob", 15);
        let bill = ("bill", 20);
        let cindy = ("cindy", 5);
        let david = ("david", 15);
        let dennis = ("dennis", 15);

        let mut cache = Cache::builder().max_capacity(31).weigher(weigher).build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", alice).await;
        cache.insert("b", bob).await;
        assert_eq!(cache.get(&"a"), Some(alice));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.get(&"b"), Some(bob));
        cache.sync();
        // order (LRU -> MRU) and counts: a -> 1, b -> 1

        cache.insert("c", cindy).await;
        assert_eq!(cache.get(&"c"), Some(cindy));
        assert!(cache.contains_key(&"c"));
        // order and counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert!(cache.contains_key(&"a"));
        assert_eq!(cache.get(&"a"), Some(alice));
        assert_eq!(cache.get(&"b"), Some(bob));
        assert!(cache.contains_key(&"b"));
        cache.sync();
        // order and counts: c -> 1, a -> 2, b -> 2

        // To enter "d" (weight: 15), it needs to evict "c" (w: 5) and "a" (w: 10).
        // "d" must have higher count than 3, which is the aggregated count
        // of "a" and "c".
        cache.insert("d", david).await; //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", david).await;
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        cache.insert("d", david).await;
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 3
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", david).await;
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 4

        // Finally "d" should be admitted by evicting "c" and "a".
        cache.insert("d", dennis).await;
        cache.sync();
        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some(bob));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(dennis));
        assert!(!cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"c"));
        assert!(cache.contains_key(&"d"));

        // Update "b" with "bill" (w: 15 -> 20). This should evict "d" (w: 15).
        cache.insert("b", bill).await;
        cache.sync();
        assert_eq!(cache.get(&"b"), Some(bill));
        assert_eq!(cache.get(&"d"), None);
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"d"));

        // Re-add "a" (w: 10) and update "b" with "bob" (w: 20 -> 15).
        cache.insert("a", alice).await;
        cache.insert("b", bob).await;
        cache.sync();
        assert_eq!(cache.get(&"a"), Some(alice));
        assert_eq!(cache.get(&"b"), Some(bob));
        assert_eq!(cache.get(&"d"), None);
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"d"));

        // Verify the sizes.
        assert_eq!(cache.estimated_entry_count(), 2);
        assert_eq!(cache.weighted_size(), 25);
    }

    #[tokio::test]
    async fn basic_multi_async_tasks() {
        let num_threads = 4;
        let cache = Cache::new(100);

        let tasks = (0..num_threads)
            .map(|id| {
                let cache = cache.clone();
                if id == 0 {
                    tokio::spawn(async move {
                        cache.blocking().insert(10, format!("{}-100", id));
                        cache.get(&10);
                        cache.blocking().insert(20, format!("{}-200", id));
                        cache.blocking().invalidate(&10);
                    })
                } else {
                    tokio::spawn(async move {
                        cache.insert(10, format!("{}-100", id)).await;
                        cache.get(&10);
                        cache.insert(20, format!("{}-200", id)).await;
                        cache.invalidate(&10).await;
                    })
                }
            })
            .collect::<Vec<_>>();

        let _ = futures_util::future::join_all(tasks).await;

        assert!(cache.get(&10).is_none());
        assert!(cache.get(&20).is_some());
        assert!(!cache.contains_key(&10));
        assert!(cache.contains_key(&20));
    }

    #[tokio::test]
    async fn invalidate_all() {
        let mut cache = Cache::new(100);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice").await;
        cache.insert("b", "bob").await;
        cache.insert("c", "cindy").await;
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), Some("cindy"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(cache.contains_key(&"c"));
        cache.sync();

        cache.invalidate_all();
        cache.sync();

        cache.insert("d", "david").await;
        cache.sync();

        assert!(cache.get(&"a").is_none());
        assert!(cache.get(&"b").is_none());
        assert!(cache.get(&"c").is_none());
        assert_eq!(cache.get(&"d"), Some("david"));
        assert!(!cache.contains_key(&"a"));
        assert!(!cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"c"));
        assert!(cache.contains_key(&"d"));
    }

    #[tokio::test]
    async fn invalidate_entries_if() -> Result<(), Box<dyn std::error::Error>> {
        use std::collections::HashSet;

        let mut cache = Cache::builder()
            .max_capacity(100)
            .support_invalidation_closures()
            .build();
        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert(0, "alice").await;
        cache.insert(1, "bob").await;
        cache.insert(2, "alex").await;
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.
        cache.sync();

        assert_eq!(cache.get(&0), Some("alice"));
        assert_eq!(cache.get(&1), Some("bob"));
        assert_eq!(cache.get(&2), Some("alex"));
        assert!(cache.contains_key(&0));
        assert!(cache.contains_key(&1));
        assert!(cache.contains_key(&2));

        let names = ["alice", "alex"].iter().cloned().collect::<HashSet<_>>();
        cache.invalidate_entries_if(move |_k, &v| names.contains(v))?;
        assert_eq!(cache.invalidation_predicate_count(), 1);

        mock.increment(Duration::from_secs(5)); // 10 secs from the start.

        cache.insert(3, "alice").await;

        // Run the invalidation task and wait for it to finish. (TODO: Need a better way than sleeping)
        cache.sync(); // To submit the invalidation task.
        std::thread::sleep(Duration::from_millis(200));
        cache.sync(); // To process the task result.
        std::thread::sleep(Duration::from_millis(200));

        assert!(cache.get(&0).is_none());
        assert!(cache.get(&2).is_none());
        assert_eq!(cache.get(&1), Some("bob"));
        // This should survive as it was inserted after calling invalidate_entries_if.
        assert_eq!(cache.get(&3), Some("alice"));

        assert!(!cache.contains_key(&0));
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));

        assert_eq!(cache.estimated_entry_count(), 2);
        assert_eq!(cache.invalidation_predicate_count(), 0);

        mock.increment(Duration::from_secs(5)); // 15 secs from the start.

        cache.invalidate_entries_if(|_k, &v| v == "alice")?;
        cache.invalidate_entries_if(|_k, &v| v == "bob")?;
        assert_eq!(cache.invalidation_predicate_count(), 2);

        // Run the invalidation task and wait for it to finish. (TODO: Need a better way than sleeping)
        cache.sync(); // To submit the invalidation task.
        std::thread::sleep(Duration::from_millis(200));
        cache.sync(); // To process the task result.
        std::thread::sleep(Duration::from_millis(200));

        assert!(cache.get(&1).is_none());
        assert!(cache.get(&3).is_none());

        assert!(!cache.contains_key(&1));
        assert!(!cache.contains_key(&3));

        assert_eq!(cache.estimated_entry_count(), 0);
        assert_eq!(cache.invalidation_predicate_count(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn time_to_live() {
        let mut cache = Cache::builder()
            .max_capacity(100)
            .time_to_live(Duration::from_secs(10))
            .build();

        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice").await;
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.
        cache.sync();

        assert_eq!(cache.get(&"a"), Some("alice"));
        assert!(cache.contains_key(&"a"));

        mock.increment(Duration::from_secs(5)); // 10 secs.
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert!(!cache.contains_key(&"a"));
        assert!(cache.is_table_empty());

        cache.insert("b", "bob").await;
        cache.sync();

        assert_eq!(cache.estimated_entry_count(), 1);

        mock.increment(Duration::from_secs(5)); // 15 secs.
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.estimated_entry_count(), 1);

        cache.insert("b", "bill").await;
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 20 secs
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bill"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.estimated_entry_count(), 1);

        mock.increment(Duration::from_secs(5)); // 25 secs
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(!cache.contains_key(&"a"));
        assert!(!cache.contains_key(&"b"));
        assert!(cache.is_table_empty());
    }

    #[tokio::test]
    async fn time_to_idle() {
        let mut cache = Cache::builder()
            .max_capacity(100)
            .time_to_idle(Duration::from_secs(10))
            .build();

        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice").await;
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.
        cache.sync();

        assert_eq!(cache.get(&"a"), Some("alice"));

        mock.increment(Duration::from_secs(5)); // 10 secs.
        cache.sync();

        cache.insert("b", "bob").await;
        cache.sync();

        assert_eq!(cache.estimated_entry_count(), 2);

        mock.increment(Duration::from_secs(2)); // 12 secs.
        cache.sync();

        // contains_key does not reset the idle timer for the key.
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        cache.sync();

        assert_eq!(cache.estimated_entry_count(), 2);

        mock.increment(Duration::from_secs(3)); // 15 secs.
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(!cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.estimated_entry_count(), 1);

        mock.increment(Duration::from_secs(10)); // 25 secs
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(!cache.contains_key(&"a"));
        assert!(!cache.contains_key(&"b"));
        assert!(cache.is_table_empty());
    }

    #[tokio::test]
    async fn get_with() {
        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run five async tasks:
        //
        // Task1 will be the first task to call `get_with` for a key, so
        // its async block will be evaluated and then a &str value "task1" will be
        // inserted to the cache.
        let task1 = {
            let cache1 = cache.clone();
            async move {
                // Call `get_with` immediately.
                let v = cache1
                    .get_with(KEY, async {
                        // Wait for 300 ms and return a &str value.
                        Timer::after(Duration::from_millis(300)).await;
                        "task1"
                    })
                    .await;
                assert_eq!(v, "task1");
            }
        };

        // Task2 will be the second task to call `get_with` for the same
        // key, so its async block will not be evaluated. Once task1's async block
        // finishes, it will get the value inserted by task1's async block.
        let task2 = {
            let cache2 = cache.clone();
            async move {
                // Wait for 100 ms before calling `get_with`.
                Timer::after(Duration::from_millis(100)).await;
                let v = cache2.get_with(KEY, async { unreachable!() }).await;
                assert_eq!(v, "task1");
            }
        };

        // Task3 will be the third task to call `get_with` for the same
        // key. By the time it calls, task1's async block should have finished
        // already and the value should be already inserted to the cache. So its
        // async block will not be evaluated and will get the value insert by task1's
        // async block immediately.
        let task3 = {
            let cache3 = cache.clone();
            async move {
                // Wait for 400 ms before calling `get_with`.
                Timer::after(Duration::from_millis(400)).await;
                let v = cache3.get_with(KEY, async { unreachable!() }).await;
                assert_eq!(v, "task1");
            }
        };

        // Task4 will call `get` for the same key. It will call when task1's async
        // block is still running, so it will get none for the key.
        let task4 = {
            let cache4 = cache.clone();
            async move {
                // Wait for 200 ms before calling `get`.
                Timer::after(Duration::from_millis(200)).await;
                let maybe_v = cache4.get(&KEY);
                assert!(maybe_v.is_none());
            }
        };

        // Task5 will call `get` for the same key. It will call after task1's async
        // block finished, so it will get the value insert by task1's async block.
        let task5 = {
            let cache5 = cache.clone();
            async move {
                // Wait for 400 ms before calling `get`.
                Timer::after(Duration::from_millis(400)).await;
                let maybe_v = cache5.get(&KEY);
                assert_eq!(maybe_v, Some("task1"));
            }
        };

        futures_util::join!(task1, task2, task3, task4, task5);
    }

    #[tokio::test]
    async fn try_get_with() {
        use std::sync::Arc;

        // Note that MyError does not implement std::error::Error trait
        // like anyhow::Error.
        #[derive(Debug)]
        pub struct MyError(String);

        type MyResult<T> = Result<T, Arc<MyError>>;

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run eight async tasks:
        //
        // Task1 will be the first task to call `get_with` for a key, so
        // its async block will be evaluated and then an error will be returned.
        // Nothing will be inserted to the cache.
        let task1 = {
            let cache1 = cache.clone();
            async move {
                // Call `try_get_with` immediately.
                let v = cache1
                    .try_get_with(KEY, async {
                        // Wait for 300 ms and return an error.
                        Timer::after(Duration::from_millis(300)).await;
                        Err(MyError("task1 error".into()))
                    })
                    .await;
                assert!(v.is_err());
            }
        };

        // Task2 will be the second task to call `get_with` for the same
        // key, so its async block will not be evaluated. Once task1's async block
        // finishes, it will get the same error value returned by task1's async
        // block.
        let task2 = {
            let cache2 = cache.clone();
            async move {
                // Wait for 100 ms before calling `try_get_with`.
                Timer::after(Duration::from_millis(100)).await;
                let v: MyResult<_> = cache2.try_get_with(KEY, async { unreachable!() }).await;
                assert!(v.is_err());
            }
        };

        // Task3 will be the third task to call `get_with` for the same
        // key. By the time it calls, task1's async block should have finished
        // already, but the key still does not exist in the cache. So its async block
        // will be evaluated and then an okay &str value will be returned. That value
        // will be inserted to the cache.
        let task3 = {
            let cache3 = cache.clone();
            async move {
                // Wait for 400 ms before calling `try_get_with`.
                Timer::after(Duration::from_millis(400)).await;
                let v: MyResult<_> = cache3
                    .try_get_with(KEY, async {
                        // Wait for 300 ms and return an Ok(&str) value.
                        Timer::after(Duration::from_millis(300)).await;
                        Ok("task3")
                    })
                    .await;
                assert_eq!(v.unwrap(), "task3");
            }
        };

        // Task4 will be the fourth task to call `get_with` for the same
        // key. So its async block will not be evaluated. Once task3's async block
        // finishes, it will get the same okay &str value.
        let task4 = {
            let cache4 = cache.clone();
            async move {
                // Wait for 500 ms before calling `try_get_with`.
                Timer::after(Duration::from_millis(500)).await;
                let v: MyResult<_> = cache4.try_get_with(KEY, async { unreachable!() }).await;
                assert_eq!(v.unwrap(), "task3");
            }
        };

        // Task5 will be the fifth task to call `get_with` for the same
        // key. So its async block will not be evaluated. By the time it calls,
        // task3's async block should have finished already, so its async block will
        // not be evaluated and will get the value insert by task3's async block
        // immediately.
        let task5 = {
            let cache5 = cache.clone();
            async move {
                // Wait for 800 ms before calling `try_get_with`.
                Timer::after(Duration::from_millis(800)).await;
                let v: MyResult<_> = cache5.try_get_with(KEY, async { unreachable!() }).await;
                assert_eq!(v.unwrap(), "task3");
            }
        };

        // Task6 will call `get` for the same key. It will call when task1's async
        // block is still running, so it will get none for the key.
        let task6 = {
            let cache6 = cache.clone();
            async move {
                // Wait for 200 ms before calling `get`.
                Timer::after(Duration::from_millis(200)).await;
                let maybe_v = cache6.get(&KEY);
                assert!(maybe_v.is_none());
            }
        };

        // Task7 will call `get` for the same key. It will call after task1's async
        // block finished with an error. So it will get none for the key.
        let task7 = {
            let cache7 = cache.clone();
            async move {
                // Wait for 400 ms before calling `get`.
                Timer::after(Duration::from_millis(400)).await;
                let maybe_v = cache7.get(&KEY);
                assert!(maybe_v.is_none());
            }
        };

        // Task8 will call `get` for the same key. It will call after task3's async
        // block finished, so it will get the value insert by task3's async block.
        let task8 = {
            let cache8 = cache.clone();
            async move {
                // Wait for 800 ms before calling `get`.
                Timer::after(Duration::from_millis(800)).await;
                let maybe_v = cache8.get(&KEY);
                assert_eq!(maybe_v, Some("task3"));
            }
        };

        futures_util::join!(task1, task2, task3, task4, task5, task6, task7, task8);
    }

    #[tokio::test]
    // https://github.com/moka-rs/moka/issues/43
    async fn handle_panic_in_get_with() {
        use tokio::time::{sleep, Duration};

        let cache = Cache::new(16);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(0));
        {
            let cache_ref = cache.clone();
            let semaphore_ref = semaphore.clone();
            tokio::task::spawn(async move {
                let _ = cache_ref
                    .get_with(1, async move {
                        semaphore_ref.add_permits(1);
                        sleep(Duration::from_millis(50)).await;
                        panic!("Panic during try_get_with");
                    })
                    .await;
            });
        }
        let _ = semaphore.acquire().await.expect("semaphore acquire failed");
        assert_eq!(cache.get_with(1, async { 5 }).await, 5);
    }

    #[tokio::test]
    // https://github.com/moka-rs/moka/issues/43
    async fn handle_panic_in_try_get_with() {
        use tokio::time::{sleep, Duration};

        let cache = Cache::new(16);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(0));
        {
            let cache_ref = cache.clone();
            let semaphore_ref = semaphore.clone();
            tokio::task::spawn(async move {
                let _ = cache_ref
                    .try_get_with(1, async move {
                        semaphore_ref.add_permits(1);
                        sleep(Duration::from_millis(50)).await;
                        panic!("Panic during try_get_with");
                    })
                    .await as Result<_, Arc<Infallible>>;
            });
        }
        let _ = semaphore.acquire().await.expect("semaphore acquire failed");
        assert_eq!(
            cache.try_get_with(1, async { Ok(5) }).await as Result<_, Arc<Infallible>>,
            Ok(5)
        );
    }

    #[tokio::test]
    // https://github.com/moka-rs/moka/issues/59
    async fn abort_get_with() {
        use tokio::time::{sleep, Duration};

        let cache = Cache::new(16);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(0));

        let handle;
        {
            let cache_ref = cache.clone();
            let semaphore_ref = semaphore.clone();

            handle = tokio::task::spawn(async move {
                let _ = cache_ref
                    .get_with(1, async move {
                        semaphore_ref.add_permits(1);
                        sleep(Duration::from_millis(50)).await;
                        unreachable!();
                    })
                    .await;
            });
        }

        let _ = semaphore.acquire().await.expect("semaphore acquire failed");
        handle.abort();

        assert_eq!(cache.get_with(1, async { 5 }).await, 5);
    }

    #[tokio::test]
    // https://github.com/moka-rs/moka/issues/59
    async fn abort_try_get_with() {
        use tokio::time::{sleep, Duration};

        let cache = Cache::new(16);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(0));

        let handle;
        {
            let cache_ref = cache.clone();
            let semaphore_ref = semaphore.clone();

            handle = tokio::task::spawn(async move {
                let _ = cache_ref
                    .try_get_with(1, async move {
                        semaphore_ref.add_permits(1);
                        sleep(Duration::from_millis(50)).await;
                        unreachable!();
                    })
                    .await as Result<_, Arc<Infallible>>;
            });
        }

        let _ = semaphore.acquire().await.expect("semaphore acquire failed");
        handle.abort();

        assert_eq!(
            cache.try_get_with(1, async { Ok(5) }).await as Result<_, Arc<Infallible>>,
            Ok(5)
        );
    }
}
