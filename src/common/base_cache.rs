use crate::sync::cache::{
    READ_LOG_FLUSH_POINT, WRITE_LOG_FLUSH_POINT, WRITE_LOG_LOW_WATER_MARK, WRITE_LOG_SIZE,
};

use super::{
    deque::{CacheRegion, DeqNode, Deque},
    deques::Deques,
    frequency_sketch::FrequencySketch,
    housekeeper::{InnerSync, SyncPace},
    AccessTime, KeyDate, KeyHash, KeyHashDate, ReadOp, ValueEntry, WriteOp,
};

use crossbeam_channel::Receiver;
use parking_lot::{Mutex, MutexGuard, RwLock};
use quanta::{Clock, Instant};
use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash, Hasher},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

type CacheStore<K, V, S> = cht::SegmentedHashMap<Arc<K>, Arc<ValueEntry<K, V>>, S>;

pub(crate) struct Inner<K, V, S> {
    pub(crate) max_capacity: usize,
    pub(crate) cache: CacheStore<K, V, S>,
    build_hasher: S,
    deques: Mutex<Deques<K>>,
    frequency_sketch: RwLock<FrequencySketch>,
    read_op_ch: Receiver<ReadOp<K, V>>,
    write_op_ch: Receiver<WriteOp<K, V>>,
    pub(crate) time_to_live: Option<Duration>,
    pub(crate) time_to_idle: Option<Duration>,
    pub(crate) has_expiration_clock: AtomicBool,
    pub(crate) expiration_clock: RwLock<Option<Clock>>,
}

