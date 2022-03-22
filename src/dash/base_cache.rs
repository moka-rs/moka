use super::Iter;
use crate::{
    common::{
        self,
        atomic_time::AtomicInstant,
        deque::{DeqNode, Deque},
        frequency_sketch::FrequencySketch,
        time::{CheckedTimeOps, Clock, Instant},
        CacheRegion,
    },
    sync::{
        deques::Deques,
        entry_info::EntryInfo,
        housekeeper::{Housekeeper, InnerSync, SyncPace},
        AccessTime, KeyDate, KeyHash, KeyHashDate, KvEntry, ReadOp, ValueEntry, Weigher, WriteOp,
    },
    Policy,
};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use crossbeam_utils::atomic::AtomicCell;
use dashmap::mapref::one::Ref as DashMapRef;
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use triomphe::Arc as TrioArc;

pub(crate) const MAX_SYNC_REPEATS: usize = 4;

const READ_LOG_FLUSH_POINT: usize = 512;
const READ_LOG_SIZE: usize = READ_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

const WRITE_LOG_FLUSH_POINT: usize = 512;
const WRITE_LOG_LOW_WATER_MARK: usize = WRITE_LOG_FLUSH_POINT / 2;
const WRITE_LOG_SIZE: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

pub(crate) const WRITE_RETRY_INTERVAL_MICROS: u64 = 50;

pub(crate) type HouseKeeperArc<K, V, S> = Arc<Housekeeper<Inner<K, V, S>>>;

pub(crate) struct BaseCache<K, V, S = RandomState> {
    pub(crate) inner: Arc<Inner<K, V, S>>,
    read_op_ch: Sender<ReadOp<K, V>>,
    pub(crate) write_op_ch: Sender<WriteOp<K, V>>,
    pub(crate) housekeeper: Option<HouseKeeperArc<K, V, S>>,
}

impl<K, V, S> Clone for BaseCache<K, V, S> {
    /// Makes a clone of this shared cache.
    ///
    /// This operation is cheap as it only creates thread-safe reference counted
    /// pointers to the shared internal data structures.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            read_op_ch: self.read_op_ch.clone(),
            write_op_ch: self.write_op_ch.clone(),
            housekeeper: self.housekeeper.as_ref().map(Arc::clone),
        }
    }
}

impl<K, V, S> Drop for BaseCache<K, V, S> {
    fn drop(&mut self) {
        // The housekeeper needs to be dropped before the inner is dropped.
        std::mem::drop(self.housekeeper.take());
    }
}

impl<K, V, S> BaseCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let (r_snd, r_rcv) = crossbeam_channel::bounded(READ_LOG_SIZE);
        let (w_snd, w_rcv) = crossbeam_channel::bounded(WRITE_LOG_SIZE);
        let inner = Arc::new(Inner::new(
            max_capacity,
            initial_capacity,
            build_hasher,
            weigher,
            r_rcv,
            w_rcv,
            time_to_live,
            time_to_idle,
        ));
        let housekeeper = Housekeeper::new(Arc::downgrade(&inner));
        Self {
            inner,
            read_op_ch: r_snd,
            write_op_ch: w_snd,
            housekeeper: Some(Arc::new(housekeeper)),
        }
    }

    #[inline]
    pub(crate) fn hash<Q>(&self, key: &Q) -> u64
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.hash(key)
    }

    pub(crate) fn get_with_hash<Q>(&self, key: &Q, hash: u64) -> Option<V>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let record = |op| {
            self.record_read_op(op).expect("Failed to record a get op");
        };

        match self.inner.get(key) {
            None => {
                record(ReadOp::Miss(hash));
                None
            }
            Some(entry) => {
                let i = &self.inner;
                let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), &i.valid_after());
                let now = i.current_time_from_expiration_clock();
                let entry = &*entry;

                if is_expired_entry_wo(ttl, va, entry, now)
                    || is_expired_entry_ao(tti, va, entry, now)
                {
                    // Expired or invalidated entry. Record this access as a cache miss
                    // rather than a hit.
                    record(ReadOp::Miss(hash));
                    None
                } else {
                    // Valid entry.
                    let v = entry.value.clone();
                    record(ReadOp::Hit(hash, TrioArc::clone(entry), now));
                    Some(v)
                }
            }
        }
    }

    #[inline]
    pub(crate) fn remove_entry<Q>(&self, key: &Q) -> Option<KvEntry<K, V>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.remove_entry(key)
    }

    #[inline]
    pub(crate) fn apply_reads_writes_if_needed(
        ch: &Sender<WriteOp<K, V>>,
        housekeeper: Option<&HouseKeeperArc<K, V, S>>,
    ) {
        let w_len = ch.len();

        if Self::should_apply_writes(w_len) {
            if let Some(h) = housekeeper {
                h.try_schedule_sync();
            }
        }
    }

    pub(crate) fn invalidate_all(&self) {
        let now = self.inner.current_time_from_expiration_clock();
        self.inner.set_valid_after(now);
    }

    pub(crate) fn policy(&self) -> Policy {
        self.inner.policy()
    }

    #[cfg(test)]
    pub(crate) fn estimated_entry_count(&self) -> u64 {
        self.inner.estimated_entry_count()
    }

    #[cfg(test)]
    pub(crate) fn weighted_size(&self) -> u64 {
        self.inner.weighted_size()
    }
}

