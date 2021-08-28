use super::{deques::Deques, KeyDate, KeyHashDate, ValueEntry};
use crate::common::{
    deque::{CacheRegion, DeqNode, Deque},
    frequency_sketch::FrequencySketch,
    AccessTime,
};

use quanta::{Clock, Instant};
use std::{
    borrow::Borrow,
    collections::{hash_map::RandomState, HashMap},
    convert::TryInto,
    hash::{BuildHasher, Hash, Hasher},
    ptr::NonNull,
    rc::Rc,
    time::Duration,
};

type CacheStore<K, V, S> = std::collections::HashMap<Rc<K>, ValueEntry<K, V>, S>;

/// An in-memory cache that is _not_ thread-safe.
///
/// `Cache` utilizes a hash table `std::collections::HashMap` from the standard
/// library for the central key-value storage. `Cache` performs a best-effort
/// bounding of the map using an entry replacement algorithm to determine which
/// entries to evict when the capacity is exceeded.
///
/// # Characteristic difference between `unsync` and `sync`/`future` caches
///
/// If you use a cache from a single thread application, `unsync::Cache` may
/// outperform other caches for updates and retrievals because other caches have some
/// overhead on syncing internal data structures between threads.
///
/// However, other caches may outperform `unsync::Cache` on the same operations when
/// expiration polices are configured on a multi-core system. `unsync::Cache` evicts
/// expired entries as a part of update and retrieval operations while others evict
/// them using a dedicated background thread.
///
/// # Examples
///
/// Cache entries are manually added using an insert method, and are stored in the
/// cache until either evicted or manually invalidated.
///
/// Here's an example of reading and updating a cache by using the main thread:
///
///```rust
/// use moka::unsync::Cache;
///
/// const NUM_KEYS: usize = 64;
///
/// fn value(n: usize) -> String {
///     format!("value {}", n)
/// }
///
/// // Create a cache that can store up to 10,000 entries.
/// let mut cache = Cache::new(10_000);
///
/// // Insert 64 entries.
/// for key in 0..NUM_KEYS {
///     cache.insert(key, value(key));
/// }
///
/// // Invalidate every 4 element of the inserted entries.
/// for key in (0..NUM_KEYS).step_by(4) {
///     cache.invalidate(&key);
/// }
///
/// // Verify the result.
/// for key in 0..NUM_KEYS {
///     if key % 4 == 0 {
///         assert_eq!(cache.get(&key), None);
///     } else {
///         assert_eq!(cache.get(&key), Some(&value(key)));
///     }
/// }
/// ```
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
/// against HashDoS attacks. It will the same one used by
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
    max_capacity: usize,
    cache: CacheStore<K, V, S>,
    build_hasher: S,
    deques: Deques<K>,
    frequency_sketch: FrequencySketch,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    expiration_clock: Option<Clock>,
}

impl<K, V> Cache<K, V, RandomState>
where
    K: Hash + Eq,
{
    /// Constructs a new `Cache<K, V>` that will store up to the `max_capacity` entries.
    ///
    /// To adjust various configuration knobs such as `initial_capacity` or
    /// `time_to_live`, use the [`CacheBuilder`][builder-struct].
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    pub fn new(max_capacity: usize) -> Self {
        let build_hasher = RandomState::default();
        Self::with_everything(max_capacity, None, build_hasher, None, None)
    }
}

