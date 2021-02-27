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
    hash::{BuildHasher, Hash, Hasher},
    ptr::NonNull,
    rc::Rc,
    time::Duration,
};

type CacheStore<K, V, S> = std::collections::HashMap<Rc<K>, ValueEntry<K, V>, S>;

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
    V: Clone,
{
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
    V: Clone,
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
        let skt_capacity = usize::max(max_capacity * 32, 100);
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

    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        Rc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hash(key);
        let has_expiry = self.has_expiry();
        let timestamp = if has_expiry {
            Some(self.current_time_from_expiration_clock())
        } else {
            None
        };

        if let Some(ts) = timestamp {
            self.evict(ts);
        }

        let (entry, sketch, deqs) = (
            self.cache.get_mut(key),
            &mut self.frequency_sketch,
            &mut self.deques,
        );

        match (entry, has_expiry) {
            // Value not found.
            (None, _) => {
                Self::record_read(sketch, deqs, hash, None, None);
                None
            }
            // Value found, no expiry.
            (Some(entry), false) => {
                Self::record_read(sketch, deqs, hash, Some(entry), None);
                Some(&entry.value)
            }
            // Value found, need to check if expired.
            (Some(entry), true) => {
                if Self::is_expired_entry_wo(&self.time_to_live, entry, timestamp.unwrap())
                    || Self::is_expired_entry_ao(&self.time_to_idle, entry, timestamp.unwrap())
                {
                    // Expired entry. Record this access as a cache miss rather than a hit.
                    Self::record_read(sketch, deqs, hash, None, None);
                    None
                } else {
                    // Valid entry.
                    Self::record_read(sketch, deqs, hash, Some(entry), timestamp);
                    Some(&entry.value)
                }
            }
        }
    }

    pub fn insert(&mut self, key: K, value: V) {
        let timestamp = if self.has_expiry() {
            Some(self.current_time_from_expiration_clock())
        } else {
            None
        };

        if let Some(ts) = timestamp {
            self.evict(ts);
        }

        let key = Rc::new(key);
        let entry = ValueEntry::new(value);

        if let Some(old_entry) = self.cache.insert(Rc::clone(&key), entry) {
            self.handle_update(key, timestamp, old_entry);
        } else {
            // Insert
            let hash = self.hash(&key);
            self.handle_insert(key, hash, timestamp);
        }
    }

    pub fn invalidate<Q>(&mut self, key: &Q)
    where
        Rc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.has_expiry() {
            let ts = self.current_time_from_expiration_clock();
            self.evict(ts);
        }

        if let Some(mut entry) = self.cache.remove(key) {
            self.deques.unlink_ao(&mut entry);
            Deques::unlink_wo(&mut self.deques.write_order, &mut entry)
        }
    }

    pub fn max_capacity(&self) -> usize {
        self.max_capacity
    }

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
    V: Clone,
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
            if ts + *tti <= now {
                return true;
            }
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
            if ts + *ttl <= now {
                return true;
            }
        }
        false
    }

    fn record_read(
        frequency_sketch: &mut FrequencySketch,
        deques: &mut Deques<K>,
        hash: u64,
        entry: Option<&mut ValueEntry<K, V>>,
        timestamp: Option<Instant>,
    ) {
        frequency_sketch.increment(hash);
        if let Some(entry) = entry {
            if let Some(ts) = timestamp {
                entry.set_last_accessed(ts);
            }
            deques.move_to_back_ao(entry)
        }
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
        deqs.move_to_back_ao(&entry);
        deqs.move_to_back_wo(&entry)
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