impl<'a, K, V, S> BaseCache<K, V, S>
where
    K: 'a + Eq + Hash,
    V: 'a,
    S: BuildHasher + Clone,
{
    pub(crate) fn iter(&self) -> Iter<'_, K, V, S> {
        self.inner.iter()
    }
}

//
// private methods
//
impl<K, V, S> BaseCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    #[inline]
    fn record_read_op(&self, op: ReadOp<K, V>) -> Result<(), TrySendError<ReadOp<K, V>>> {
        self.apply_reads_if_needed();
        let ch = &self.read_op_ch;
        match ch.try_send(op) {
            // Discard the ReadOp when the channel is full.
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(e @ TrySendError::Disconnected(_)) => Err(e),
        }
    }

    #[inline]
    pub(crate) fn do_insert_with_hash(&self, key: Arc<K>, hash: u64, value: V) -> WriteOp<K, V> {
        let weight = self.inner.weigh(&key, &value);
        let mut insert_op = None;
        let mut update_op = None;

        self.inner
            .cache
            .entry(Arc::clone(&key))
            // Update
            .and_modify(|entry| {
                // NOTES on `new_value_entry_from` method:
                // 1. The internal EntryInfo will be shared between the old and new ValueEntries.
                // 2. This method will set the last_accessed and last_modified to the max value to
                //    prevent this new ValueEntry from being evicted by an expiration policy.
                // 3. This method will update the policy_weight with the new weight.
                let old_weight = entry.policy_weight();
                *entry = self.new_value_entry_from(value.clone(), weight, entry);
                update_op = Some(WriteOp::Upsert {
                    key_hash: KeyHash::new(Arc::clone(&key), hash),
                    value_entry: TrioArc::clone(entry),
                    old_weight,
                    new_weight: weight,
                });
            })
            // Insert
            .or_insert_with(|| {
                let entry = self.new_value_entry(value.clone(), weight);
                insert_op = Some(WriteOp::Upsert {
                    key_hash: KeyHash::new(Arc::clone(&key), hash),
                    value_entry: TrioArc::clone(&entry),
                    old_weight: 0,
                    new_weight: weight,
                });
                entry
            });

        match (insert_op, update_op) {
            (Some(ins_op), None) => ins_op,
            (None, Some(upd_op)) => upd_op,
            _ => unreachable!(),
        }
    }

    #[inline]
    fn new_value_entry(&self, value: V, policy_weight: u32) -> TrioArc<ValueEntry<K, V>> {
        let info = TrioArc::new(EntryInfo::new(policy_weight));
        TrioArc::new(ValueEntry::new(value, info))
    }

    #[inline]
    fn new_value_entry_from(
        &self,
        value: V,
        policy_weight: u32,
        other: &ValueEntry<K, V>,
    ) -> TrioArc<ValueEntry<K, V>> {
        let info = TrioArc::clone(other.entry_info());
        info.set_policy_weight(policy_weight);
        TrioArc::new(ValueEntry::new_from(value, info, other))
    }

    #[inline]
    fn apply_reads_if_needed(&self) {
        let len = self.read_op_ch.len();

        if Self::should_apply_reads(len) {
            if let Some(h) = &self.housekeeper {
                h.try_schedule_sync();
            }
        }
    }

    #[inline]
    fn should_apply_reads(ch_len: usize) -> bool {
        ch_len >= READ_LOG_FLUSH_POINT
    }

    #[inline]
    fn should_apply_writes(ch_len: usize) -> bool {
        ch_len >= WRITE_LOG_FLUSH_POINT
    }
}

