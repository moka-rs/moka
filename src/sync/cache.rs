use super::{
    base_cache::{BaseCache, HouseKeeperArc, MAX_SYNC_REPEATS, WRITE_RETRY_INTERVAL_MICROS},
    debug_fmt::{CacheRef, DebugFmt},
    housekeeper::InnerSync,
    iter::{Iter, ScanningGet},
    value_initializer::{InitResult, ValueInitializer},
    CacheBuilder, ConcurrentCacheExt, PredicateId, Weigher, WriteOp,
};
use crate::{Policy, PredicateError};

use crossbeam_channel::{Sender, TrySendError};
use std::{
    any::TypeId,
    borrow::Borrow,
    collections::hash_map::RandomState,
    fmt,
    hash::{BuildHasher, Hash},
    sync::Arc,
    time::Duration,
};

/// A thread-safe concurrent synchronous in-memory cache.
///
/// `Cache` supports full concurrency of retrievals and a high expected concurrency
/// for updates.
///
/// `Cache` utilizes a lock-free concurrent hash table as the central key-value
/// storage. `Cache` performs a best-effort bounding of the map using an entry
/// replacement algorithm to determine which entries to evict when the capacity is
/// exceeded.
///
/// # Examples
///
/// Cache entries are manually added using [`insert`](#method.insert) or
/// [`get_with`](#method.get_with) methods, and are stored in
/// the cache until either evicted or manually invalidated.
///
/// Here's an example of reading and updating a cache by using multiple threads:
///
/// ```rust
/// use moka::sync::Cache;
///
/// use std::thread;
///
/// fn value(n: usize) -> String {
///     format!("value {}", n)
/// }
///
/// const NUM_THREADS: usize = 16;
/// const NUM_KEYS_PER_THREAD: usize = 64;
///
/// // Create a cache that can store up to 10,000 entries.
/// let cache = Cache::new(10_000);
///
/// // Spawn threads and read and update the cache simultaneously.
/// let threads: Vec<_> = (0..NUM_THREADS)
///     .map(|i| {
///         // To share the same cache across the threads, clone it.
///         // This is a cheap operation.
///         let my_cache = cache.clone();
///         let start = i * NUM_KEYS_PER_THREAD;
///         let end = (i + 1) * NUM_KEYS_PER_THREAD;
///
///         thread::spawn(move || {
///             // Insert 64 entries. (NUM_KEYS_PER_THREAD = 64)
///             for key in start..end {
///                 my_cache.insert(key, value(key));
///                 // get() returns Option<String>, a clone of the stored value.
///                 assert_eq!(my_cache.get(&key), Some(value(key)));
///             }
///
///             // Invalidate every 4 element of the inserted entries.
///             for key in (start..end).step_by(4) {
///                 my_cache.invalidate(&key);
///             }
///         })
///     })
///     .collect();
///
/// // Wait for all threads to complete.
/// threads.into_iter().for_each(|t| t.join().expect("Failed"));
///
/// // Verify the result.
/// for key in 0..(NUM_THREADS * NUM_KEYS_PER_THREAD) {
///     if key % 4 == 0 {
///         assert_eq!(cache.get(&key), None);
///     } else {
///         assert_eq!(cache.get(&key), Some(value(key)));
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
/// use std::convert::TryInto;
/// use moka::sync::Cache;
///
/// // Evict based on the number of entries in the cache.
/// let cache = Cache::builder()
///     // Up to 10,000 entries.
///     .max_capacity(10_000)
///     // Create the cache.
///     .build();
/// cache.insert(1, "one".to_string());
///
/// // Evict based on the byte length of strings in the cache.
/// let cache = Cache::builder()
///     // A weigher closure takes &K and &V and returns a u32
///     // representing the relative size of the entry.
///     .weigher(|_key, value: &String| -> u32 {
///         value.len().try_into().unwrap_or(u32::MAX)
///     })
///     // This cache will hold up to 32MiB of values.
///     .max_capacity(32 * 1024 * 1024)
///     .build();
/// cache.insert(2, "two".to_string());
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
/// use moka::sync::Cache;
/// use std::time::Duration;
///
/// let cache = Cache::builder()
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
/// # Thread Safety
///
/// All methods provided by the `Cache` are considered thread-safe, and can be safely
/// accessed by multiple concurrent threads.
///
/// - `Cache<K, V, S>` requires trait bounds `Send`, `Sync` and `'static` for `K`
///   (key), `V` (value) and `S` (hasher state).
/// - `Cache<K, V, S>` will implement `Send` and `Sync`.
///
/// # Sharing a cache across threads
///
/// To share a cache across threads, do one of the followings:
///
/// - Create a clone of the cache by calling its `clone` method and pass it to other
///   thread.
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