//
// public
//
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    pub(crate) fn with_everything(
        max_capacity: usize,
        initial_capacity: Option<usize>,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let cache = HashMap::with_capacity_and_hasher(
            initial_capacity.unwrap_or_default(),
            build_hasher.clone(),
        );

        // Ensure skt_capacity fits in a range of `128u32..=u32::MAX`.
        let skt_capacity = max_capacity
            .try_into() // Convert to u32.
            .unwrap_or(u32::MAX)
            .max(128);
        let frequency_sketch = FrequencySketch::with_capacity(skt_capacity);
        Self {
            max_capacity,
            cache,
            build_hasher,
            deques: Deques::default(),
            frequency_sketch,
            time_to_live,
            time_to_idle,
            expiration_clock: None,
        }
    }

    /// Returns an immutable reference of the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    ///
    /// [rustdoc-std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        Rc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let timestamp = self.evict_if_needed();
        self.frequency_sketch.increment(self.hash(key));

        match (self.cache.get_mut(key), timestamp, &mut self.deques) {
            // Value not found.
            (None, _, _) => None,
            // Value found, no expiry.
            (Some(entry), None, deqs) => {
                Self::record_hit(deqs, entry, None);
                Some(&entry.value)
            }
            // Value found, check if expired.
            (Some(entry), Some(ts), deqs) => {
                if Self::is_expired_entry_wo(&self.time_to_live, entry, ts)
                    || Self::is_expired_entry_ao(&self.time_to_idle, entry, ts)
                {
                    None
                } else {
                    Self::record_hit(deqs, entry, timestamp);
                    Some(&entry.value)
                }
            }
        }
    }

    /// Inserts a key-value pair into the cache.
    ///
    /// If the cache has this key present, the value is updated.
    pub fn insert(&mut self, key: K, value: V) {
        let timestamp = self.evict_if_needed();
        let key = Rc::new(key);
        let entry = ValueEntry::new(value);

        if let Some(old_entry) = self.cache.insert(Rc::clone(&key), entry) {
            self.handle_update(key, timestamp, old_entry);
        } else {
            let hash = self.hash(&key);
            self.handle_insert(key, hash, timestamp);
        }
    }

    /// Discards any cached value for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub fn invalidate<Q>(&mut self, key: &Q)
    where
        Rc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.evict_if_needed();

        if let Some(mut entry) = self.cache.remove(key) {
            self.deques.unlink_ao(&mut entry);
            Deques::unlink_wo(&mut self.deques.write_order, &mut entry)
        }
    }

    /// Discards all cached values.
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    pub fn invalidate_all(&mut self) {
        self.cache.clear();
        self.deques.clear();
    }

    /// Discards cached values that satisfy a predicate.
    ///
    /// `invalidate_entries_if` takes a closure that returns `true` or `false`.
    /// `invalidate_entries_if` will apply the closure to each cached value,
    /// and if the closure returns `true`, the value will be invalidated.
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.

    // We need this #[allow(...)] to avoid a false Clippy warning about needless
    // collect to create keys_to_invalidate.
    // clippy 0.1.52 (9a1dfd2dc5c 2021-04-30) in Rust 1.52.0-beta.7
    #[allow(clippy::needless_collect)]
    pub fn invalidate_entries_if(&mut self, mut predicate: impl FnMut(&K, &V) -> bool) {
        let Self { cache, deques, .. } = self;

        // Since we can't do cache.iter() and cache.remove() at the same time,
        // invalidation needs to run in two steps:
        // 1. Examine all entries in this cache and collect keys to invalidate.
        // 2. Remove entries for the keys.

        let keys_to_invalidate = cache
            .iter()
            .filter(|(key, entry)| (predicate)(key, &entry.value))
            .map(|(key, _)| Rc::clone(key))
            .collect::<Vec<_>>();

        keys_to_invalidate.into_iter().for_each(|k| {
            if let Some(mut entry) = cache.remove(&k) {
                deques.unlink_ao(&mut entry);
                Deques::unlink_wo(&mut deques.write_order, &mut entry);
            }
        });
    }

    /// Returns the `max_capacity` of this cache.
    pub fn max_capacity(&self) -> usize {
        self.max_capacity
    }

    /// Returns the `time_to_live` of this cache.
    pub fn time_to_live(&self) -> Option<Duration> {
        self.time_to_live
    }

    /// Returns the `time_to_idle` of this cache.
    pub fn time_to_idle(&self) -> Option<Duration> {
        self.time_to_idle
    }
}