//
// for testing
//
#[cfg(test)]
impl<K, V, S> BaseCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn reconfigure_for_testing(&mut self) {
        // Stop the housekeeping job that may cause sync() method to return earlier.
        if let Some(housekeeper) = &self.housekeeper {
            // TODO: Extract this into a housekeeper method.
            let mut job = housekeeper.periodical_sync_job().lock();
            if let Some(job) = job.take() {
                job.cancel();
            }
        }
        // Enable the frequency sketch.
        self.inner.enable_frequency_sketch_for_testing();
    }

    pub(crate) fn set_expiration_clock(&self, clock: Option<Clock>) {
        self.inner.set_expiration_clock(clock);
    }
}

struct EvictionCounters {
    entry_count: u64,
    weighted_size: u64,
}

impl EvictionCounters {
    #[inline]
    fn new(entry_count: u64, weighted_size: u64) -> Self {
        Self {
            entry_count,
            weighted_size,
        }
    }

    #[inline]
    fn saturating_add(&mut self, entry_count: u64, weight: u32) {
        self.entry_count += entry_count;
        let total = &mut self.weighted_size;
        *total = total.saturating_add(weight as u64);
    }

    #[inline]
    fn saturating_sub(&mut self, entry_count: u64, weight: u32) {
        self.entry_count -= entry_count;
        let total = &mut self.weighted_size;
        *total = total.saturating_sub(weight as u64);
    }
}

#[derive(Default)]
struct EntrySizeAndFrequency {
    policy_weight: u64,
    freq: u32,
}

impl EntrySizeAndFrequency {
    fn new(policy_weight: u32) -> Self {
        Self {
            policy_weight: policy_weight as u64,
            ..Default::default()
        }
    }

    fn add_policy_weight(&mut self, weight: u32) {
        self.policy_weight += weight as u64;
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
        skipped_nodes: SmallVec<[AoqNode<K>; 4]>,
    },
    Rejected {
        skipped_nodes: SmallVec<[AoqNode<K>; 4]>,
    },
}

type CacheStore<K, V, S> = dashmap::DashMap<Arc<K>, TrioArc<ValueEntry<K, V>>, S>;

type CacheEntryRef<'a, K, V, S> = DashMapRef<'a, Arc<K>, TrioArc<ValueEntry<K, V>>, S>;

pub(crate) struct Inner<K, V, S> {
    max_capacity: Option<u64>,
    entry_count: AtomicCell<u64>,
    weighted_size: AtomicCell<u64>,
    cache: CacheStore<K, V, S>,
    build_hasher: S,
    deques: Mutex<Deques<K>>,
    frequency_sketch: RwLock<FrequencySketch>,
    frequency_sketch_enabled: AtomicBool,
    read_op_ch: Receiver<ReadOp<K, V>>,
    write_op_ch: Receiver<WriteOp<K, V>>,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    valid_after: AtomicInstant,
    weigher: Option<Weigher<K, V>>,
    has_expiration_clock: AtomicBool,
    expiration_clock: RwLock<Option<Clock>>,
}