// NOTE: We cannot do `#[derive(Clone)]` because it will add `Clone` bound to `K`.
impl<K, V, S> Clone for Cache<K, V, S> {
    /// Makes a clone of this shared cache.
    ///
    /// This operation is cheap as it only creates thread-safe reference counted
    /// pointers to the shared internal data structures.
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            value_initializer: Arc::clone(&self.value_initializer),
        }
    }
}

impl<K, V, S> fmt::Debug for Cache<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.debug_fmt().default_fmt().fmt(f)
    }
}

impl<K, V, S> Cache<K, V, S> {
    /// Returns a read-only cache policy of this cache.
    ///
    /// At this time, cache policy cannot be modified after cache creation.
    /// A future version may support to modify it.
    pub fn policy(&self) -> Policy {
        self.base.policy()
    }

    pub fn debug_fmt(&self) -> DebugFmt<'_, K, V, S> {
        DebugFmt::new(CacheRef::Sync(self))
    }

    pub fn entry_count(&self) -> u64 {
        self.base.entry_count()
    }

    pub fn weighted_size(&self) -> u64 {
        self.base.weighted_size()
    }
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

    /// Returns a [`CacheBuilder`][builder-struct], which can builds a `Cache` or
    /// `SegmentedCache` with various configuration knobs.
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

    pub(crate) fn contains_key_with_hash<Q>(&self, key: &Q, hash: u64) -> bool
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.contains_key_with_hash(key, hash)
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

    pub(crate) fn get_with_hash<Q>(&self, key: &Q, hash: u64) -> Option<V>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.get_with_hash(key, hash)
    }

    /// Deprecated, replaced with [`get_with`](#method.get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `get_with`")]
    pub fn get_or_insert_with(&self, key: K, init: impl FnOnce() -> V) -> V {
        self.get_with(key, init)
    }

    /// Deprecated, replaced with [`try_get_with`](#method.try_get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `try_get_with`")]
    pub fn get_or_try_insert_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        self.try_get_with(key, init)
    }

    /// Ensures the value of the key exists by inserting the output of the `init`
    /// closure if not exist, and returns a _clone_ of the value.
    ///
    /// This method prevents to evaluate the `init` closure multiple times on the
    /// same key even if the method is concurrently called by many threads; only one
    /// of the calls evaluates its closure, and other calls wait for that closure to
    /// complete.
    ///
    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    /// use std::{sync::Arc, thread, time::Duration};
    ///
    /// const TEN_MIB: usize = 10 * 1024 * 1024; // 10MiB
    /// let cache = Cache::new(100);
    ///
    /// // Spawn four threads.
    /// let threads: Vec<_> = (0..4_u8)
    ///     .map(|task_id| {
    ///         let my_cache = cache.clone();
    ///         thread::spawn(move || {
    ///             println!("Thread {} started.", task_id);
    ///
    ///             // Try to insert and get the value for key1. Although all four
    ///             // threads will call `get_with` at the same time, the `init` closure
    ///             // must be evaluated only once.
    ///             let value = my_cache.get_with("key1", || {
    ///                 println!("Thread {} inserting a value.", task_id);
    ///                 Arc::new(vec![0u8; TEN_MIB])
    ///             });
    ///
    ///             // Ensure the value exists now.
    ///             assert_eq!(value.len(), TEN_MIB);
    ///             thread::sleep(Duration::from_millis(10));
    ///             assert!(my_cache.get(&"key1").is_some());
    ///
    ///             println!("Thread {} got the value. (len: {})", task_id, value.len());
    ///         })
    ///     })
    ///     .collect();
    ///
    /// // Wait all threads to complete.
    /// threads
    ///     .into_iter()
    ///     .for_each(|t| t.join().expect("Thread failed"));
    /// ```
    ///
    /// **Result**
    ///
    /// - The `init` closure was called exactly once by thread 1.
    /// - Other threads were blocked until thread 1 inserted the value.
    ///
    /// ```console
    /// Thread 1 started.
    /// Thread 0 started.
    /// Thread 3 started.
    /// Thread 2 started.
    /// Thread 1 inserting a value.
    /// Thread 2 got the value. (len: 10485760)
    /// Thread 1 got the value. (len: 10485760)
    /// Thread 0 got the value. (len: 10485760)
    /// Thread 3 got the value. (len: 10485760)
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics when the `init` closure has been panicked. When it
    /// happens, only the caller whose `init` closure panicked will get the panic
    /// (e.g. only thread 1 in the above sample). If there are other calls in
    /// progress (e.g. thread 0, 2 and 3 above), this method will restart and resolve
    /// one of the remaining `init` closure.
    ///
    pub fn get_with(&self, key: K, init: impl FnOnce() -> V) -> V {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        let replace_if = None as Option<fn(&V) -> bool>;
        self.get_or_insert_with_hash_and_fun(key, hash, init, replace_if)
    }

    /// Works like [`get_with`](#method.get_with), but takes an additional
    /// `replace_if` closure.
    ///
    /// This method will evaluate the `init` closure and insert the output to the
    /// cache when:
    ///
    /// - The key does not exist.
    /// - Or, `replace_if` closure returns `true`.
    pub fn get_with_if(
        &self,
        key: K,
        init: impl FnOnce() -> V,
        replace_if: impl FnMut(&V) -> bool,
    ) -> V {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.get_or_insert_with_hash_and_fun(key, hash, init, Some(replace_if))
    }

    pub(crate) fn get_or_insert_with_hash_and_fun(
        &self,
        key: Arc<K>,
        hash: u64,
        init: impl FnOnce() -> V,
        mut replace_if: Option<impl FnMut(&V) -> bool>,
    ) -> V {
        match (self.base.get_with_hash(&key, hash), &mut replace_if) {
            (Some(v), None) => return v,
            (Some(v), Some(cond)) => {
                if !cond(&v) {
                    return v;
                }
            }
            _ => (),
        }

        match self.value_initializer.init_or_read(Arc::clone(&key), init) {
            InitResult::Initialized(v) => {
                self.insert_with_hash(Arc::clone(&key), hash, v.clone());
                self.value_initializer
                    .remove_waiter(&key, TypeId::of::<()>());
                v
            }
            InitResult::ReadExisting(v) => v,
            InitResult::InitErr(_) => unreachable!(),
        }
    }

    /// Try to ensure the value of the key exists by inserting an `Ok` result of the
    /// init closure if not exist, and returns a _clone_ of the value or the `Err`
    /// returned by the closure.
    ///
    /// This method prevents to evaluate the init closure multiple times on the same
    /// key even if the method is concurrently called by many threads; only one of
    /// the calls evaluates its closure (as long as these closures return the same
    /// error type), and other calls wait for that closure to complete.
    ///
    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    /// use std::{path::Path, time::Duration, thread};
    ///
    /// /// This function tries to get the file size in bytes.
    /// fn get_file_size(thread_id: u8, path: impl AsRef<Path>) -> Result<u64, std::io::Error> {
    ///     println!("get_file_size() called by thread {}.", thread_id);
    ///     Ok(std::fs::metadata(path)?.len())
    /// }
    ///
    /// let cache = Cache::new(100);
    ///
    /// // Spawn four threads.
    /// let threads: Vec<_> = (0..4_u8)
    ///     .map(|thread_id| {
    ///         let my_cache = cache.clone();
    ///         thread::spawn(move || {
    ///             println!("Thread {} started.", thread_id);
    ///
    ///             // Try to insert and get the value for key1. Although all four
    ///             // threads will call `try_get_with` at the same time,
    ///             // get_file_size() must be called only once.
    ///             let value = my_cache.try_get_with(
    ///                 "key1",
    ///                 || get_file_size(thread_id, "./Cargo.toml"),
    ///             );
    ///
    ///             // Ensure the value exists now.
    ///             assert!(value.is_ok());
    ///             thread::sleep(Duration::from_millis(10));
    ///             assert!(my_cache.get(&"key1").is_some());
    ///
    ///             println!(
    ///                 "Thread {} got the value. (len: {})",
    ///                 thread_id,
    ///                 value.unwrap()
    ///             );
    ///         })
    ///     })
    ///     .collect();
    ///
    /// // Wait all threads to complete.
    /// threads
    ///     .into_iter()
    ///     .for_each(|t| t.join().expect("Thread failed"));
    /// ```
    ///
    /// **Result**
    ///
    /// - `get_file_size()` was called exactly once by thread 1.
    /// - Other threads were blocked until thread 1 inserted the value.
    ///
    /// ```console
    /// Thread 1 started.
    /// Thread 2 started.
    /// get_file_size() called by thread 1.
    /// Thread 3 started.
    /// Thread 0 started.
    /// Thread 2 got the value. (len: 1466)
    /// Thread 0 got the value. (len: 1466)
    /// Thread 1 got the value. (len: 1466)
    /// Thread 3 got the value. (len: 1466)
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics when the `init` closure has been panicked. When it
    /// happens, only the caller whose `init` closure panicked will get the panic
    /// (e.g. only thread 1 in the above sample). If there are other calls in
    /// progress (e.g. thread 0, 2 and 3 above), this method will restart and resolve
    /// one of the remaining `init` closure.
    ///
    pub fn try_get_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.get_or_try_insert_with_hash_and_fun(key, hash, init)
    }

    pub(crate) fn get_or_try_insert_with_hash_and_fun<F, E>(
        &self,
        key: Arc<K>,
        hash: u64,
        init: F,
    ) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        if let Some(v) = self.get_with_hash(&key, hash) {
            return Ok(v);
        }

        match self
            .value_initializer
            .try_init_or_read(Arc::clone(&key), init)
        {
            InitResult::Initialized(v) => {
                self.insert_with_hash(Arc::clone(&key), hash, v.clone());
                self.value_initializer
                    .remove_waiter(&key, TypeId::of::<E>());
                Ok(v)
            }
            InitResult::ReadExisting(v) => Ok(v),
            InitResult::InitErr(e) => Err(e),
        }
    }

    /// Inserts a key-value pair into the cache.
    ///
    /// If the cache has this key present, the value is updated.
    pub fn insert(&self, key: K, value: V) {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.insert_with_hash(key, hash, value)
    }

    pub(crate) fn insert_with_hash(&self, key: Arc<K>, hash: u64, value: V) {
        let op = self.base.do_insert_with_hash(key, hash, value);
        let hk = self.base.housekeeper.as_ref();
        Self::schedule_write_op(&self.base.write_op_ch, op, hk).expect("Failed to insert");
    }

    /// Discards any cached value for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub fn invalidate<Q>(&self, key: &Q)
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.base.hash(key);
        self.invalidate_with_hash(key, hash);
    }

    pub(crate) fn invalidate_with_hash<Q>(&self, key: &Q, hash: u64)
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(kv) = self.base.remove_entry(key, hash) {
            let op = WriteOp::Remove(kv);
            let hk = self.base.housekeeper.as_ref();
            Self::schedule_write_op(&self.base.write_op_ch, op, hk).expect("Failed to remove");
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

    pub(crate) fn invalidate_entries_with_arc_fun<F>(
        &self,
        predicate: Arc<F>,
    ) -> Result<PredicateId, PredicateError>
    where
        F: Fn(&K, &V) -> bool + Send + Sync + 'static,
    {
        self.base.invalidate_entries_if(predicate)
    }

    /// Creates an iterator visiting all key-value pairs in arbitrary order. The
    /// iterator element type is `(Arc<K>, V)`, where `V` is a clone of a stored
    /// value.
    ///
    /// Iterators do not block concurrent reads and writes on the cache. An entry can
    /// be inserted to, invalidated or evicted from a cache while iterators are alive
    /// on the same cache.
    ///
    /// Unlike the `get` method, visiting entries via an iterator do not update the
    /// historic popularity estimator or reset idle timers for keys.
    ///
    /// # Guarantees
    ///
    /// In order to allow concurrent access to the cache, iterator's `next` method
    /// does _not_ guarantee the following:
    ///
    /// - It does not guarantee to return a key-value pair (an entry) if its key has
    ///   been inserted to the cache _after_ the iterator was created.
    ///   - Such an entry may or may not be returned depending on key's hash and
    ///     timing.
    ///
    /// and the `next` method guarantees the followings:
    ///
    /// - It guarantees not to return the same entry more than once.
    /// - It guarantees not to return an entry if it has been removed from the cache
    ///   after the iterator was created.
    ///     - Note: An entry can be removed by following reasons:
    ///         - Manually invalidated.
    ///         - Expired (e.g. time-to-live).
    ///         - Evicted as the cache capacity exceeded.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache = Cache::new(100);
    /// cache.insert("Julia", 14);
    ///
    /// let mut iter = cache.iter();
    /// let (k, v) = iter.next().unwrap(); // (Arc<K>, V)
    /// assert_eq!(*k, "Julia");
    /// assert_eq!(v, 14);
    ///
    /// assert!(iter.next().is_none());
    /// ```
    ///
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter::with_single_cache_segment(&self.base, self.num_cht_segments())
    }
}