//
// private
//
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    #[inline]
    fn hash<Q>(&self, key: &Q) -> u64
    where
        Rc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut hasher = self.build_hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[inline]
    fn has_expiry(&self) -> bool {
        self.time_to_live.is_some() || self.time_to_idle.is_some()
    }

    #[inline]
    fn evict_if_needed(&mut self) -> Option<Instant> {
        if self.has_expiry() {
            let ts = self.current_time_from_expiration_clock();
            self.evict(ts);
            Some(ts)
        } else {
            None
        }
    }

    #[inline]
    fn current_time_from_expiration_clock(&self) -> Instant {
        if let Some(clock) = &self.expiration_clock {
            clock.now()
        } else {
            Instant::now()
        }
    }

    #[inline]
    fn is_expired_entry_ao(
        time_to_idle: &Option<Duration>,
        entry: &impl AccessTime,
        now: Instant,
    ) -> bool {
        if let (Some(ts), Some(tti)) = (entry.last_accessed(), time_to_idle) {
            return ts + *tti <= now;
        }
        false
    }

    #[inline]
    fn is_expired_entry_wo(
        time_to_live: &Option<Duration>,
        entry: &impl AccessTime,
        now: Instant,
    ) -> bool {
        if let (Some(ts), Some(ttl)) = (entry.last_modified(), time_to_live) {
            return ts + *ttl <= now;
        }
        false
    }

    fn record_hit(deques: &mut Deques<K>, entry: &mut ValueEntry<K, V>, ts: Option<Instant>) {
        if let Some(ts) = ts {
            entry.set_last_accessed(ts);
        }
        deques.move_to_back_ao(entry)
    }

    #[inline]
    fn handle_insert(&mut self, key: Rc<K>, hash: u64, timestamp: Option<Instant>) {
        let has_free_space = self.cache.len() <= self.max_capacity;
        let (cache, deqs, freq) = (&mut self.cache, &mut self.deques, &self.frequency_sketch);

        if has_free_space {
            // Add the candidate to the deque.
            let key = Rc::clone(&key);
            let mut entry = cache.get_mut(&key).unwrap();
            deqs.push_back_ao(
                CacheRegion::MainProbation,
                KeyHashDate::new(Rc::clone(&key), hash, timestamp),
                &mut entry,
            );
            if self.time_to_live.is_some() {
                deqs.push_back_wo(KeyDate::new(key, timestamp), &mut entry);
            }
        } else {
            let victim = Self::find_cache_victim(deqs, freq);
            if Self::admit(hash, victim, freq) {
                // Remove the victim from the cache and deque.
                //
                // TODO: Check if the selected victim was actually removed. If not,
                // maybe we should find another victim. This can happen because it
                // could have been already removed from the cache but the removal
                // from the deque is still on the write operations queue and is not
                // yet executed.
                if let Some(mut vic_entry) = cache.remove(&victim.element.key) {
                    deqs.unlink_ao(&mut vic_entry);
                    Deques::unlink_wo(&mut deqs.write_order, &mut vic_entry);
                } else {
                    let victim = NonNull::from(victim);
                    deqs.unlink_node_ao(victim);
                }
                // Add the candidate to the deque.
                let mut entry = cache.get_mut(&key).unwrap();

                let key = Rc::clone(&key);
                deqs.push_back_ao(
                    CacheRegion::MainProbation,
                    KeyHashDate::new(Rc::clone(&key), hash, timestamp),
                    &mut entry,
                );
                if self.time_to_live.is_some() {
                    deqs.push_back_wo(KeyDate::new(key, timestamp), &mut entry);
                }
            } else {
                // Remove the candidate from the cache.
                cache.remove(&key);
            }
        }
    }

    #[inline]
    fn find_cache_victim<'a>(
        deqs: &'a mut Deques<K>,
        _freq: &FrequencySketch,
    ) -> &'a DeqNode<KeyHashDate<K>> {
        // TODO: Check its frequency. If it is not very low, maybe we should
        // check frequencies of next few others and pick from them.
        deqs.probation.peek_front().expect("No victim found")
    }

    #[inline]
    fn admit(
        candidate_hash: u64,
        victim: &DeqNode<KeyHashDate<K>>,
        freq: &FrequencySketch,
    ) -> bool {
        // TODO: Implement some randomness to mitigate hash DoS attack.
        // See Caffeine's implementation.
        freq.frequency(candidate_hash) > freq.frequency(victim.element.hash)
    }

    fn handle_update(
        &mut self,
        key: Rc<K>,
        timestamp: Option<Instant>,
        old_entry: ValueEntry<K, V>,
    ) {
        let entry = self.cache.get_mut(&key).unwrap();
        entry.replace_deq_nodes_with(old_entry);
        if let Some(ts) = timestamp {
            entry.set_last_accessed(ts);
            entry.set_last_modified(ts);
        }
        let deqs = &mut self.deques;
        deqs.move_to_back_ao(entry);
        deqs.move_to_back_wo(entry)
    }

    fn evict(&mut self, now: Instant) {
        const EVICTION_BATCH_SIZE: usize = 100;

        if self.time_to_live.is_some() {
            self.remove_expired_wo(EVICTION_BATCH_SIZE, now);
        }

        if self.time_to_idle.is_some() {
            let deqs = &mut self.deques;
            let (window, probation, protected, wo, cache, time_to_idle) = (
                &mut deqs.window,
                &mut deqs.probation,
                &mut deqs.protected,
                &mut deqs.write_order,
                &mut self.cache,
                &self.time_to_idle,
            );

            let mut rm_expired_ao = |name, deq| {
                Self::remove_expired_ao(
                    name,
                    deq,
                    wo,
                    cache,
                    time_to_idle,
                    EVICTION_BATCH_SIZE,
                    now,
                )
            };

            rm_expired_ao("window", window);
            rm_expired_ao("probation", probation);
            rm_expired_ao("protected", protected);
        }
    }

    #[inline]
    fn remove_expired_ao(
        deq_name: &str,
        deq: &mut Deque<KeyHashDate<K>>,
        write_order_deq: &mut Deque<KeyDate<K>>,
        cache: &mut CacheStore<K, V, S>,
        time_to_idle: &Option<Duration>,
        batch_size: usize,
        now: Instant,
    ) {
        for _ in 0..batch_size {
            let key = deq
                .peek_front()
                .and_then(|node| {
                    if Self::is_expired_entry_ao(time_to_idle, &*node, now) {
                        Some(Some(Rc::clone(&node.element.key)))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            if key.is_none() {
                break;
            }

            if let Some(mut entry) = cache.remove(&key.unwrap()) {
                Deques::unlink_ao_from_deque(deq_name, deq, &mut entry);
                Deques::unlink_wo(write_order_deq, &mut entry);
            } else {
                deq.pop_front();
            }
        }
    }

    #[inline]
    fn remove_expired_wo(&mut self, batch_size: usize, now: Instant) {
        let time_to_live = &self.time_to_live;
        for _ in 0..batch_size {
            let key = self
                .deques
                .write_order
                .peek_front()
                .and_then(|node| {
                    if Self::is_expired_entry_wo(time_to_live, &*node, now) {
                        Some(Some(Rc::clone(&node.element.key)))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            if key.is_none() {
                break;
            }

            if let Some(mut entry) = self.cache.remove(&key.unwrap()) {
                self.deques.unlink_ao(&mut entry);
                Deques::unlink_wo(&mut self.deques.write_order, &mut entry);
            } else {
                self.deques.write_order.pop_front();
            }
        }
    }
}

//
// for testing
//
#[cfg(test)]
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    fn set_expiration_clock(&mut self, clock: Option<quanta::Clock>) {
        self.expiration_clock = clock;
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::Cache;
    use crate::unsync::CacheBuilder;

    use quanta::Clock;
    use std::time::Duration;

    #[test]
    fn basic_single_thread() {
        let mut cache = Cache::new(3);

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some(&"alice"));
        assert_eq!(cache.get(&"b"), Some(&"bob"));
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"c"), Some(&"cindy"));
        // counts: a -> 1, b -> 1, c -> 1

        assert_eq!(cache.get(&"a"), Some(&"alice"));
        assert_eq!(cache.get(&"b"), Some(&"bob"));
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david"); //   count: d -> 0
        assert_eq!(cache.get(&"d"), None); //   d -> 1

        cache.insert("d", "david");
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        cache.insert("d", "dennis");
        assert_eq!(cache.get(&"a"), Some(&"alice"));
        assert_eq!(cache.get(&"b"), Some(&"bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(&"dennis"));

        cache.invalidate(&"b");
        assert_eq!(cache.get(&"b"), None);
    }

    #[test]
    fn invalidate_all() {
        let mut cache = Cache::new(100);

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"a"), Some(&"alice"));
        assert_eq!(cache.get(&"b"), Some(&"bob"));
        assert_eq!(cache.get(&"c"), Some(&"cindy"));

        cache.invalidate_all();

        cache.insert("d", "david");

        assert!(cache.get(&"a").is_none());
        assert!(cache.get(&"b").is_none());
        assert!(cache.get(&"c").is_none());
        assert_eq!(cache.get(&"d"), Some(&"david"));
    }

    #[test]
    fn invalidate_entries_if() {
        use std::collections::HashSet;

        let mut cache = Cache::new(100);

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        cache.insert(0, "alice");
        cache.insert(1, "bob");
        cache.insert(2, "alex");

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.

        assert_eq!(cache.get(&0), Some(&"alice"));
        assert_eq!(cache.get(&1), Some(&"bob"));
        assert_eq!(cache.get(&2), Some(&"alex"));

        let names = ["alice", "alex"].iter().cloned().collect::<HashSet<_>>();
        cache.invalidate_entries_if(move |_k, &v| names.contains(v));

        mock.increment(Duration::from_secs(5)); // 10 secs from the start.

        cache.insert(3, "alice");

        assert!(cache.get(&0).is_none());
        assert!(cache.get(&2).is_none());
        assert_eq!(cache.get(&1), Some(&"bob"));
        // This should survive as it was inserted after calling invalidate_entries_if.
        assert_eq!(cache.get(&3), Some(&"alice"));
        assert_eq!(cache.cache.len(), 2);

        mock.increment(Duration::from_secs(5)); // 15 secs from the start.

        cache.invalidate_entries_if(|_k, &v| v == "alice");
        cache.invalidate_entries_if(|_k, &v| v == "bob");

        assert!(cache.get(&1).is_none());
        assert!(cache.get(&3).is_none());
        assert_eq!(cache.cache.len(), 0);
    }

    #[test]
    fn time_to_live() {
        let mut cache = CacheBuilder::new(100)
            .time_to_live(Duration::from_secs(10))
            .build();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        cache.insert("a", "alice");

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.

        cache.get(&"a");

        mock.increment(Duration::from_secs(5)); // 10 secs.

        assert_eq!(cache.get(&"a"), None);
        assert!(cache.cache.is_empty());

        cache.insert("b", "bob");

        assert_eq!(cache.cache.len(), 1);

        mock.increment(Duration::from_secs(5)); // 15 secs.

        assert_eq!(cache.get(&"b"), Some(&"bob"));
        assert_eq!(cache.cache.len(), 1);

        cache.insert("b", "bill");

        mock.increment(Duration::from_secs(5)); // 20 secs

        assert_eq!(cache.get(&"b"), Some(&"bill"));
        assert_eq!(cache.cache.len(), 1);

        mock.increment(Duration::from_secs(5)); // 25 secs

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(cache.cache.is_empty());
    }

    #[test]
    fn time_to_idle() {
        let mut cache = CacheBuilder::new(100)
            .time_to_idle(Duration::from_secs(10))
            .build();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        cache.insert("a", "alice");

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.

        assert_eq!(cache.get(&"a"), Some(&"alice"));

        mock.increment(Duration::from_secs(5)); // 10 secs.

        cache.insert("b", "bob");

        assert_eq!(cache.cache.len(), 2);

        mock.increment(Duration::from_secs(5)); // 15 secs.

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some(&"bob"));
        assert_eq!(cache.cache.len(), 1);

        mock.increment(Duration::from_secs(10)); // 25 secs

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(cache.cache.is_empty());
    }
}