// functions/methods used by BaseCache
impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone,
{
    // Disable a Clippy warning for having more than seven arguments.
    // https://rust-lang.github.io/rust-clippy/master/index.html#too_many_arguments
    #[allow(clippy::too_many_arguments)]
    fn new(
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        read_op_ch: Receiver<ReadOp<K, V>>,
        write_op_ch: Receiver<WriteOp<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let initial_capacity = initial_capacity
            .map(|cap| cap + WRITE_LOG_SIZE * 4)
            .unwrap_or_default();
        let cache =
            dashmap::DashMap::with_capacity_and_hasher(initial_capacity, build_hasher.clone());

        Self {
            max_capacity: max_capacity.map(|n| n as u64),
            entry_count: Default::default(),
            weighted_size: Default::default(),
            cache,
            build_hasher,
            deques: Mutex::new(Default::default()),
            frequency_sketch: RwLock::new(Default::default()),
            frequency_sketch_enabled: Default::default(),
            read_op_ch,
            write_op_ch,
            time_to_live,
            time_to_idle,
            valid_after: Default::default(),
            weigher,
            has_expiration_clock: AtomicBool::new(false),
            expiration_clock: RwLock::new(None),
        }
    }

    #[inline]
    fn hash<Q>(&self, key: &Q) -> u64
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut hasher = self.build_hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[inline]
    fn get<Q>(&self, key: &Q) -> Option<CacheEntryRef<'_, K, V, S>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cache.get(key)
    }

    #[inline]
    fn remove_entry<Q>(&self, key: &Q) -> Option<KvEntry<K, V>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cache
            .remove(key)
            .map(|(key, entry)| KvEntry::new(key, entry))
    }

    fn policy(&self) -> Policy {
        Policy::new(self.max_capacity, 1, self.time_to_live, self.time_to_idle)
    }

    #[inline]
    fn time_to_live(&self) -> Option<Duration> {
        self.time_to_live
    }

    #[inline]
    fn time_to_idle(&self) -> Option<Duration> {
        self.time_to_idle
    }

    #[cfg(test)]
    #[inline]
    fn estimated_entry_count(&self) -> u64 {
        self.entry_count.load()
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn weighted_size(&self) -> u64 {
        self.weighted_size.load()
    }

    #[inline]
    fn has_expiry(&self) -> bool {
        self.time_to_live.is_some() || self.time_to_idle.is_some()
    }

    #[inline]
    fn is_write_order_queue_enabled(&self) -> bool {
        self.time_to_live.is_some()
    }

    #[inline]
    fn valid_after(&self) -> Option<Instant> {
        self.valid_after.instant()
    }

    #[inline]
    fn set_valid_after(&self, timestamp: Instant) {
        self.valid_after.set_instant(timestamp);
    }

    #[inline]
    fn has_valid_after(&self) -> bool {
        self.valid_after.is_set()
    }

    #[inline]
    fn weigh(&self, key: &K, value: &V) -> u32 {
        self.weigher.as_ref().map(|w| w(key, value)).unwrap_or(1)
    }

    #[inline]
    fn current_time_from_expiration_clock(&self) -> Instant {
        if self.has_expiration_clock.load(Ordering::Relaxed) {
            Instant::new(
                self.expiration_clock
                    .read()
                    .as_ref()
                    .expect("Cannot get the expiration clock")
                    .now(),
            )
        } else {
            Instant::now()
        }
    }
}

impl<'a, K, V, S> Inner<K, V, S>
where
    K: 'a + Eq + Hash,
    V: 'a,
    S: BuildHasher + Clone,
{
    fn iter(&self) -> Iter<'_, K, V, S> {
        let map_iter = self.cache.iter();
        Iter::new(map_iter)
    }
}

mod batch_size {
    pub(crate) const EVICTION_BATCH_SIZE: usize = 500;
}

// TODO: Divide this method into smaller methods so that unit tests can do more
// precise testing.
// - sync_reads
// - sync_writes
// - evict
// - invalidate_entries
impl<K, V, S> InnerSync for Inner<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn sync(&self, max_repeats: usize) -> Option<SyncPace> {
        let mut deqs = self.deques.lock();
        let mut calls = 0;
        let mut should_sync = true;

        let current_ec = self.entry_count.load();
        let current_ws = self.weighted_size.load();
        let mut counters = EvictionCounters::new(current_ec, current_ws);

        while should_sync && calls <= max_repeats {
            let r_len = self.read_op_ch.len();
            if r_len > 0 {
                self.apply_reads(&mut deqs, r_len);
            }

            let w_len = self.write_op_ch.len();
            if w_len > 0 {
                self.apply_writes(&mut deqs, w_len, &mut counters);
            }

            if self.should_enable_frequency_sketch(&counters) {
                self.enable_frequency_sketch(&counters);
            }

            calls += 1;
            should_sync = self.read_op_ch.len() >= READ_LOG_FLUSH_POINT
                || self.write_op_ch.len() >= WRITE_LOG_FLUSH_POINT;
        }

        if self.has_expiry() || self.has_valid_after() {
            self.evict_expired(&mut deqs, batch_size::EVICTION_BATCH_SIZE, &mut counters);
        }

        // Evict if this cache has more entries than its capacity.
        let weights_to_evict = self.weights_to_evict(&counters);
        if weights_to_evict > 0 {
            self.evict_lru_entries(
                &mut deqs,
                batch_size::EVICTION_BATCH_SIZE,
                weights_to_evict,
                &mut counters,
            );
        }

        debug_assert_eq!(self.entry_count.load(), current_ec);
        debug_assert_eq!(self.weighted_size.load(), current_ws);
        self.entry_count.store(counters.entry_count);
        self.weighted_size.store(counters.weighted_size);

