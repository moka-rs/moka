use super::{deques::Deques, AccessTime, CacheBuilder, KeyDate, KeyHashDate, ValueEntry, Weigher};
use crate::common::{
    self,
    deque::{DeqNode, Deque},
    frequency_sketch::FrequencySketch,
    time::{CheckedTimeOps, Clock, Instant},
    CacheRegion,
};

use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    collections::{hash_map::RandomState, HashMap},
    hash::{BuildHasher, Hash, Hasher},
    ptr::NonNull,
    rc::Rc,
    time::Duration,
};

const EVICTION_BATCH_SIZE: usize = 100;

type CacheStore<K, V, S> = std::collections::HashMap<Rc<K>, ValueEntry<K, V>, S>;

/// An in-memory cache that is _not_ thread-safe.
///
/// `Cache` utilizes a hash table [`std::collections::HashMap`][std-hashmap] from the
/// standard library for the central key-value storage. `Cache` performs a
/// best-effort bounding of the map using an entry replacement algorithm to determine
/// which entries to evict when the capacity is exceeded.
///
/// [std-hashmap]: https://doc.rust-lang.org/std/collections/struct.HashMap.html
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
/// Cache entries are manually added using the insert method, and are stored in the
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
/// # Size-based Eviction
///
/// ```rust
/// use std::convert::TryInto;
/// use moka::unsync::Cache;
///
/// // Evict based on the number of entries in the cache.
/// let mut cache = Cache::builder()
///     // Up to 10,000 entries.
///     .max_capacity(10_000)
///     // Create the cache.
///     .build();
/// cache.insert(1, "one".to_string());
///
/// // Evict based on the byte length of strings in the cache.
/// let mut cache = Cache::builder()
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
    max_capacity: Option<u64>,
    entry_count: u64,
    weighted_size: u64,
    cache: CacheStore<K, V, S>,
    build_hasher: S,
    weigher: Option<Weigher<K, V>>,
    deques: Deques<K>,
    frequency_sketch: FrequencySketch,
    frequency_sketch_enabled: bool,
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

