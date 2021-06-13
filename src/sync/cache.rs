use super::{
    base_cache::{BaseCache, HouseKeeperArc, MAX_SYNC_REPEATS, WRITE_RETRY_INTERVAL_MICROS},
    housekeeper::InnerSync,
    ConcurrentCacheExt, PredicateId, WriteOp,
};
use crate::PredicateError;

use crossbeam_channel::{Sender, TrySendError};
use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
    sync::Arc,
    time::Duration,
};

/// A thread-safe concurrent in-memory cache.
///
/// `Cache` supports full concurrency of retrievals and a high expected concurrency
/// for updates.
///
/// `Cache` utilizes a lock-free concurrent hash table `cht::SegmentedHashMap` from
/// the [cht][cht-crate] crate for the central key-value storage. `Cache` performs a
/// best-effort bounding of the map using an entry replacement algorithm to determine
/// which entries to evict when the capacity is exceeded.
///
/// [cht-crate]: https://crates.io/crates/cht
///
/// # Examples
///
/// Cache entries are manually added using `insert` method, and are stored in the
/// cache until either evicted or manually invalidated.
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
/// # Thread Safety
///
/// All methods provided by the `Cache` are considered thread-safe, and can be safely
/// accessed by multiple concurrent threads.
///
/// `Cache<K, V, S>` will implement `Send` when all of the following conditions meet:
///
/// - `K` (key) and `V` (value) implement `Send` and `Sync`.
/// - `S` (the hash-map state) implements `Send`.
///
/// and will implement `Sync` when all of the following conditions meet:
///
/// - `K` (key) and `V` (value) implement `Send` and `Sync`.
/// - `S` (the hash-map state) implements `Sync`.
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
/// # Expiration Policies
///
/// `Cache` supports the following expiration policies:
///
/// - **Time to live**: A cached entry will be expired after the specified duration
///   past from `insert`.
/// - **Time to idle**: A cached entry will be expired after the specified duration
///   past from `get` or `insert`.
///
/// See the [`CacheBuilder`][builder-struct]'s doc for how to configure a cache
/// with them.
///
/// [builder-struct]: ./struct.CacheBuilder.html
///
/// # Hashing Algorithm
///
/// By default, `Cache` uses a hashing algorithm selected to provide resistance
/// against HashDoS attacks.
///
/// The default hashing algorithm is the one used by `std::collections::HashMap`,
/// which is currently SipHash 1-3.
///
/// While its performance is very competitive for medium sized keys, other hashing
/// algorithms will outperform it for small keys such as integers as well as large
/// keys such as long strings. However those algorithms will typically not protect
/// against attacks such as HashDoS.
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
}

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
    /// Constructs a new `Cache<K, V>` that will store up to the `max_capacity` entries.
    ///
    /// To adjust various configuration knobs such as `initial_capacity` or
    /// `time_to_live`, use the [`CacheBuilder`][builder-struct].
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    pub fn new(max_capacity: usize) -> Self {
        let build_hasher = RandomState::default();
        Self::with_everything(max_capacity, None, build_hasher, None, None, false)
    }
}

impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn with_everything(
        max_capacity: usize,
        initial_capacity: Option<usize>,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
        invalidator_enabled: bool,
    ) -> Self {
        Self {
            base: BaseCache::new(
                max_capacity,
                initial_capacity,
                build_hasher,
                time_to_live,
                time_to_idle,
                invalidator_enabled,
            ),
        }
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

    /// Inserts a key-value pair into the cache.
    ///
    /// If the cache has this key present, the value is updated.
    pub fn insert(&self, key: K, value: V) {
        let hash = self.base.hash(&key);
        self.insert_with_hash(key, hash, value)
    }

    pub(crate) fn insert_with_hash(&self, key: K, hash: u64, value: V) {
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
        if let Some(entry) = self.base.remove(key) {
            let op = WriteOp::Remove(entry);
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
    /// each value inserted before the time when `invalidate_entries_if` was
    /// called. If the closure returns `true` on a value, that value will be evicted
    /// from the cache.
    ///
    /// Also the `get` method will apply the closure to a value to determine if it
    /// has been invalidated. Therefore, it is guaranteed that the `get` method must
    /// not return invalidated values.
    ///
    /// Note that you must call
    /// [`CacheBuilder::enable_invalidation_with_closures`][enable-invalidation-closures]
    /// at the cache creation time as the cache will maintain additional internal
    /// data structures to support this method. Otherwise, calling this method will
    /// fail with a
    /// [`PredicateError::InvalidationClosuresDisabled`][invalidation-disabled-error].
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    ///
    /// [enable-invalidation-closures]: ./struct.CacheBuilder.html#method.enable_invalidation_with_closures
    /// [invalidation-disabled-error]: ../enum.PredicateError.html#variant.InvalidationClosuresDisabled
    pub fn invalidate_entries_if<F>(&self, predicate: F) -> Result<PredicateId, PredicateError>
    where
        F: Fn(&K, &V) -> bool + Send + Sync + 'static,
    {
        self.base.invalidate_entries_if(Arc::new(predicate))
    }

    /// Returns the `max_capacity` of this cache.
    pub fn max_capacity(&self) -> usize {
        self.base.max_capacity()
    }

    /// Returns the `time_to_live` of this cache.
    pub fn time_to_live(&self) -> Option<Duration> {
        self.base.time_to_live()
    }

    /// Returns the `time_to_idle` of this cache.
    pub fn time_to_idle(&self) -> Option<Duration> {
        self.base.time_to_idle()
    }

    /// Returns the number of internal segments of this cache.
    ///
    /// `Cache` always returns `1`.
    pub fn num_segments(&self) -> usize {
        1
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
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn reconfigure_for_testing(&mut self) {
        self.base.reconfigure_for_testing();
    }

    fn set_expiration_clock(&self, clock: Option<quanta::Clock>) {
        self.base.set_expiration_clock(clock);
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{Cache, ConcurrentCacheExt};
    use crate::sync::CacheBuilder;

    use quanta::Clock;
    use std::time::Duration;

    #[test]
    fn basic_single_thread() {
        let mut cache = Cache::new(3);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"c"), Some("cindy"));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david"); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1

        cache.insert("d", "david");
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        cache.insert("d", "dennis");
        cache.sync();
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some("dennis"));

        cache.invalidate(&"b");
        assert_eq!(cache.get(&"b"), None);
    }

    #[test]
    fn basic_multi_threads() {
        let num_threads = 4;
        let cache = Cache::new(100);

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
        cache.sync();

        cache.invalidate_all();
        cache.sync();

        cache.insert("d", "david");
        cache.sync();

        assert!(cache.get(&"a").is_none());
        assert!(cache.get(&"b").is_none());
        assert!(cache.get(&"c").is_none());
        assert_eq!(cache.get(&"d"), Some("david"));
    }

    #[test]
    fn invalidate_entries_if() -> Result<(), Box<dyn std::error::Error>> {
        use std::collections::HashSet;

        let mut cache = CacheBuilder::new(100)
            .enable_invalidation_with_closures()
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
        assert_eq!(cache.base.len(), 2);
        assert_eq!(cache.base.invalidation_predicate_count(), 0);

        mock.increment(Duration::from_secs(5)); // 15 secs from the start.

        cache.invalidate_entries_if(|_k, &v| v == "alice")?;
        cache.invalidate_entries_if(|_k, &v| v == "bob")?;
        assert_eq!(cache.base.invalidation_predicate_count(), 2);

        // Run the invalidation task and wait for it to finish. (TODO: Need a better way than sleeping)
        cache.sync(); // To submit the invalidation task.
        std::thread::sleep(Duration::from_millis(200));
        cache.sync(); // To process the task result.
        std::thread::sleep(Duration::from_millis(200));

        assert!(cache.get(&1).is_none());
        assert!(cache.get(&3).is_none());
        assert_eq!(cache.base.len(), 0);
        assert_eq!(cache.base.invalidation_predicate_count(), 0);

        Ok(())
    }

    #[test]
    fn time_to_live() {
        let mut cache = CacheBuilder::new(100)
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

        cache.get(&"a");

        mock.increment(Duration::from_secs(5)); // 10 secs.
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert!(cache.base.is_empty());

        cache.insert("b", "bob");
        cache.sync();

        assert_eq!(cache.base.len(), 1);

        mock.increment(Duration::from_secs(5)); // 15 secs.
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.base.len(), 1);

        cache.insert("b", "bill");
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 20 secs
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bill"));
        assert_eq!(cache.base.len(), 1);

        mock.increment(Duration::from_secs(5)); // 25 secs
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(cache.base.is_empty());
    }

    #[test]
    fn time_to_idle() {
        let mut cache = CacheBuilder::new(100)
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

        assert_eq!(cache.base.len(), 2);

        mock.increment(Duration::from_secs(5)); // 15 secs.
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.base.len(), 1);

        mock.increment(Duration::from_secs(10)); // 25 secs
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(cache.base.is_empty());
    }
}