        if should_sync {
            Some(SyncPace::Fast)
        } else if self.write_op_ch.len() <= WRITE_LOG_LOW_WATER_MARK {
            Some(SyncPace::Normal)
        } else {
            // Keep the current pace.
            None
        }
    }
}

//
// private methods
//
impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn has_enough_capacity(&self, candidate_weight: u32, counters: &EvictionCounters) -> bool {
        self.max_capacity
            .map(|limit| counters.weighted_size + candidate_weight as u64 <= limit)
            .unwrap_or(true)
    }

    fn weights_to_evict(&self, counters: &EvictionCounters) -> u64 {
        self.max_capacity
            .map(|limit| counters.weighted_size.saturating_sub(limit))
            .unwrap_or_default()
    }

    #[inline]
    fn should_enable_frequency_sketch(&self, counters: &EvictionCounters) -> bool {
        if self.frequency_sketch_enabled.load(Ordering::Acquire) {
            false
        } else if let Some(max_cap) = self.max_capacity {
            counters.weighted_size >= max_cap / 2
        } else {
            false
        }
    }

    #[inline]
    fn enable_frequency_sketch(&self, counters: &EvictionCounters) {
        if let Some(max_cap) = self.max_capacity {
            let c = counters;
            let cap = if self.weigher.is_none() {
                max_cap
            } else {
                (c.entry_count as f64 * (c.weighted_size as f64 / max_cap as f64)) as u64
            };
            self.do_enable_frequency_sketch(cap);
        }
    }

    #[cfg(test)]
    fn enable_frequency_sketch_for_testing(&self) {
        if let Some(max_cap) = self.max_capacity {
            self.do_enable_frequency_sketch(max_cap);
        }
    }

    #[inline]
    fn do_enable_frequency_sketch(&self, cache_capacity: u64) {
        let skt_capacity = common::sketch_capacity(cache_capacity);
        self.frequency_sketch.write().ensure_capacity(skt_capacity);
        self.frequency_sketch_enabled.store(true, Ordering::Release);
    }

    fn apply_reads(&self, deqs: &mut Deques<K>, count: usize) {
        use ReadOp::*;
        let mut freq = self.frequency_sketch.write();
        let ch = &self.read_op_ch;
        for _ in 0..count {
            match ch.try_recv() {
                Ok(Hit(hash, entry, timestamp)) => {
                    freq.increment(hash);
                    entry.set_last_accessed(timestamp);
                    deqs.move_to_back_ao(&entry)
                }
                Ok(Miss(hash)) => freq.increment(hash),
                Err(_) => break,
            }
        }
    }

    fn apply_writes(&self, deqs: &mut Deques<K>, count: usize, counters: &mut EvictionCounters) {
        use WriteOp::*;
        let freq = self.frequency_sketch.read();
        let ch = &self.write_op_ch;
        let ts = self.current_time_from_expiration_clock();

        for _ in 0..count {
            match ch.try_recv() {
                Ok(Upsert {
                    key_hash: kh,
                    value_entry: entry,
                    old_weight,
                    new_weight,
                }) => {
                    self.handle_upsert(kh, entry, old_weight, new_weight, ts, deqs, &freq, counters)
                }
                Ok(Remove(KvEntry { key: _key, entry })) => {
                    Self::handle_remove(deqs, entry, counters)
                }
                Err(_) => break,
            };
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_upsert(
        &self,
        kh: KeyHash<K>,
        entry: TrioArc<ValueEntry<K, V>>,
        old_weight: u32,
        new_weight: u32,
        timestamp: Instant,
        deqs: &mut Deques<K>,
        freq: &FrequencySketch,
        counters: &mut EvictionCounters,
    ) {
        entry.set_last_accessed(timestamp);
        entry.set_last_modified(timestamp);

        if entry.is_admitted() {
            // The entry has been already admitted, so treat this as an update.
            counters.saturating_sub(0, old_weight);
            counters.saturating_add(0, new_weight);
            deqs.move_to_back_ao(&entry);
            deqs.move_to_back_wo(&entry);
            return;
        }

        if self.has_enough_capacity(new_weight, counters) {
            // There are enough room in the cache (or the cache is unbounded).
            // Add the candidate to the deques.
            self.handle_admit(kh, &entry, new_weight, deqs, counters);
            return;
        }

        if let Some(max) = self.max_capacity {
            if new_weight as u64 > max {
                // The candidate is too big to fit in the cache. Reject it.
                self.cache.remove(&Arc::clone(&kh.key));
                return;
            }
        }

        let skipped_nodes;
        let mut candidate = EntrySizeAndFrequency::new(new_weight);
        candidate.add_frequency(freq, kh.hash);

        // Try to admit the candidate.
        match Self::admit(&candidate, &self.cache, deqs, freq) {
            AdmissionResult::Admitted {
                victim_nodes,
                skipped_nodes: mut skipped,
            } => {
                // Try to remove the victims from the cache (hash map).
                for victim in victim_nodes {
                    if let Some((_vic_key, vic_entry)) =
                        self.cache.remove(unsafe { victim.as_ref().element.key() })
                    {
                        // And then remove the victim from the deques.
                        Self::handle_remove(deqs, vic_entry, counters);
                    } else {
                        // Could not remove the victim from the cache. Skip this
                        // victim node as its ValueEntry might have been
                        // invalidated. Add it to the skipped nodes.
                        skipped.push(victim);
                    }
                }
                skipped_nodes = skipped;

                // Add the candidate to the deques.
                self.handle_admit(kh, &entry, new_weight, deqs, counters);
            }
            AdmissionResult::Rejected { skipped_nodes: s } => {
                skipped_nodes = s;
                // Remove the candidate from the cache (hash map).
                self.cache.remove(&Arc::clone(&kh.key));
            }
        };

        // Move the skipped nodes to the back of the deque. We do not unlink (drop)
        // them because ValueEntries in the write op queue should be pointing them.
        for node in skipped_nodes {
            unsafe { deqs.probation.move_to_back(node) };
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
    ) -> AdmissionResult<K> {
        const MAX_CONSECUTIVE_RETRIES: usize = 5;
        let mut retries = 0;

        let mut victims = EntrySizeAndFrequency::default();
        let mut victim_nodes = SmallVec::default();
        let mut skipped_nodes = SmallVec::default();

        // Get first potential victim at the LRU position.
        let mut next_victim = deqs.probation.peek_front();

        // Aggregate potential victims.
        while victims.policy_weight < candidate.policy_weight {
            if candidate.freq < victims.freq {
                break;
            }
            if let Some(victim) = next_victim.take() {
                next_victim = victim.next_node();

                if let Some(vic_entry) = cache.get(victim.element.key()) {
                    victims.add_policy_weight(vic_entry.policy_weight());
                    victims.add_frequency(freq, victim.element.hash());
                    victim_nodes.push(NonNull::from(victim));
                    retries = 0;
                } else {
                    // Could not get the victim from the cache (hash map). Skip this node
                    // as its ValueEntry might have been invalidated.
                    skipped_nodes.push(NonNull::from(victim));

                    retries += 1;
                    if retries > MAX_CONSECUTIVE_RETRIES {
                        break;
                    }
                }
            } else {
                // No more potential victims.
                break;
            }
        }

        // Admit or reject the candidate.

        // TODO: Implement some randomness to mitigate hash DoS attack.
        // See Caffeine's implementation.

        if victims.policy_weight >= candidate.policy_weight && candidate.freq > victims.freq {
            AdmissionResult::Admitted {
                victim_nodes,
                skipped_nodes,
            }
        } else {
            AdmissionResult::Rejected { skipped_nodes }
        }
    }

    fn handle_admit(
        &self,
        kh: KeyHash<K>,
        entry: &TrioArc<ValueEntry<K, V>>,
        policy_weight: u32,
        deqs: &mut Deques<K>,
        counters: &mut EvictionCounters,
    ) {
        let key = Arc::clone(&kh.key);
        counters.saturating_add(1, policy_weight);
        deqs.push_back_ao(
            CacheRegion::MainProbation,
            KeyHashDate::new(kh, entry.entry_info()),
            entry,
        );
        if self.is_write_order_queue_enabled() {
            deqs.push_back_wo(KeyDate::new(key, entry.entry_info()), entry);
        }
        entry.set_is_admitted(true);
    }

    fn handle_remove(
        deqs: &mut Deques<K>,
        entry: TrioArc<ValueEntry<K, V>>,
        counters: &mut EvictionCounters,
    ) {
        if entry.is_admitted() {
            entry.set_is_admitted(false);
            counters.saturating_sub(1, entry.policy_weight());
            // The following two unlink_* functions will unset the deq nodes.
            deqs.unlink_ao(&entry);
            Deques::unlink_wo(&mut deqs.write_order, &entry);
        } else {
            entry.unset_q_nodes();
        }
    }

    fn handle_remove_with_deques(
        ao_deq_name: &str,
        ao_deq: &mut Deque<KeyHashDate<K>>,
        wo_deq: &mut Deque<KeyDate<K>>,
        entry: TrioArc<ValueEntry<K, V>>,
        counters: &mut EvictionCounters,
    ) {
        if entry.is_admitted() {
            entry.set_is_admitted(false);
            counters.saturating_sub(1, entry.policy_weight());
            // The following two unlink_* functions will unset the deq nodes.
            Deques::unlink_ao_from_deque(ao_deq_name, ao_deq, &entry);
            Deques::unlink_wo(wo_deq, &entry);
        } else {
            entry.unset_q_nodes();
        }
    }

    fn evict_expired(
        &self,
        deqs: &mut Deques<K>,
        batch_size: usize,
        counters: &mut EvictionCounters,
    ) {
        let now = self.current_time_from_expiration_clock();

        if self.is_write_order_queue_enabled() {
            self.remove_expired_wo(deqs, batch_size, now, counters);
        }

        if self.time_to_idle.is_some() || self.has_valid_after() {
            let (window, probation, protected, wo) = (
                &mut deqs.window,
                &mut deqs.probation,
                &mut deqs.protected,
                &mut deqs.write_order,
            );

            let mut rm_expired_ao =
                |name, deq| self.remove_expired_ao(name, deq, wo, batch_size, now, counters);

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
        counters: &mut EvictionCounters,
    ) {
        let tti = &self.time_to_idle;
        let va = &self.valid_after();
        for _ in 0..batch_size {
            // Peek the front node of the deque and check if it is expired.
            let key = deq.peek_front().and_then(|node| {
                if is_expired_entry_ao(tti, va, &*node, now) {
                    Some(Arc::clone(node.element.key()))
                } else {
                    None
                }
            });

            if key.is_none() {
                break;
            }

            let key = key.as_ref().unwrap();

            // Remove the key from the map only when the entry is really
            // expired. This check is needed because it is possible that the entry in
            // the map has been updated or deleted but its deque node we checked
            // above have not been updated yet.
            let maybe_entry = self
                .cache
                .remove_if(key, |_, v| is_expired_entry_ao(tti, va, v, now));

            if let Some((_k, entry)) = maybe_entry {
                Self::handle_remove_with_deques(deq_name, deq, write_order_deq, entry, counters);
            } else if !self.try_skip_updated_entry(key, deq_name, deq, write_order_deq) {
                break;
            }
        }
    }

    #[inline]
    fn try_skip_updated_entry(
        &self,
        key: &K,
        deq_name: &str,
        deq: &mut Deque<KeyHashDate<K>>,
        write_order_deq: &mut Deque<KeyDate<K>>,
    ) -> bool {
        if let Some(entry) = self.cache.get(key) {
            if entry.last_accessed().is_none() {
                // The key exists and the entry has been updated.
                Deques::move_to_back_ao_in_deque(deq_name, deq, &entry);
                Deques::move_to_back_wo_in_deque(write_order_deq, &entry);
                true
            } else {
                // The key exists but something unexpected.
                false
            }
        } else {
            // Skip this entry as the key might have been invalidated. Since the
            // invalidated ValueEntry (which should be still in the write op
            // queue) has a pointer to this node, move the node to the back of
            // the deque instead of popping (dropping) it.
            if let Some(node) = deq.peek_front() {
                let node = NonNull::from(node);
                unsafe { deq.move_to_back(node) };
            }
            true
        }
    }

    #[inline]
    fn remove_expired_wo(
        &self,
        deqs: &mut Deques<K>,
        batch_size: usize,
        now: Instant,
        counters: &mut EvictionCounters,
    ) {
        let ttl = &self.time_to_live;
        let va = &self.valid_after();
        for _ in 0..batch_size {
            let key = deqs.write_order.peek_front().and_then(|node| {
                if is_expired_entry_wo(ttl, va, &*node, now) {
                    Some(Arc::clone(node.element.key()))
                } else {
                    None
                }
            });

            if key.is_none() {
                break;
            }

            let key = key.as_ref().unwrap();

            let maybe_entry = self
                .cache
                .remove_if(key, |_, v| is_expired_entry_wo(ttl, va, v, now));

            if let Some((_k, entry)) = maybe_entry {
                Self::handle_remove(deqs, entry, counters);
            } else if let Some(entry) = self.cache.get(key) {
                if entry.last_modified().is_none() {
                    deqs.move_to_back_ao(&entry);
                    deqs.move_to_back_wo(&entry);
                } else {
                    // The key exists but something unexpected. Break.
                    break;
                }
            } else {
                // Skip this entry as the key might have been invalidated. Since the
                // invalidated ValueEntry (which should be still in the write op
                // queue) has a pointer to this node, move the node to the back of
                // the deque instead of popping (dropping) it.
                if let Some(node) = deqs.write_order.peek_front() {
                    let node = NonNull::from(node);
                    unsafe { deqs.write_order.move_to_back(node) };
                }
            }
        }
    }

    fn evict_lru_entries(
        &self,
        deqs: &mut Deques<K>,
        batch_size: usize,
        weights_to_evict: u64,
        counters: &mut EvictionCounters,
    ) {
        const DEQ_NAME: &str = "probation";
        let mut evicted = 0u64;
        let (deq, write_order_deq) = (&mut deqs.probation, &mut deqs.write_order);

        for _ in 0..batch_size {
            if evicted >= weights_to_evict {
                break;
            }

            let maybe_key_and_ts = deq.peek_front().map(|node| {
                (
                    Arc::clone(node.element.key()),
                    node.element.entry_info().last_modified(),
                )
            });

            let (key, ts) = match maybe_key_and_ts {
                Some((key, Some(ts))) => (key, ts),
                Some((key, None)) => {
                    if self.try_skip_updated_entry(&key, DEQ_NAME, deq, write_order_deq) {
                        continue;
                    } else {
                        break;
                    }
                }
                None => break,
            };

            let maybe_entry = self.cache.remove_if(&key, |_, v| {
                if let Some(lm) = v.last_modified() {
                    lm == ts
                } else {
                    false
                }
            });

            if let Some((_k, entry)) = maybe_entry {
                let weight = entry.policy_weight();
                Self::handle_remove_with_deques(DEQ_NAME, deq, write_order_deq, entry, counters);
                evicted = evicted.saturating_add(weight as u64);
            } else if !self.try_skip_updated_entry(&key, DEQ_NAME, deq, write_order_deq) {
                break;
            }
        }
    }
}

//
// for testing
//
#[cfg(test)]
impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    fn set_expiration_clock(&self, clock: Option<Clock>) {
        let mut exp_clock = self.expiration_clock.write();
        if let Some(clock) = clock {
            *exp_clock = Some(clock);
            self.has_expiration_clock.store(true, Ordering::SeqCst);
        } else {
            self.has_expiration_clock.store(false, Ordering::SeqCst);
            *exp_clock = None;
        }
    }
}

//
// private free-standing functions
//
#[inline]
fn is_expired_entry_ao(
    time_to_idle: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_accessed() {
        if let Some(va) = valid_after {
            if ts < *va {
                return true;
            }
        }
        if let Some(tti) = time_to_idle {
            let checked_add = ts.checked_add(*tti);
            if checked_add.is_none() {
                panic!("ttl overflow")
            }
            return checked_add.unwrap() <= now;
        }
    }
    false
}

#[inline]
fn is_expired_entry_wo(
    time_to_live: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_modified() {
        if let Some(va) = valid_after {
            if ts < *va {
                return true;
            }
        }
        if let Some(ttl) = time_to_live {
            let checked_add = ts.checked_add(*ttl);
            if checked_add.is_none() {
                panic!("ttl overflow");
            }
            return checked_add.unwrap() <= now;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::BaseCache;

    #[cfg_attr(target_pointer_width = "16", ignore)]
    #[test]
    fn test_skt_capacity_will_not_overflow() {
        use std::collections::hash_map::RandomState;

        // power of two
        let pot = |exp| 2u64.pow(exp);

        let ensure_sketch_len = |max_capacity, len, name| {
            let cache = BaseCache::<u8, u8>::new(
                Some(max_capacity),
                None,
                RandomState::default(),
                None,
                None,
                None,
            );
            cache.inner.enable_frequency_sketch_for_testing();
            assert_eq!(
                cache.inner.frequency_sketch.read().table_len(),
                len as usize,
                "{}",
                name
            );
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
