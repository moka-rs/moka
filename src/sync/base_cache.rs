use super::{
    deques::Deques,
    entry_info::EntryInfo,
    housekeeper::{Housekeeper, InnerSync, SyncPace},
    invalidator::{GetOrRemoveEntry, InvalidationResult, Invalidator, KeyDateLite, PredicateFun},
    iter::ScanningGet,
    AccessTime, KeyDate, KeyHash, KeyHashDate, KvEntry, PredicateId, ReadOp, ValueEntry, Weigher,
    WriteOp,
};

#[cfg(feature = "unstable-debug-counters")]
use super::debug_counters::CacheDebugStats;

use crate::{
    common::{
        self,
        deque::{DeqNode, Deque},
        frequency_sketch::FrequencySketch,
        time::{atomic_time::AtomicInstant, CheckedTimeOps, Clock, Instant},
        CacheRegion,
    },
    Policy, PredicateError,
};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use crossbeam_utils::atomic::AtomicCell;
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    ptr::NonNull,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
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
// const WRITE_LOG_HIGH_WATER_MARK: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS - 1);
const WRITE_LOG_SIZE: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

pub(crate) const WRITE_RETRY_INTERVAL_MICROS: u64 = 50;

pub(crate) const PERIODICAL_SYNC_INITIAL_DELAY_MILLIS: u64 = 500;
pub(crate) const PERIODICAL_SYNC_NORMAL_PACE_MILLIS: u64 = 300;
pub(crate) const PERIODICAL_SYNC_FAST_PACE_NANOS: u64 = 500;

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

impl<K, V, S> BaseCache<K, V, S> {
    pub(crate) fn policy(&self) -> Policy {
        self.inner.policy()
    }

    #[cfg(feature = "unstable-debug-counters")]
    pub fn debug_stats(&self) -> CacheDebugStats {
        self.inner.debug_stats()
    }

    // #[cfg(test)]
    pub(crate) fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    // #[cfg(test)]
    pub(crate) fn weighted_size(&self) -> u64 {
        self.inner.weighted_size()
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
        invalidator_enabled: bool,
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
            invalidator_enabled,
        ));
        if invalidator_enabled {
            inner.set_invalidator(&inner);
        }
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

    pub(crate) fn contains_key_with_hash<Q>(&self, key: &Q, hash: u64) -> bool
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner
            .get_key_value_and(key, hash, |k, entry| {
                let i = &self.inner;
                let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), &i.valid_after());
                let now = i.current_time_from_expiration_clock();

                !is_expired_entry_wo(ttl, va, entry, now)
                    && !is_expired_entry_ao(tti, va, entry, now)
                    && !i.is_invalidated_entry(k, entry)
            })
            .unwrap_or_default() // `false` is the default for `bool` type.
    }

    pub(crate) fn get_with_hash<Q>(&self, key: &Q, hash: u64) -> Option<V>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        // Define a closure to record a read op.
        let record = |op| {
            self.record_read_op(op).expect("Failed to record a get op");
        };

        let maybe_entry = self.inner.get_key_value_and_then(key, hash, |k, entry| {
            let i = &self.inner;
            let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), &i.valid_after());
            let now = i.current_time_from_expiration_clock();

            if is_expired_entry_wo(ttl, va, entry, now)
                || is_expired_entry_ao(tti, va, entry, now)
                || i.is_invalidated_entry(k, entry)
            {
                // Expired or invalidated entry.
                None
            } else {
                // Valid entry.
                Some((TrioArc::clone(entry), now))
            }
        });

        if let Some((entry, now)) = maybe_entry {
            let v = entry.value.clone();
            record(ReadOp::Hit(hash, entry, now));
            Some(v)
        } else {
            record(ReadOp::Miss(hash));
            None
        }
    }

    #[inline]
    pub(crate) fn remove_entry<Q>(&self, key: &Q, hash: u64) -> Option<KvEntry<K, V>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.remove_entry(key, hash)
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

    pub(crate) fn invalidate_entries_if(
        &self,
        predicate: PredicateFun<K, V>,
    ) -> Result<PredicateId, PredicateError> {
        let now = self.inner.current_time_from_expiration_clock();
        self.inner.register_invalidation_predicate(predicate, now)
    }
}