//
// public
//
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    pub(crate) fn with_everything(
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let cache = HashMap::with_capacity_and_hasher(
            initial_capacity.unwrap_or_default(),
            build_hasher.clone(),
        );

        Self {
            max_capacity: max_capacity.map(|n| n as u64),
            entry_count: 0,
            weighted_size: 0,
            cache,
            build_hasher,
            weigher,
            deques: Default::default(),
            frequency_sketch: Default::default(),
            frequency_sketch_enabled: false,
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
        let timestamp = self.evict_expired_if_needed();
        self.evict_lru_entries();
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
        let timestamp = self.evict_expired_if_needed();
        self.evict_lru_entries();
        let policy_weight = weigh(&mut self.weigher, &key, &value);
        let key = Rc::new(key);
        let entry = ValueEntry::new(value, policy_weight);

        if let Some(old_entry) = self.cache.insert(Rc::clone(&key), entry) {
            self.handle_update(key, timestamp, policy_weight, old_entry);
        } else {
            let hash = self.hash(&key);
            self.handle_insert(key, hash, policy_weight, timestamp);
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
        self.evict_expired_if_needed();
        self.evict_lru_entries();

        if let Some(mut entry) = self.cache.remove(key) {
            let weight = entry.policy_weight();
            self.deques.unlink_ao(&mut entry);
            Deques::unlink_wo(&mut self.deques.write_order, &mut entry);
            self.saturating_sub_from_total_weight(weight as u64);
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
        self.weighted_size = 0;
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

        let mut invalidated = 0u64;

        keys_to_invalidate.into_iter().for_each(|k| {
            if let Some(mut entry) = cache.remove(&k) {
                let weight = entry.policy_weight();
                deques.unlink_ao(&mut entry);
                Deques::unlink_wo(&mut deques.write_order, &mut entry);
                invalidated = invalidated.saturating_sub(weight as u64);
            }
        });
        self.saturating_sub_from_total_weight(invalidated);
    }

    /// Returns the `max_capacity` of this cache.
    pub fn max_capacity(&self) -> Option<usize> {
        self.max_capacity.map(|n| n as usize)
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
    fn evict_expired_if_needed(&mut self) -> Option<Instant> {
        if self.has_expiry() {
            let ts = self.current_time_from_expiration_clock();
            self.evict_expired(ts);
            Some(ts)
        } else {
            None
        }
    }

    #[inline]
    fn current_time_from_expiration_clock(&self) -> Instant {
        if let Some(clock) = &self.expiration_clock {
            Instant::new(clock.now())
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
            let checked_add = ts.checked_add(*tti);
            if checked_add.is_none() {
                panic!("ttl overflow")
            }
            return checked_add.unwrap() <= now;
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
            let checked_add = ts.checked_add(*ttl);
            if checked_add.is_none() {
                panic!("ttl overflow")
            }
            return checked_add.unwrap() <= now;
        }
        false
    }

    fn record_hit(deques: &mut Deques<K>, entry: &mut ValueEntry<K, V>, ts: Option<Instant>) {
        if let Some(ts) = ts {
            entry.set_last_accessed(ts);
        }
        deques.move_to_back_ao(entry)
    }

    fn has_enough_capacity(&self, candidate_weight: u32, ws: u64) -> bool {
        self.max_capacity
            .map(|limit| ws + candidate_weight as u64 <= limit)
            .unwrap_or(true)
    }

    fn weights_to_evict(&self) -> u64 {
        self.max_capacity
            .map(|limit| self.weighted_size.saturating_sub(limit))
            .unwrap_or_default()
    }

    #[inline]
    fn should_enable_frequency_sketch(&self) -> bool {
        if self.frequency_sketch_enabled {
            false
        } else if let Some(max_cap) = self.max_capacity {
            self.weighted_size >= max_cap / 2
        } else {
            false
        }
    }

    #[inline]
    fn enable_frequency_sketch(&mut self) {
        if let Some(max_cap) = self.max_capacity {
            let cap = if self.weigher.is_none() {
                max_cap
            } else {
                (self.entry_count as f64 * (self.weighted_size as f64 / max_cap as f64)) as u64
            };
            self.do_enable_frequency_sketch(cap);
        }
    }

    #[cfg(test)]
    fn enable_frequency_sketch_for_testing(&mut self) {
        if let Some(max_cap) = self.max_capacity {
            self.do_enable_frequency_sketch(max_cap);
        }
    }

    #[inline]
    fn do_enable_frequency_sketch(&mut self, cache_capacity: u64) {
        let skt_capacity = common::sketch_capacity(cache_capacity);
        self.frequency_sketch.ensure_capacity(skt_capacity);
        self.frequency_sketch_enabled = true;
    }

    fn saturating_add_to_total_weight(&mut self, weight: u64) {
        let total = &mut self.weighted_size;
        *total = total.saturating_add(weight as u64);
    }

    fn saturating_sub_from_total_weight(&mut self, weight: u64) {
        let total = &mut self.weighted_size;
        *total = total.saturating_sub(weight as u64);
    }

    #[inline]
    fn handle_insert(
        &mut self,
        key: Rc<K>,
        hash: u64,
        policy_weight: u32,
        timestamp: Option<Instant>,
    ) {
        let has_free_space = self.has_enough_capacity(policy_weight, self.weighted_size);
        let (cache, deqs, freq) = (&mut self.cache, &mut self.deques, &self.frequency_sketch);

        if has_free_space {
            // Add the candidate to the deque.
            let key = Rc::clone(&key);
            let entry = cache.get_mut(&key).unwrap();
            deqs.push_back_ao(
                CacheRegion::MainProbation,
                KeyHashDate::new(Rc::clone(&key), hash, timestamp),
                entry,
            );
            if self.time_to_live.is_some() {
                deqs.push_back_wo(KeyDate::new(key, timestamp), entry);
            }
            self.entry_count += 1;
            self.saturating_add_to_total_weight(policy_weight as u64);

            if self.should_enable_frequency_sketch() {
                self.enable_frequency_sketch();
            }

            return;
        }

        if let Some(max) = self.max_capacity {
            if policy_weight as u64 > max {
                // The candidate is too big to fit in the cache. Reject it.
                cache.remove(&Rc::clone(&key));
                return;
            }
        }

        let mut candidate = EntrySizeAndFrequency::new(policy_weight as u64);
        candidate.add_frequency(freq, hash);

        match Self::admit(&candidate, cache, deqs, freq, &mut self.weigher) {
            AdmissionResult::Admitted {
                victim_nodes,
                victims_weight,
            } => {
                // Remove the victims from the cache (hash map) and deque.
                for victim in victim_nodes {
                    // Remove the victim from the hash map.
                    let mut vic_entry = cache
                        .remove(unsafe { &victim.as_ref().element.key })
                        .expect("Cannot remove a victim from the hash map");
                    // And then remove the victim from the deques.
                    deqs.unlink_ao(&mut vic_entry);
                    Deques::unlink_wo(&mut deqs.write_order, &mut vic_entry);
                    self.entry_count -= 1;
                }

                // Add the candidate to the deque.
                let entry = cache.get_mut(&key).unwrap();
                let key = Rc::clone(&key);
                deqs.push_back_ao(
                    CacheRegion::MainProbation,
                    KeyHashDate::new(Rc::clone(&key), hash, timestamp),
                    entry,
                );
                if self.time_to_live.is_some() {
                    deqs.push_back_wo(KeyDate::new(key, timestamp), entry);
                }

                self.entry_count += 1;
                Self::saturating_sub_from_total_weight(self, victims_weight);
                Self::saturating_add_to_total_weight(self, policy_weight as u64);

                if self.should_enable_frequency_sketch() {
                    self.enable_frequency_sketch();
                }
            }
            AdmissionResult::Rejected => {
                // Remove the candidate from the cache.
                cache.remove(&key);
            }
        }
    }

    /// Performs size-aware admission explained in the paper:
    /// [Lightweight Robust Size Aware Cache Management][size-aware-cache-paper]
    /// by Gil Einziger, Ohad Eytan, Roy Friedman, Ben Manes.
    ///
    /// [size-aware-cache-paper]: https://arxiv.org/abs/2105.08770
    ///
    /// There are some modifications in this implementation:
    /// - To admit to the main space, candidate's frequency must be higher than
    ///   the aggregated frequencies of the potential victims. (In the paper,
    ///   `>=` operator is used rather than `>`)  The `>` operator will do a better
    ///   job to prevent the main space from polluting.
    /// - When a candidate is rejected, the potential victims will stay at the LRU
    ///   position of the probation access-order queue. (In the paper, they will be
    ///   promoted (to the MRU position?) to force the eviction policy to select a
    ///   different set of victims for the next candidate). We may implement the
    ///   paper's behavior later?
    ///
    #[inline]
    fn admit(
        candidate: &EntrySizeAndFrequency,
        cache: &CacheStore<K, V, S>,
        deqs: &Deques<K>,
        freq: &FrequencySketch,
        weigher: &mut Option<Weigher<K, V>>,
    ) -> AdmissionResult<K> {
        let mut victims = EntrySizeAndFrequency::default();
        let mut victim_nodes = SmallVec::default();

        // Get first potential victim at the LRU position.
        let mut next_victim = deqs.probation.peek_front();

        // Aggregate potential victims.
        while victims.weight < candidate.weight {
            if candidate.freq < victims.freq {
                break;
            }
            if let Some(victim) = next_victim.take() {
                next_victim = victim.next_node();

                let vic_entry = cache
                    .get(&victim.element.key)
                    .expect("Cannot get an victim entry");
                victims.add_policy_weight(victim.element.key.as_ref(), &vic_entry.value, weigher);
                victims.add_frequency(freq, victim.element.hash);
                victim_nodes.push(NonNull::from(victim));
            } else {
                // No more potential victims.
                break;
            }
        }

        // Admit or reject the candidate.

        // TODO: Implement some randomness to mitigate hash DoS attack.
        // See Caffeine's implementation.

        if victims.weight >= candidate.weight && candidate.freq > victims.freq {
            AdmissionResult::Admitted {
                victim_nodes,
                victims_weight: victims.weight,
            }
        } else {
            AdmissionResult::Rejected
        }
    }

    fn handle_update(
        &mut self,
        key: Rc<K>,
        timestamp: Option<Instant>,
        policy_weight: u32,
        old_entry: ValueEntry<K, V>,
    ) {
        let old_policy_weight = old_entry.policy_weight();

        let entry = self.cache.get_mut(&key).unwrap();
        entry.replace_deq_nodes_with(old_entry);
        if let Some(ts) = timestamp {
            entry.set_last_accessed(ts);
            entry.set_last_modified(ts);
        }
        entry.set_policy_weight(policy_weight);

        let deqs = &mut self.deques;
        deqs.move_to_back_ao(entry);
        if self.time_to_live.is_some() {
            deqs.move_to_back_wo(entry);
        }

        self.saturating_sub_from_total_weight(old_policy_weight as u64);
        self.saturating_add_to_total_weight(policy_weight as u64);
    }

    fn evict_expired(&mut self, now: Instant) {
        if self.time_to_live.is_some() {
            let (count, weight) = self.remove_expired_wo(EVICTION_BATCH_SIZE, now);
            self.entry_count -= count;
            self.saturating_sub_from_total_weight(weight);
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

            let (count1, weight1) = rm_expired_ao("window", window);
            let (count2, weight2) = rm_expired_ao("probation", probation);
            let (count3, weight3) = rm_expired_ao("protected", protected);

            self.entry_count -= count1 + count2 + count3;
            self.saturating_sub_from_total_weight(weight1);
            self.saturating_sub_from_total_weight(weight2);
            self.saturating_sub_from_total_weight(weight3);
        }
    }

    // Returns (u64, u64) where (evicted_entry_count, evicted_policy_weight).
    #[inline]
    fn remove_expired_ao(
        deq_name: &str,
        deq: &mut Deque<KeyHashDate<K>>,
        write_order_deq: &mut Deque<KeyDate<K>>,
        cache: &mut CacheStore<K, V, S>,
        time_to_idle: &Option<Duration>,
        batch_size: usize,
        now: Instant,
    ) -> (u64, u64) {
        let mut evicted_entry_count = 0u64;
        let mut evicted_policy_weight = 0u64;

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

            let key = key.unwrap();

            if let Some(mut entry) = cache.remove(&key) {
                let weight = entry.policy_weight();
                Deques::unlink_ao_from_deque(deq_name, deq, &mut entry);
                Deques::unlink_wo(write_order_deq, &mut entry);
                evicted_entry_count += 1;
                evicted_policy_weight = evicted_policy_weight.saturating_add(weight as u64);
            } else {
                deq.pop_front();
            }
        }

        (evicted_entry_count, evicted_policy_weight)
    }

    // Returns (u64, u64) where (evicted_entry_count, evicted_policy_weight).
    #[inline]
    fn remove_expired_wo(&mut self, batch_size: usize, now: Instant) -> (u64, u64) {
        let mut evicted_entry_count = 0u64;
        let mut evicted_policy_weight = 0u64;
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

            let key = key.unwrap();

            if let Some(mut entry) = self.cache.remove(&key) {
                let weight = entry.policy_weight();
                self.deques.unlink_ao(&mut entry);
                Deques::unlink_wo(&mut self.deques.write_order, &mut entry);
                evicted_entry_count += 1;
                evicted_policy_weight = evicted_policy_weight.saturating_sub(weight as u64);
            } else {
                self.deques.write_order.pop_front();
            }
        }

        (evicted_entry_count, evicted_policy_weight)
    }

    #[inline]
    fn evict_lru_entries(&mut self) {
        const DEQ_NAME: &str = "probation";

        let weights_to_evict = self.weights_to_evict();
        let mut evicted_count = 0u64;
        let mut evicted_policy_weight = 0u64;

        {
            let deqs = &mut self.deques;
            let (probation, wo, cache) =
                (&mut deqs.probation, &mut deqs.write_order, &mut self.cache);

            for _ in 0..EVICTION_BATCH_SIZE {
                if evicted_policy_weight >= weights_to_evict {
                    break;
                }

                let key = probation
                    .peek_front()
                    .map(|node| Rc::clone(&node.element.key));

                if key.is_none() {
                    break;
                }
                let key = key.unwrap();

                if let Some(mut entry) = cache.remove(&key) {
                    let weight = entry.policy_weight();
                    Deques::unlink_ao_from_deque(DEQ_NAME, probation, &mut entry);
                    Deques::unlink_wo(wo, &mut entry);
                    evicted_count += 1;
                    evicted_policy_weight = evicted_policy_weight.saturating_add(weight as u64);
                } else {
                    probation.pop_front();
                }
            }
        }

        self.entry_count -= evicted_count;
        self.saturating_sub_from_total_weight(evicted_policy_weight);
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
    fn set_expiration_clock(&mut self, clock: Option<crate::common::time::Clock>) {
        self.expiration_clock = clock;
    }
}

#[derive(Default)]
struct EntrySizeAndFrequency {
    weight: u64,
    freq: u32,
}

impl EntrySizeAndFrequency {
    fn new(policy_weight: u64) -> Self {
        Self {
            weight: policy_weight,
            ..Default::default()
        }
    }

    fn add_policy_weight<K, V>(&mut self, key: &K, value: &V, weigher: &mut Option<Weigher<K, V>>) {
        self.weight += weigh(weigher, key, value) as u64;
    }

    fn add_frequency(&mut self, freq: &FrequencySketch, hash: u64) {
        self.freq += freq.frequency(hash) as u32;
    }
}

// Access-Order Queue Node
type AoqNode<K> = NonNull<DeqNode<KeyHashDate<K>>>;

enum AdmissionResult<K> {
    Admitted {
        victim_nodes: SmallVec<[AoqNode<K>; 8]>,
        victims_weight: u64,
    },
    Rejected,
}

//
// private free-standing functions
//
#[inline]
fn weigh<K, V>(weigher: &mut Option<Weigher<K, V>>, key: &K, value: &V) -> u32 {
    weigher.as_mut().map(|w| w(key, value)).unwrap_or(1)
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::Cache;
    use crate::common::time::Clock;

    use std::time::Duration;

    #[test]
    fn basic_single_thread() {
        let mut cache = Cache::new(3);
        cache.enable_frequency_sketch_for_testing();

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
        // because d's frequency is higher than c's.
        cache.insert("d", "dennis");
        assert_eq!(cache.get(&"a"), Some(&"alice"));
        assert_eq!(cache.get(&"b"), Some(&"bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(&"dennis"));

        cache.invalidate(&"b");
        assert_eq!(cache.get(&"b"), None);
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
        cache.enable_frequency_sketch_for_testing();

        cache.insert("a", alice);
        cache.insert("b", bob);
        assert_eq!(cache.get(&"a"), Some(&alice));
        assert_eq!(cache.get(&"b"), Some(&bob));
        // order (LRU -> MRU) and counts: a -> 1, b -> 1

        cache.insert("c", cindy);
        assert_eq!(cache.get(&"c"), Some(&cindy));
        // order and counts: a -> 1, b -> 1, c -> 1

        assert_eq!(cache.get(&"a"), Some(&alice));
        assert_eq!(cache.get(&"b"), Some(&bob));
        // order and counts: c -> 1, a -> 2, b -> 2

        // To enter "d" (weight: 15), it needs to evict "c" (w: 5) and "a" (w: 10).
        // "d" must have higher count than 3, which is the aggregated count
        // of "a" and "c".
        cache.insert("d", david); //   count: d -> 0
        assert_eq!(cache.get(&"d"), None); //   d -> 1

        cache.insert("d", david);
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        cache.insert("d", david);
        assert_eq!(cache.get(&"d"), None); //   d -> 3

        cache.insert("d", david);
        assert_eq!(cache.get(&"d"), None); //   d -> 4

        // Finally "d" should be admitted by evicting "c" and "a".
        cache.insert("d", dennis);
        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some(&bob));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(&dennis));

        // Update "b" with "bill" (w: 15 -> 20). This should evict "d" (w: 15).
        cache.insert("b", bill);
        assert_eq!(cache.get(&"b"), Some(&bill));
        assert_eq!(cache.get(&"d"), None);

        // Re-add "a" (w: 10) and update "b" with "bob" (w: 20 -> 15).
        cache.insert("a", alice);
        cache.insert("b", bob);
        assert_eq!(cache.get(&"a"), Some(&alice));
        assert_eq!(cache.get(&"b"), Some(&bob));
        assert_eq!(cache.get(&"d"), None);

        // Verify the sizes.
        assert_eq!(cache.entry_count, 2);
        assert_eq!(cache.weighted_size, 25);
    }

    #[test]
    fn invalidate_all() {
        let mut cache = Cache::new(100);
        cache.enable_frequency_sketch_for_testing();

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
        cache.enable_frequency_sketch_for_testing();

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
        let mut cache = Cache::builder()
            .max_capacity(100)
            .time_to_live(Duration::from_secs(10))
            .build();
        cache.enable_frequency_sketch_for_testing();

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
        let mut cache = Cache::builder()
            .max_capacity(100)
            .time_to_idle(Duration::from_secs(10))
            .build();
        cache.enable_frequency_sketch_for_testing();

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

    #[cfg_attr(target_pointer_width = "16", ignore)]
    #[test]
    fn test_skt_capacity_will_not_overflow() {
        // power of two
        let pot = |exp| 2u64.pow(exp);

        let ensure_sketch_len = |max_capacity, len, name| {
            let mut cache = Cache::<u8, u8>::new(max_capacity);
            cache.enable_frequency_sketch_for_testing();
            assert_eq!(cache.frequency_sketch.table_len(), len as usize, "{}", name);
        };

        if cfg!(target_pointer_width = "32") {
            let pot24 = pot(24);
            let pot16 = pot(16);
            ensure_sketch_len(0, 128, "0");
            ensure_sketch_len(128, 128, "128");
            ensure_sketch_len(pot16, pot16, "pot16");
            // due to ceiling to next_power_of_two
            ensure_sketch_len(pot16 + 1, pot(17), "pot16 + 1");
            // due to ceiling to next_power_of_two
            ensure_sketch_len(pot24 - 1, pot24, "pot24 - 1");
            ensure_sketch_len(pot24, pot24, "pot24");
            ensure_sketch_len(pot(27), pot24, "pot(27)");
            ensure_sketch_len(u32::MAX as u64, pot24, "u32::MAX");
        } else {
            // target_pointer_width: 64 or larger.
            let pot30 = pot(30);
            let pot16 = pot(16);
            ensure_sketch_len(0, 128, "0");
            ensure_sketch_len(128, 128, "128");
            ensure_sketch_len(pot16, pot16, "pot16");
            // due to ceiling to next_power_of_two
            ensure_sketch_len(pot16 + 1, pot(17), "pot16 + 1");
            // due to ceiling to next_power_of_two
            ensure_sketch_len(pot30 - 1, pot30, "pot30- 1");
            ensure_sketch_len(pot30, pot30, "pot30");
            ensure_sketch_len(u64::MAX, pot30, "u64::MAX");
        };
    }
}