// functions/methods used by Cache
impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    pub(crate) fn new(
        max_capacity: usize,
        initial_capacity: Option<usize>,
        build_hasher: S,
        read_op_ch: Receiver<ReadOp<K, V>>,
        write_op_ch: Receiver<WriteOp<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let initial_capacity = initial_capacity
            .map(|cap| cap + WRITE_LOG_SIZE * 4)
            .unwrap_or_default();
        let num_segments = 64;
        let cache = cht::SegmentedHashMap::with_num_segments_capacity_and_hasher(
            num_segments,
            initial_capacity,
            build_hasher.clone(),
        );
        let skt_capacity = usize::max(max_capacity * 32, 100);
        let frequency_sketch = FrequencySketch::with_capacity(skt_capacity);
        Self {
            max_capacity,
            cache,
            build_hasher,
            deques: Mutex::new(Deques::default()),
            frequency_sketch: RwLock::new(frequency_sketch),
            read_op_ch,
            write_op_ch,
            time_to_live,
            time_to_idle,
            has_expiration_clock: AtomicBool::new(false),
            expiration_clock: RwLock::new(None),
        }
    }

    #[inline]
    pub(crate) fn hash<Q>(&self, key: &Q) -> u64
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut hasher = self.build_hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[inline]
    pub(crate) fn get<Q>(&self, key: &Q) -> Option<Arc<ValueEntry<K, V>>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cache.get(key)
    }

    fn apply_reads(&self, deqs: &mut Deques<K>, count: usize) {
        use ReadOp::*;
        let mut freq = self.frequency_sketch.write();
        let ch = &self.read_op_ch;
        for _ in 0..count {
            match ch.try_recv() {
                Ok(Hit(hash, mut entry, timestamp)) => {
                    freq.increment(hash);
                    if let Some(ts) = timestamp {
                        entry.set_last_accessed(ts);
                    }
                    deqs.move_to_back_ao(entry)
                }
                Ok(Miss(hash)) => freq.increment(hash),
                Err(_) => break,
            }
        }
    }

    fn apply_writes(&self, deqs: &mut Deques<K>, count: usize) {
        use WriteOp::*;
        let freq = self.frequency_sketch.read();
        let ch = &self.write_op_ch;

        let timestamp = if self.has_expiry() {
            Some(self.current_time_from_expiration_clock())
        } else {
            None
        };

        for _ in 0..count {
            match ch.try_recv() {
                Ok(Insert(kh, entry)) => self.handle_insert(kh, entry, timestamp, deqs, &freq),
                Ok(Update(mut entry)) => {
                    if let Some(ts) = timestamp {
                        entry.set_last_accessed(ts);
                        entry.set_last_modified(ts);
                    }
                    deqs.move_to_back_ao(Arc::clone(&entry));
                    deqs.move_to_back_wo(entry)
                }
                Ok(Remove(entry)) => {
                    deqs.unlink_ao(Arc::clone(&entry));
                    Deques::unlink_wo(&mut deqs.write_order, entry);
                }
                Err(_) => break,
            };
        }
    }

    fn evict(&self, deqs: &mut Deques<K>, batch_size: usize) {
        debug_assert!(self.has_expiry());

        let now = self.current_time_from_expiration_clock();

        if self.time_to_live.is_some() {
            self.remove_expired_wo(deqs, batch_size, now);
        }

        if self.time_to_idle.is_some() {
            let (window, probation, protected, wo) = (
                &mut deqs.window,
                &mut deqs.probation,
                &mut deqs.protected,
                &mut deqs.write_order,
            );

            let mut rm_expired_ao =
                |name, deq| self.remove_expired_ao(name, deq, wo, batch_size, now);

            rm_expired_ao("window", window);
            rm_expired_ao("probation", probation);
            rm_expired_ao("protected", protected);
        }
    }

    #[inline]
    fn remove_expired_ao(
        &self,
        deq_name: &str,
        deq: &mut Deque<KeyHashDate<K>>,
        write_order_deq: &mut Deque<KeyDate<K>>,
        batch_size: usize,
        now: Instant,
    ) {
        for _ in 0..batch_size {
            let key = deq
                .peek_front()
                .and_then(|node| {
                    if self.is_expired_entry_ao(&*node, now) {
                        Some(Some(Arc::clone(&node.element.key)))
                    } else {
                        None
                    }
                })
                .unwrap_or(None);

            if key.is_none() {
                break;
            }

            if let Some(entry) = self.cache.remove(&key.unwrap()) {
                Deques::unlink_ao_from_deque(deq_name, deq, Arc::clone(&entry));
                Deques::unlink_wo(write_order_deq, entry);
            } else {
                deq.pop_front();
            }
        }
    }

    #[inline]
    fn remove_expired_wo(&self, deqs: &mut Deques<K>, batch_size: usize, now: Instant) {
        for _ in 0..batch_size {
            let key = deqs
                .write_order
                .peek_front()
                .and_then(|node| {
                    if self.is_expired_entry_wo(&*node, now) {
                        Some(Some(Arc::clone(&node.element.key)))
                    } else {
                        None
                    }
                })
                .unwrap_or(None);

            if key.is_none() {
                break;
            }

            if let Some(entry) = self.cache.remove(&key.unwrap()) {
                deqs.unlink_ao(Arc::clone(&entry));
                Deques::unlink_wo(&mut deqs.write_order, entry);
            } else {
                deqs.write_order.pop_front();
            }
        }
    }

    #[inline]
    pub(crate) fn current_time_from_expiration_clock(&self) -> Instant {
        if self.has_expiration_clock.load(Ordering::Relaxed) {
            self.expiration_clock
                .read()
                .as_ref()
                .expect("Cannot get the expiration clock")
                .now()
        } else {
            Instant::now()
        }
    }

    #[inline]
    pub(crate) fn has_expiry(&self) -> bool {
        self.time_to_live.is_some() || self.time_to_idle.is_some()
    }

    #[inline]
    pub(crate) fn is_expired_entry_ao(&self, entry: &impl AccessTime, now: Instant) -> bool {
        debug_assert!(self.has_expiry());
        if let (Some(ts), Some(tti)) = (entry.last_accessed(), self.time_to_idle) {
            if ts + tti <= now {
                return true;
            }
        }
        false
    }

    #[inline]
    pub(crate) fn is_expired_entry_wo(&self, entry: &impl AccessTime, now: Instant) -> bool {
        debug_assert!(self.has_expiry());
        if let (Some(ts), Some(ttl)) = (entry.last_modified(), self.time_to_live) {
            if ts + ttl <= now {
                return true;
            }
        }
        false
    }
}

impl<K, V, S> InnerSync for Inner<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    fn sync(&self, max_repeats: usize) -> Option<SyncPace> {
        if self.read_op_ch.is_empty() && self.write_op_ch.is_empty() && !self.has_expiry() {
            return None;
        }

