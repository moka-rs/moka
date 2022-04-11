use super::{
    base_cache::{BaseCache, HouseKeeperArc, MAX_SYNC_REPEATS, WRITE_RETRY_INTERVAL_MICROS},
    CacheBuilder, ConcurrentCacheExt, EntryRef, Iter,
};
use crate::{
    sync::{housekeeper::InnerSync, Weigher, WriteOp},
    Policy,
};

use crossbeam_channel::{Sender, TrySendError};
use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    fmt,
    hash::{BuildHasher, Hash},
    sync::Arc,
    time::Duration,
};

/// **Experimental**: A thread-safe concurrent in-memory cache built upon
/// [`dashmap::DashMap`][dashmap].
///
/// The `dash::Cache` uses `DashMap` as the central key-value storage, while other
/// `sync` and `future` caches are using a lock-free concurrent hash table.
/// Since `DashMap` employs read-write locks on internal shards, it will have lower
/// concurrency on retrievals and updates than other caches.
///
/// `dash` cache performs a best-effort bounding of the map using an entry
/// replacement algorithm to determine which entries to evict when the capacity is
/// exceeded.
///
/// To use this cache, enable a crate feature called "dash" in your Cargo.toml.
/// Please note that the API of `dash` cache will _be changed very often_ in next few
/// releases as this is yet an experimental component.
///
/// # Examples
///
/// Cache entries are manually added using [`insert`](#method.insert) method, and are
/// stored in the cache until either evicted or manually invalidated.
///
/// Here's an example of reading and updating a cache by using multiple threads:
///
/// ```rust
/// use moka::dash::Cache;
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
/// use moka::dash::Cache;
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
/// use moka::dash::Cache;
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
#[derive(Clone)]
pub struct Cache<K, V, S = RandomState> {
    base: BaseCache<K, V, S>,
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

impl<K, V, S> fmt::Debug for Cache<K, V, S>
where
    K: Eq + Hash + fmt::Debug,
    V: fmt::Debug,
    S: BuildHasher + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d_map = f.debug_map();

        for r in self.iter() {
            let (k, v) = r.pair();
            d_map.entry(k, v);
        }