//
// Iterator support
//
impl<K, V, S> ScanningGet<K, V> for BaseCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn num_cht_segments(&self) -> usize {
        self.inner.num_cht_segments()
    }

    fn scanning_get(&self, key: &Arc<K>) -> Option<V> {
        let hash = self.hash(key);
        self.inner.get_key_value_and_then(key, hash, |k, entry| {
            let i = &self.inner;
            let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), &i.valid_after());
            let now = i.current_time_from_expiration_clock();

            if is_expired_entry_wo(ttl, va, entry, now)
                || is_expired_entry_ao(tti, va, entry, now)
                || i.is_invalidated_entry(k, entry)
            {
                // Expired or invalidated entry.
                None
            } else {
                // Valid entry.
                Some(entry.value.clone())
            }
        })
    }

    fn keys(&self, cht_segment: usize) -> Option<Vec<Arc<K>>> {
        self.inner.keys(cht_segment)
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
        let op_cnt1 = Rc::new(AtomicU8::new(0));
        let op_cnt2 = Rc::clone(&op_cnt1);
        let mut op1 = None;
        let mut op2 = None;

        // Since the cache (cht::SegmentedHashMap) employs optimistic locking
        // strategy, insert_with_or_modify() may get an insert/modify operation
        // conflicted with other concurrent hash table operations. In that case, it
        // has to retry the insertion or modification, so on_insert and/or on_modify
        // closures can be executed more than once. In order to identify the last
        // call of these closures, we use a shared counter (op_cnt{1,2}) here to
        // record a serial number on a WriteOp, and consider the WriteOp with the
        // largest serial number is the one made by the last call of the closures.
        self.inner.cache.insert_with_or_modify(
            Arc::clone(&key),
            hash,
            // on_insert
            || {
                let entry = self.new_value_entry(value.clone(), weight);
                let cnt = op_cnt1.fetch_add(1, Ordering::Relaxed);
                op1 = Some((
                    cnt,
                    WriteOp::Upsert {
                        key_hash: KeyHash::new(Arc::clone(&key), hash),
                        value_entry: TrioArc::clone(&entry),
                        old_weight: 0,
                        new_weight: weight,
                    },
                ));
                entry
            },
            // on_modify
            |_k, old_entry| {
                // NOTES on `new_value_entry_from` method:
                // 1. The internal EntryInfo will be shared between the old and new ValueEntries.
                // 2. This method will set the last_accessed and last_modified to the max value to
                //    prevent this new ValueEntry from being evicted by an expiration policy.
                // 3. This method will update the policy_weight with the new weight.
                let old_weight = old_entry.policy_weight();
                let entry = self.new_value_entry_from(value.clone(), weight, old_entry);
                let cnt = op_cnt2.fetch_add(1, Ordering::Relaxed);
                op2 = Some((
                    cnt,
                    TrioArc::clone(old_entry),
                    WriteOp::Upsert {
                        key_hash: KeyHash::new(Arc::clone(&key), hash),
                        value_entry: TrioArc::clone(&entry),
                        old_weight,
                        new_weight: weight,
                    },
                ));
                entry
            },
        );

        match (op1, op2) {
            (Some((_cnt, ins_op)), None) => ins_op,
            (None, Some((_cnt, old_entry, upd_op))) => {
                old_entry.unset_q_nodes();
                upd_op
            }
            (Some((cnt1, ins_op)), Some((cnt2, old_entry, upd_op))) => {
                if cnt1 > cnt2 {
                    ins_op
                } else {
                    old_entry.unset_q_nodes();
                    upd_op
                }
            }
            (None, None) => unreachable!(),
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
    pub(crate) fn invalidation_predicate_count(&self) -> usize {
        self.inner.invalidation_predicate_count()
    }

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

type CacheStore<K, V, S> = crate::cht::SegmentedHashMap<Arc<K>, TrioArc<ValueEntry<K, V>>, S>;

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
    invalidator_enabled: bool,
    invalidator: RwLock<Option<Invalidator<K, V, S>>>,
    has_expiration_clock: AtomicBool,
    expiration_clock: RwLock<Option<Clock>>,
}

// functions/methods used by BaseCache
impl<K, V, S> Inner<K, V, S> {
    fn policy(&self) -> Policy {
        Policy::new(self.max_capacity, 1, self.time_to_live, self.time_to_idle)
    }

    #[inline]
    fn entry_count(&self) -> u64 {
        self.entry_count.load()
    }

    #[inline]
    pub(crate) fn weighted_size(&self) -> u64 {
        self.weighted_size.load()
    }

    #[cfg(feature = "unstable-debug-counters")]
    pub fn debug_stats(&self) -> CacheDebugStats {
        let ec = self.entry_count.load();
        let ws = self.weighted_size.load();

        CacheDebugStats::new(
            ec,
            ws,
            (self.cache.capacity() * 2) as u64,
            self.frequency_sketch.read().table_size(),
        )
    }
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
        invalidator_enabled: bool,
    ) -> Self {
        let initial_capacity = initial_capacity
            .map(|cap| cap + WRITE_LOG_SIZE * 4)
            .unwrap_or_default();
        const NUM_SEGMENTS: usize = 64;
        let cache = crate::cht::SegmentedHashMap::with_num_segments_capacity_and_hasher(
            NUM_SEGMENTS,
            initial_capacity,
            build_hasher.clone(),
        );

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
            invalidator_enabled,
            // When enabled, this field will be set later via the set_invalidator method.
            invalidator: RwLock::new(None),
            has_expiration_clock: AtomicBool::new(false),
            expiration_clock: RwLock::new(None),
        }
    }

    fn set_invalidator(&self, self_ref: &Arc<Self>) {
        *self.invalidator.write() = Some(Invalidator::new(Arc::downgrade(&Arc::clone(self_ref))));
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
    fn get_key_value_and<Q, F, T>(&self, key: &Q, hash: u64, with_entry: F) -> Option<T>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        F: FnOnce(&Arc<K>, &TrioArc<ValueEntry<K, V>>) -> T,
    {
        self.cache.get_key_value_and(key, hash, with_entry)
    }

    #[inline]
    fn get_key_value_and_then<Q, F, T>(&self, key: &Q, hash: u64, with_entry: F) -> Option<T>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        F: FnOnce(&Arc<K>, &TrioArc<ValueEntry<K, V>>) -> Option<T>,
    {
        self.cache.get_key_value_and_then(key, hash, with_entry)
    }

    #[inline]
    fn remove_entry<Q>(&self, key: &Q, hash: u64) -> Option<KvEntry<K, V>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cache
            .remove_entry(key, hash)
            .map(|(key, entry)| KvEntry::new(key, entry))
    }

    fn keys(&self, cht_segment: usize) -> Option<Vec<Arc<K>>> {
        // Do `Arc::clone` instead of `Arc::downgrade`. Updating existing entry
        // in the cht with a new value replaces the key in the cht even though the
        // old and new keys are equal. If we return `Weak<K>`, it will not be
        // upgraded later to `Arc<K> as the key may have been replaced with a new
        // key that equals to the old key.
        self.cache.keys(cht_segment, Arc::clone)
    }

    fn num_cht_segments(&self) -> usize {
        self.cache.actual_num_segments()
    }

    #[inline]
    fn time_to_live(&self) -> Option<Duration> {
        self.time_to_live
    }

    #[inline]
    fn time_to_idle(&self) -> Option<Duration> {
        self.time_to_idle
    }

    #[inline]
    fn has_expiry(&self) -> bool {
        self.time_to_live.is_some() || self.time_to_idle.is_some()
    }

    #[inline]
    fn is_write_order_queue_enabled(&self) -> bool {
        self.time_to_live.is_some() || self.invalidator_enabled
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
    fn register_invalidation_predicate(
        &self,
        predicate: PredicateFun<K, V>,
        registered_at: Instant,
    ) -> Result<PredicateId, PredicateError> {
        if let Some(inv) = &*self.invalidator.read() {
            inv.register_predicate(predicate, registered_at)
        } else {
            Err(PredicateError::InvalidationClosuresDisabled)
        }
    }

    #[inline]
    fn is_invalidated_entry(&self, key: &Arc<K>, entry: &TrioArc<ValueEntry<K, V>>) -> bool {
        if self.invalidator_enabled {
            if let Some(inv) = &*self.invalidator.read() {
                return inv.apply_predicates(key, entry);
            }
        }
        false
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

impl<K, V, S> GetOrRemoveEntry<K, V> for Arc<Inner<K, V, S>>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    fn get_value_entry(&self, key: &Arc<K>, hash: u64) -> Option<TrioArc<ValueEntry<K, V>>> {
        self.cache.get(key, hash)
    }

    fn remove_key_value_if<F>(
        &self,
        key: &Arc<K>,
        hash: u64,
        condition: F,
    ) -> Option<TrioArc<ValueEntry<K, V>>>
    where
        F: FnMut(&Arc<K>, &TrioArc<ValueEntry<K, V>>) -> bool,
    {
        self.cache.remove_if(key, hash, condition)
    }
}

#[cfg(feature = "unstable-debug-counters")]
mod batch_size {
    pub(crate) const EVICTION_BATCH_SIZE: usize = 10_000;
    pub(crate) const INVALIDATION_BATCH_SIZE: usize = 10_000;
}

#[cfg(not(feature = "unstable-debug-counters"))]
mod batch_size {
    pub(crate) const EVICTION_BATCH_SIZE: usize = 500;
    pub(crate) const INVALIDATION_BATCH_SIZE: usize = 500;
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

        if self.invalidator_enabled {
            if let Some(invalidator) = &*self.invalidator.read() {
                if !invalidator.is_empty() && !invalidator.is_task_running() {
                    self.invalidate_entries(
                        invalidator,
                        &mut deqs,
                        batch_size::INVALIDATION_BATCH_SIZE,
                        &mut counters,
                    );
                }
            }
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
                self.cache.remove(&Arc::clone(&kh.key), kh.hash);
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
                    let element = unsafe { &victim.as_ref().element };
                    if let Some((_vic_key, vic_entry)) =
                        self.cache.remove_entry(element.key(), element.hash())
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
                self.cache.remove(&Arc::clone(&kh.key), kh.hash);
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
                let vic_elem = &victim.element;

                if let Some(vic_entry) = cache.get(vic_elem.key(), vic_elem.hash()) {
                    victims.add_policy_weight(vic_entry.policy_weight());
                    victims.add_frequency(freq, vic_elem.hash());
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
            let key_hash = deq.peek_front().and_then(|node| {
                if is_expired_entry_ao(tti, va, &*node, now) {
                    Some((Arc::clone(node.element.key()), node.element.hash()))
                } else {
                    None
                }
            });

            if key_hash.is_none() {
                break;
            }

            let (key, hash) = key_hash.as_ref().map(|(k, h)| (k, *h)).unwrap();

            // Remove the key from the map only when the entry is really
            // expired. This check is needed because it is possible that the entry in
            // the map has been updated or deleted but its deque node we checked
            // above have not been updated yet.
            let maybe_entry = self
                .cache
                .remove_if(key, hash, |_, v| is_expired_entry_ao(tti, va, v, now));

            if let Some(entry) = maybe_entry {
                Self::handle_remove_with_deques(deq_name, deq, write_order_deq, entry, counters);
            } else if !self.try_skip_updated_entry(key, hash, deq_name, deq, write_order_deq) {
                break;
            }
        }
    }

    #[inline]
    fn try_skip_updated_entry(
        &self,
        key: &K,
        hash: u64,
        deq_name: &str,
        deq: &mut Deque<KeyHashDate<K>>,
        write_order_deq: &mut Deque<KeyDate<K>>,
    ) -> bool {
        if let Some(entry) = self.cache.get(key, hash) {
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
            let hash = self.hash(key);

            let maybe_entry = self
                .cache
                .remove_if(key, hash, |_, v| is_expired_entry_wo(ttl, va, v, now));

            if let Some(entry) = maybe_entry {
                Self::handle_remove(deqs, entry, counters);
            } else if let Some(entry) = self.cache.get(key, hash) {
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

    fn invalidate_entries(
        &self,
        invalidator: &Invalidator<K, V, S>,
        deqs: &mut Deques<K>,
        batch_size: usize,
        counters: &mut EvictionCounters,
    ) {
        self.process_invalidation_result(invalidator, deqs, counters);
        self.submit_invalidation_task(invalidator, &mut deqs.write_order, batch_size);
    }

    fn process_invalidation_result(
        &self,
        invalidator: &Invalidator<K, V, S>,
        deqs: &mut Deques<K>,
        counters: &mut EvictionCounters,
    ) {
        if let Some(InvalidationResult {
            invalidated,
            is_done,
        }) = invalidator.task_result()
        {
            for KvEntry { key: _, entry } in invalidated {
                Self::handle_remove(deqs, entry, counters);
            }
            if is_done {
                deqs.write_order.reset_cursor();
            }
        }
    }

    fn submit_invalidation_task(
        &self,
        invalidator: &Invalidator<K, V, S>,
        write_order: &mut Deque<KeyDate<K>>,
        batch_size: usize,
    ) {
        let now = self.current_time_from_expiration_clock();

        // If the write order queue is empty, we are done and can remove the predicates
        // that have been registered by now.
        if write_order.len() == 0 {
            invalidator.remove_predicates_registered_before(now);
            return;
        }

        let mut candidates = Vec::with_capacity(batch_size);
        let mut iter = write_order.peekable();
        let mut len = 0;

        while len < batch_size {
            if let Some(kd) = iter.next() {
                if let Some(ts) = kd.last_modified() {
                    let key = &kd.key;
                    let hash = self.hash(key);
                    candidates.push(KeyDateLite::new(key, hash, ts));
                    len += 1;
                }
            } else {
                break;
            }
        }

        if len > 0 {
            let is_truncated = len == batch_size && iter.peek().is_some();
            invalidator.submit_task(candidates, is_truncated);
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

            let maybe_key_hash_ts = deq.peek_front().map(|node| {
                (
                    Arc::clone(node.element.key()),
                    node.element.hash(),
                    node.element.entry_info().last_modified(),
                )
            });

            let (key, hash, ts) = match maybe_key_hash_ts {
                Some((key, hash, Some(ts))) => (key, hash, ts),
                Some((key, hash, None)) => {
                    if self.try_skip_updated_entry(&key, hash, DEQ_NAME, deq, write_order_deq) {
                        continue;
                    } else {
                        break;
                    }
                }
                None => break,
            };

            let maybe_entry = self.cache.remove_if(&key, hash, |_, v| {
                if let Some(lm) = v.last_modified() {
                    lm == ts
                } else {
                    false
                }
            });

            if let Some(entry) = maybe_entry {
                let weight = entry.policy_weight();
                Self::handle_remove_with_deques(DEQ_NAME, deq, write_order_deq, entry, counters);
                evicted = evicted.saturating_add(weight as u64);
            } else if !self.try_skip_updated_entry(&key, hash, DEQ_NAME, deq, write_order_deq) {
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
    fn invalidation_predicate_count(&self) -> usize {
        self.invalidator
            .read()
            .as_ref()
            .map(|inv| inv.predicate_count())
            .unwrap_or(0)
    }

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
                false,
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

            // The following tests will allocate large memory (~8GiB).
            // Skip when running on Circle CI.
            if !cfg!(circleci) {
                // due to ceiling to next_power_of_two
                ensure_sketch_len(pot30 - 1, pot30, "pot30- 1");
                ensure_sketch_len(pot30, pot30, "pot30");
                ensure_sketch_len(u64::MAX, pot30, "u64::MAX");
            }
        };
    }
}