impl<'a, K, V, S> IntoIterator for &'a Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    type Item = (Arc<K>, V);

    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
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

//
// Iterator support
//
impl<K, V, S> ScanningGet<K, V> for Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn num_cht_segments(&self) -> usize {
        self.base.num_cht_segments()
    }

    fn scanning_get(&self, key: &Arc<K>) -> Option<V> {
        self.base.scanning_get(key)
    }

    fn keys(&self, cht_segment: usize) -> Option<Vec<Arc<K>>> {
        self.base.keys(cht_segment)
    }
}

//
// private methods
//
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    #[inline]
    fn schedule_write_op(
        ch: &Sender<WriteOp<K, V>>,
        op: WriteOp<K, V>,
        housekeeper: Option<&HouseKeeperArc<K, V, S>>,
    ) -> Result<(), TrySendError<WriteOp<K, V>>> {
        let mut op = op;

        // NOTES:
        // - This will block when the channel is full.
        // - We are doing a busy-loop here. We were originally calling `ch.send(op)?`,
        //   but we got a notable performance degradation.
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
    pub(crate) fn is_table_empty(&self) -> bool {
        self.entry_count() == 0
    }

    pub(crate) fn invalidation_predicate_count(&self) -> usize {
        self.base.invalidation_predicate_count()
    }

    pub(crate) fn reconfigure_for_testing(&mut self) {
        self.base.reconfigure_for_testing();
    }

    pub(crate) fn set_expiration_clock(&self, clock: Option<crate::common::time::Clock>) {
        self.base.set_expiration_clock(clock);
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{Cache, ConcurrentCacheExt};
    use crate::common::time::Clock;

    use std::{convert::Infallible, sync::Arc, time::Duration};

    #[test]
    fn basic_single_thread() {
        let mut cache = Cache::new(3);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy");
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
        cache.insert("d", "david"); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", "david");
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher than c's.
        cache.insert("d", "dennis");
        cache.sync();
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some("dennis"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"c"));
        assert!(cache.contains_key(&"d"));

        cache.invalidate(&"b");
        assert_eq!(cache.get(&"b"), None);
        assert!(!cache.contains_key(&"b"));
    }

    #[test]
    fn size_aware_eviction() {
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

        cache.insert("a", alice);
        cache.insert("b", bob);
        assert_eq!(cache.get(&"a"), Some(alice));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.get(&"b"), Some(bob));
        cache.sync();
        // order (LRU -> MRU) and counts: a -> 1, b -> 1

        cache.insert("c", cindy);
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
        cache.insert("d", david); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", david);
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        cache.insert("d", david);
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 3
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", david);
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 4

        // Finally "d" should be admitted by evicting "c" and "a".
        cache.insert("d", dennis);
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
        cache.insert("b", bill);
        cache.sync();
        assert_eq!(cache.get(&"b"), Some(bill));
        assert_eq!(cache.get(&"d"), None);
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"d"));

        // Re-add "a" (w: 10) and update "b" with "bob" (w: 20 -> 15).
        cache.insert("a", alice);
        cache.insert("b", bob);
        cache.sync();
        assert_eq!(cache.get(&"a"), Some(alice));
        assert_eq!(cache.get(&"b"), Some(bob));
        assert_eq!(cache.get(&"d"), None);
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"d"));

        // Verify the sizes.
        assert_eq!(cache.entry_count(), 2);
        assert_eq!(cache.weighted_size(), 25);
    }

    #[test]
    fn basic_multi_threads() {
        let num_threads = 4;
        let cache = Cache::new(100);

        // https://rust-lang.github.io/rust-clippy/master/index.html#needless_collect
        #[allow(clippy::needless_collect)]
        let handles = (0..num_threads)
            .map(|id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    cache.insert(10, format!("{}-100", id));
                    cache.get(&10);
                    cache.insert(20, format!("{}-200", id));
                    cache.invalidate(&10);
                })
            })
            .collect::<Vec<_>>();

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        assert!(cache.get(&10).is_none());
        assert!(cache.get(&20).is_some());
        assert!(!cache.contains_key(&10));
        assert!(cache.contains_key(&20));
    }

    #[test]
    fn invalidate_all() {
        let mut cache = Cache::new(100);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), Some("cindy"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(cache.contains_key(&"c"));
        cache.sync();

        cache.invalidate_all();
        cache.sync();

        cache.insert("d", "david");
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

    #[test]
    fn invalidate_entries_if() -> Result<(), Box<dyn std::error::Error>> {
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

        cache.insert(0, "alice");
        cache.insert(1, "bob");
        cache.insert(2, "alex");
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
        assert_eq!(cache.base.invalidation_predicate_count(), 1);

        mock.increment(Duration::from_secs(5)); // 10 secs from the start.

        cache.insert(3, "alice");

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

        assert_eq!(cache.entry_count(), 2);
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

        assert_eq!(cache.entry_count(), 0);
        assert_eq!(cache.invalidation_predicate_count(), 0);

        Ok(())
    }

    #[test]
    fn time_to_live() {
        let mut cache = Cache::builder()
            .max_capacity(100)
            .time_to_live(Duration::from_secs(10))
            .build();

        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.
        cache.sync();

        assert_eq!(cache.get(&"a"), Some("alice"));
        assert!(cache.contains_key(&"a"));

        mock.increment(Duration::from_secs(5)); // 10 secs.
        assert_eq!(cache.get(&"a"), None);
        assert!(!cache.contains_key(&"a"));

        assert_eq!(cache.iter().count(), 0);

        cache.sync();
        assert!(cache.is_table_empty());

        cache.insert("b", "bob");
        cache.sync();

        assert_eq!(cache.entry_count(), 1);

        mock.increment(Duration::from_secs(5)); // 15 secs.
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.entry_count(), 1);

        cache.insert("b", "bill");
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 20 secs
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bill"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.entry_count(), 1);

        mock.increment(Duration::from_secs(5)); // 25 secs

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(!cache.contains_key(&"a"));
        assert!(!cache.contains_key(&"b"));

        assert_eq!(cache.iter().count(), 0);

        cache.sync();
        assert!(cache.is_table_empty());
    }

    #[test]
    fn time_to_idle() {
        let mut cache = Cache::builder()
            .max_capacity(100)
            .time_to_idle(Duration::from_secs(10))
            .build();

        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.
        cache.sync();

        assert_eq!(cache.get(&"a"), Some("alice"));

        mock.increment(Duration::from_secs(5)); // 10 secs.
        cache.sync();

        cache.insert("b", "bob");
        cache.sync();

        assert_eq!(cache.entry_count(), 2);

        mock.increment(Duration::from_secs(2)); // 12 secs.
        cache.sync();

        // contains_key does not reset the idle timer for the key.
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        cache.sync();

        assert_eq!(cache.entry_count(), 2);

        mock.increment(Duration::from_secs(3)); // 15 secs.
        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(!cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));

        assert_eq!(cache.iter().count(), 1);

        cache.sync();
        assert_eq!(cache.entry_count(), 1);

        mock.increment(Duration::from_secs(10)); // 25 secs
        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(!cache.contains_key(&"a"));
        assert!(!cache.contains_key(&"b"));

        assert_eq!(cache.iter().count(), 0);

        cache.sync();
        assert!(cache.is_table_empty());
    }

    #[test]
    fn test_iter() {
        const NUM_KEYS: usize = 50;

        fn make_value(key: usize) -> String {
            format!("val: {}", key)
        }

        let cache = Cache::builder()
            .max_capacity(100)
            .time_to_idle(Duration::from_secs(10))
            .build();

        for key in 0..NUM_KEYS {
            cache.insert(key, make_value(key));
        }

        let mut key_set = std::collections::HashSet::new();

        for (key, value) in &cache {
            assert_eq!(value, make_value(*key));

            key_set.insert(*key);
        }

        // Ensure there are no missing or duplicate keys in the iteration.
        assert_eq!(key_set.len(), NUM_KEYS);
    }

    /// Runs 16 threads at the same time and ensures no deadlock occurs.
    ///
    /// - Eight of the threads will update key-values in the cache.
    /// - Eight others will iterate the cache.
    ///
    #[test]
    fn test_iter_multi_threads() {
        use std::collections::HashSet;

        const NUM_KEYS: usize = 1024;
        const NUM_THREADS: usize = 16;

        fn make_value(key: usize) -> String {
            format!("val: {}", key)
        }

        let cache = Cache::builder()
            .max_capacity(2048)
            .time_to_idle(Duration::from_secs(10))
            .build();

        // Initialize the cache.
        for key in 0..NUM_KEYS {
            cache.insert(key, make_value(key));
        }

        let rw_lock = Arc::new(std::sync::RwLock::<()>::default());
        let write_lock = rw_lock.write().unwrap();

        // https://rust-lang.github.io/rust-clippy/master/index.html#needless_collect
        #[allow(clippy::needless_collect)]
        let handles = (0..NUM_THREADS)
            .map(|n| {
                let cache = cache.clone();
                let rw_lock = Arc::clone(&rw_lock);

                if n % 2 == 0 {
                    // This thread will update the cache.
                    std::thread::spawn(move || {
                        let read_lock = rw_lock.read().unwrap();
                        for key in 0..NUM_KEYS {
                            // TODO: Update keys in a random order?
                            cache.insert(key, make_value(key));
                        }
                        std::mem::drop(read_lock);
                    })
                } else {
                    // This thread will iterate the cache.
                    std::thread::spawn(move || {
                        let read_lock = rw_lock.read().unwrap();
                        let mut key_set = HashSet::new();
                        for (key, value) in &cache {
                            assert_eq!(value, make_value(*key));
                            key_set.insert(*key);
                        }
                        // Ensure there are no missing or duplicate keys in the iteration.
                        assert_eq!(key_set.len(), NUM_KEYS);
                        std::mem::drop(read_lock);
                    })
                }
            })
            .collect::<Vec<_>>();

        // Let these threads to run by releasing the write lock.
        std::mem::drop(write_lock);

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        // Ensure there are no missing or duplicate keys in the iteration.
        let key_set = cache.iter().map(|(k, _v)| *k).collect::<HashSet<_>>();
        assert_eq!(key_set.len(), NUM_KEYS);
    }

    #[test]
    fn get_with() {
        use std::thread::{sleep, spawn};

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run five threads:
        //
        // Thread1 will be the first thread to call `get_with` for a key, so its init
        // closure will be evaluated and then a &str value "thread1" will be inserted
        // to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `get_with` immediately.
                let v = cache1.get_with(KEY, || {
                    // Wait for 300 ms and return a &str value.
                    sleep(Duration::from_millis(300));
                    "thread1"
                });
                assert_eq!(v, "thread1");
            })
        };

        // Thread2 will be the second thread to call `get_with` for the same key, so
        // its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the value inserted by thread1's init closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `get_with`.
                sleep(Duration::from_millis(100));
                let v = cache2.get_with(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already and
        // the value should be already inserted to the cache. So its init closure
        // will not be evaluated and will get the value insert by thread1's init
        // closure immediately.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get_with`.
                sleep(Duration::from_millis(400));
                let v = cache3.get_with(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread4 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache4.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread5 will call `get` for the same key. It will call after thread1's init
        // closure finished, so it will get the value insert by thread1's init closure.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache5.get(&KEY);
                assert_eq!(maybe_v, Some("thread1"));
            })
        };

        for t in vec![thread1, thread2, thread3, thread4, thread5] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn get_with_if() {
        use std::thread::{sleep, spawn};

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run seven threads:
        //
        // Thread1 will be the first thread to call `get_with_if` for a key, so its
        // init closure will be evaluated and then a &str value "thread1" will be
        // inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `get_with` immediately.
                let v = cache1.get_with_if(
                    KEY,
                    || {
                        // Wait for 300 ms and return a &str value.
                        sleep(Duration::from_millis(300));
                        "thread1"
                    },
                    |_v| unreachable!(),
                );
                assert_eq!(v, "thread1");
            })
        };

        // Thread2 will be the second thread to call `get_with_if` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the value inserted by thread1's init closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `get_with`.
                sleep(Duration::from_millis(100));
                let v = cache2.get_with_if(KEY, || unreachable!(), |_v| unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread3 will be the third thread to call `get_with_if` for the same
        // key. By the time it calls, thread1's init closure should have finished
        // already and the value should be already inserted to the cache. Also
        // thread3's `replace_if` closure returns `false`. So its init closure will
        // not be evaluated and will get the value inserted by thread1's init closure
        // immediately.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 350 ms before calling `get_with_if`.
                sleep(Duration::from_millis(350));
                let v = cache3.get_with_if(
                    KEY,
                    || unreachable!(),
                    |v| {
                        assert_eq!(v, &"thread1");
                        false
                    },
                );
                assert_eq!(v, "thread1");
            })
        };

        // Thread4 will be the fourth thread to call `get_with_if` for the same
        // key. The value should have been already inserted to the cache by
        // thread1. However thread4's `replace_if` closure returns `true`. So its
        // init closure will be evaluated to replace the current value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get_with_if`.
                sleep(Duration::from_millis(400));
                let v = cache4.get_with_if(
                    KEY,
                    || "thread4",
                    |v| {
                        assert_eq!(v, &"thread1");
                        true
                    },
                );
                assert_eq!(v, "thread4");
            })
        };

        // Thread5 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache5.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 350 ms before calling `get`.
                sleep(Duration::from_millis(350));
                let maybe_v = cache6.get(&KEY);
                assert_eq!(maybe_v, Some("thread1"));
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished, so it will get the value insert by thread1's init closure.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 450 ms before calling `get`.
                sleep(Duration::from_millis(450));
                let maybe_v = cache7.get(&KEY);
                assert_eq!(maybe_v, Some("thread4"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn try_get_with() {
        use std::{
            sync::Arc,
            thread::{sleep, spawn},
        };

        // Note that MyError does not implement std::error::Error trait like
        // anyhow::Error.
        #[derive(Debug)]
        pub struct MyError(String);

        type MyResult<T> = Result<T, Arc<MyError>>;

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run eight threads:
        //
        // Thread1 will be the first thread to call `try_get_with` for a key, so its
        // init closure will be evaluated and then an error will be returned. Nothing
        // will be inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `try_get_with` immediately.
                let v = cache1.try_get_with(KEY, || {
                    // Wait for 300 ms and return an error.
                    sleep(Duration::from_millis(300));
                    Err(MyError("thread1 error".into()))
                });
                assert!(v.is_err());
            })
        };

        // Thread2 will be the second thread to call `try_get_with` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the same error value returned by thread1's init
        // closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `try_get_with`.
                sleep(Duration::from_millis(100));
                let v: MyResult<_> = cache2.try_get_with(KEY, || unreachable!());
                assert!(v.is_err());
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already,
        // but the key still does not exist in the cache. So its init closure will be
        // evaluated and then an okay &str value will be returned. That value will be
        // inserted to the cache.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `try_get_with`.
                sleep(Duration::from_millis(400));
                let v: MyResult<_> = cache3.try_get_with(KEY, || {
                    // Wait for 300 ms and return an Ok(&str) value.
                    sleep(Duration::from_millis(300));
                    Ok("thread3")
                });
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // thread4 will be the fourth thread to call `try_get_with` for the same
        // key. So its init closure will not be evaluated. Once thread3's init
        // closure finishes, it will get the same okay &str value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 500 ms before calling `try_get_with`.
                sleep(Duration::from_millis(500));
                let v: MyResult<_> = cache4.try_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread5 will be the fifth thread to call `try_get_with` for the same
        // key. So its init closure will not be evaluated. By the time it calls,
        // thread3's init closure should have finished already, so its init closure
        // will not be evaluated and will get the value insert by thread3's init
        // closure immediately.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `try_get_with`.
                sleep(Duration::from_millis(800));
                let v: MyResult<_> = cache5.try_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache6.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished with an error. So it will get none for the key.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache7.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread8 will call `get` for the same key. It will call after thread3's init
        // closure finished, so it will get the value insert by thread3's init closure.
        let thread8 = {
            let cache8 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `get`.
                sleep(Duration::from_millis(800));
                let maybe_v = cache8.get(&KEY);
                assert_eq!(maybe_v, Some("thread3"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7, thread8,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    // https://github.com/moka-rs/moka/issues/43
    fn handle_panic_in_get_with() {
        use std::{sync::Barrier, thread};

        let cache = Cache::new(16);
        let barrier = Arc::new(Barrier::new(2));
        {
            let cache_ref = cache.clone();
            let barrier_ref = barrier.clone();
            thread::spawn(move || {
                let _ = cache_ref.get_with(1, || {
                    barrier_ref.wait();
                    thread::sleep(Duration::from_millis(50));
                    panic!("Panic during get_with");
                });
            });
        }

        barrier.wait();
        assert_eq!(cache.get_with(1, || 5), 5);
    }

    #[test]
    // https://github.com/moka-rs/moka/issues/43
    fn handle_panic_in_try_get_with() {
        use std::{sync::Barrier, thread};

        let cache = Cache::new(16);
        let barrier = Arc::new(Barrier::new(2));
        {
            let cache_ref = cache.clone();
            let barrier_ref = barrier.clone();
            thread::spawn(move || {
                let _ = cache_ref.try_get_with(1, || {
                    barrier_ref.wait();
                    thread::sleep(Duration::from_millis(50));
                    panic!("Panic during try_get_with");
                }) as Result<_, Arc<Infallible>>;
            });
        }

        barrier.wait();
        assert_eq!(
            cache.try_get_with(1, || Ok(5)) as Result<_, Arc<Infallible>>,
            Ok(5)
        );
    }

    #[test]
    fn debug_formats() {
        let mut cache = Cache::builder().max_capacity(10).build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert('a', "alice");
        cache.insert('b', "bob");
        cache.insert('c', "cindy");
        cache.sync();

        assert_eq!(
            format!("{:?}", cache),
            "Cache { max_capacity: Some(10), entry_count: 3, weighted_size: 3 }"
        );

        let debug_str = format!("{:?}", cache.debug_fmt().entries());
        assert!(debug_str.starts_with('{'));
        assert!(debug_str.contains(r#"'a': "alice""#));
        assert!(debug_str.contains(r#"'b': "bob""#));
        assert!(debug_str.contains(r#"'c': "cindy""#));
        assert!(debug_str.ends_with('}'));

        let weigher = |_k: &char, v: &(&str, u32)| v.1;

        let mut cache = Cache::builder().max_capacity(50).weigher(weigher).build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert('a', ("alice", 10));
        cache.insert('b', ("bob", 15));
        cache.insert('c', ("cindy", 5));
        cache.sync();

        assert_eq!(
            format!("{:?}", cache),
            "Cache { max_capacity: Some(50), entry_count: 3, weighted_size: 30 }"
        );

        let debug_str = format!("{:?}", cache.debug_fmt().entries());
        assert!(debug_str.starts_with('{'));
        assert!(debug_str.contains(r#"'a': ("alice", 10)"#));
        assert!(debug_str.contains(r#"'b': ("bob", 15)"#));
        assert!(debug_str.contains(r#"'c': ("cindy", 5)"#));
        assert!(debug_str.ends_with('}'));
    }
}