        d_map.finish()
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
        Self::with_everything(Some(max_capacity), None, build_hasher, None, None, None)
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
    ) -> Self {
        Self {
            base: BaseCache::new(
                max_capacity,
                initial_capacity,
                build_hasher,
                weigher,
                time_to_live,
                time_to_idle,
            ),
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
        self.base.contains_key(key)
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

    /// Deprecated, replaced with [`get`](#method.get)
    #[doc(hidden)]
    #[deprecated(since = "0.8.0", note = "Replaced with `get`")]
    pub fn get_if_present<Q>(&self, key: &Q) -> Option<V>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get(key)
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
        if let Some(kv) = self.base.remove_entry(key) {
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

    /// Returns a read-only cache policy of this cache.
    ///
    /// At this time, cache policy cannot be modified after cache creation.
    /// A future version may support to modify it.
    pub fn policy(&self) -> Policy {
        self.base.policy()
    }

    #[cfg(test)]
    pub(crate) fn estimated_entry_count(&self) -> u64 {
        self.base.estimated_entry_count()
    }

    #[cfg(test)]
    pub(crate) fn weighted_size(&self) -> u64 {
        self.base.weighted_size()
    }
}

impl<'a, K, V, S> Cache<K, V, S>
where
    K: 'a + Eq + Hash,
    V: 'a,
    S: BuildHasher + Clone,
{
    /// Creates an iterator visiting all key-value pairs in arbitrary order. The
    /// iterator element type is [`EntryRef<'a, K, V, S>`][moka-entry-ref].
    ///
    /// Unlike the `get` method, visiting entries via an iterator do not update the
    /// historic popularity estimator or reset idle timers for keys.
    ///
    /// # Locking behavior
    ///
    /// This iterator relies on the iterator of [`dashmap::DashMap`][dashmap-iter],
    /// which employs read-write locks. May deadlock if the thread holding an
    /// iterator attempts to update the cache.
    ///
    /// [moka-entry-ref]: ./struct.EntryRef.html
    /// [dashmap-iter]: <https://docs.rs/dashmap/*/dashmap/struct.DashMap.html#method.iter>
    ///
    /// # Examples
    ///
    /// ```rust
    /// use moka::dash::Cache;
    ///
    /// let cache = Cache::new(100);
    /// cache.insert("Julia", 14);
    ///
    /// let mut iter = cache.iter();
    /// let entry_ref = iter.next().unwrap();
    /// assert_eq!(entry_ref.pair(), (&"Julia", &14));
    /// assert_eq!(entry_ref.key(), &"Julia");
    /// assert_eq!(entry_ref.value(), &14);
    /// assert_eq!(*entry_ref, 14);
    ///
    /// assert!(iter.next().is_none());
    /// ```
    ///
    pub fn iter(&self) -> Iter<'_, K, V, S> {
        self.base.iter()
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

impl<'a, K, V, S> IntoIterator for &'a Cache<K, V, S>
where
    K: 'a + Eq + Hash,
    V: 'a,
    S: BuildHasher + Clone,
{
    type Item = EntryRef<'a, K, V, S>;

    type IntoIter = Iter<'a, K, V, S>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
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
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn is_table_empty(&self) -> bool {
        self.estimated_entry_count() == 0
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

    use std::{sync::Arc, time::Duration};

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
        assert_eq!(cache.estimated_entry_count(), 2);
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

        assert_eq!(cache.estimated_entry_count(), 1);

        mock.increment(Duration::from_secs(5)); // 15 secs.
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.estimated_entry_count(), 1);

        cache.insert("b", "bill");
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 20 secs
        cache.sync();

        assert_eq!(cache.get(&"b"), Some("bill"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.estimated_entry_count(), 1);

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

        assert_eq!(cache.estimated_entry_count(), 2);

        mock.increment(Duration::from_secs(2)); // 12 secs.
        cache.sync();

        // contains_key does not reset the idle timer for the key.
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        cache.sync();

        assert_eq!(cache.estimated_entry_count(), 2);

        mock.increment(Duration::from_secs(3)); // 15 secs.
        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(!cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));

        assert_eq!(cache.iter().count(), 1);

        cache.sync();
        assert_eq!(cache.estimated_entry_count(), 1);

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

        for entry in &cache {
            let (key, value) = entry.pair();
            assert_eq!(value, &make_value(*key));

            key_set.insert(*key);
        }

        // Ensure there are no missing or duplicate keys in the iteration.
        assert_eq!(key_set.len(), NUM_KEYS);

        // DO NOT REMOVE THE COMMENT FROM THIS BLOCK.
        // This block demonstrates how you can write a code to get a deadlock.
        // {
        //     let mut iter = cache.iter();
        //     let _ = iter.next();

        //     for key in 0..NUM_KEYS {
        //         cache.insert(key, make_value(key));
        //         println!("{}", key);
        //     }

        //     let _ = iter.next();
        // }
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
                        for entry in &cache {
                            let (key, value) = entry.pair();
                            assert_eq!(value, &make_value(*key));
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
        let key_set = cache.iter().map(|ent| *ent.key()).collect::<HashSet<_>>();
        assert_eq!(key_set.len(), NUM_KEYS);
    }

    #[test]
    fn test_debug_format() {
        let cache = Cache::new(10);
        cache.insert('a', "alice");
        cache.insert('b', "bob");
        cache.insert('c', "cindy");

        let debug_str = format!("{:?}", cache);
        assert!(debug_str.starts_with('{'));
        assert!(debug_str.contains(r#"'a': "alice""#));
        assert!(debug_str.contains(r#"'b': "bob""#));
        assert!(debug_str.contains(r#"'c': "cindy""#));
        assert!(debug_str.ends_with('}'));
    }
}