        let deqs = self.deques.lock();
        self.do_sync(deqs, max_repeats)
    }
}

// private methods
impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    #[inline]
    fn admit(
        &self,
        candidate_hash: u64,
        victim: &DeqNode<KeyHashDate<K>>,
        freq: &FrequencySketch,
    ) -> bool {
        // TODO: Implement some randomness to mitigate hash DoS attack.
        // See Caffeine's implementation.
        freq.frequency(candidate_hash) > freq.frequency(victim.element.hash)
    }

    fn do_sync(&self, mut deqs: MutexGuard<'_, Deques<K>>, max_repeats: usize) -> Option<SyncPace> {
        let mut calls = 0;
        let mut should_sync = true;
        const EVICTION_BATCH_SIZE: usize = 500;

        while should_sync && calls <= max_repeats {
            let r_len = self.read_op_ch.len();
            if r_len > 0 {
                self.apply_reads(&mut deqs, r_len);
            }

            let w_len = self.write_op_ch.len();
            if w_len > 0 {
                self.apply_writes(&mut deqs, w_len);
            }

            if self.has_expiry() {
                self.evict(&mut deqs, EVICTION_BATCH_SIZE);
            }

            calls += 1;
            should_sync = self.read_op_ch.len() >= READ_LOG_FLUSH_POINT
                || self.write_op_ch.len() >= WRITE_LOG_FLUSH_POINT;
        }

        if should_sync {
            Some(SyncPace::Fast)
        } else if self.write_op_ch.len() <= WRITE_LOG_LOW_WATER_MARK {
            Some(SyncPace::Normal)
        } else {
            // Keep the current pace.
            None
        }
    }

    #[inline]
    fn find_cache_victim<'a>(
        &self,
        deqs: &'a mut Deques<K>,
        _freq: &FrequencySketch,
    ) -> &'a DeqNode<KeyHashDate<K>> {
        // TODO: Check its frequency. If it is not very low, maybe we should
        // check frequencies of next few others and pick from them.
        deqs.probation.peek_front().expect("No victim found")
    }

    #[inline]
    fn handle_insert(
        &self,
        kh: KeyHash<K>,
        entry: Arc<ValueEntry<K, V>>,
        timestamp: Option<Instant>,
        deqs: &mut Deques<K>,
        freq: &FrequencySketch,
    ) {
        let last_accessed = entry.raw_last_accessed().map(|ts| {
            ts.store(timestamp.unwrap().as_u64(), Ordering::Relaxed);
            ts
        });
        let last_modified = entry.raw_last_modified().map(|ts| {
            ts.store(timestamp.unwrap().as_u64(), Ordering::Relaxed);
            ts
        });

        if self.cache.len() <= self.max_capacity {
            // Add the candidate to the deque.
            let key = Arc::clone(&kh.key);
            deqs.push_back_ao(
                CacheRegion::MainProbation,
                KeyHashDate::new(kh, last_accessed),
                &entry,
            );
            if self.time_to_live.is_some() {
                deqs.push_back_wo(KeyDate::new(key, last_modified), &entry);
            }
        } else {
            let victim = self.find_cache_victim(deqs, freq);
            if self.admit(kh.hash, victim, freq) {
                // Remove the victim from the cache and deque.
                //
                // TODO: Check if the selected victim was actually removed. If not,
                // maybe we should find another victim. This can happen because it
                // could have been already removed from the cache but the removal
                // from the deque is still on the write operations queue and is not
                // yet executed.
                if let Some(vic_entry) = self.cache.remove(&victim.element.key) {
                    deqs.unlink_ao(Arc::clone(&vic_entry));
                    Deques::unlink_wo(&mut deqs.write_order, vic_entry);
                } else {
                    let victim = NonNull::from(victim);
                    deqs.unlink_node_ao(victim);
                }
                // Add the candidate to the deque.
                let key = Arc::clone(&kh.key);
                deqs.push_back_ao(
                    CacheRegion::MainProbation,
                    KeyHashDate::new(kh, last_accessed),
                    &entry,
                );
                if self.time_to_live.is_some() {
                    deqs.push_back_wo(KeyDate::new(key, last_modified), &entry);
                }
            } else {
                // Remove the candidate from the cache.
                self.cache.remove(&kh.key);
            }
        }
    }
}
