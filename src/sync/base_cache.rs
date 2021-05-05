use super::{
    deques::Deques,
    housekeeper::{Housekeeper, InnerSync, SyncPace},
    invalidator::{
        GetOrRemoveEntry, InvalidationResult, Invalidator, KeyDateLite, PredicateFun,
        PredicateRegistrationError,
    },
    KeyDate, KeyHash, KeyHashDate, ReadOp, ValueEntry, WriteOp,
};
use crate::common::{
    deque::{CacheRegion, DeqNode, Deque},
    frequency_sketch::FrequencySketch,
    AccessTime,
};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::{Mutex, RwLock};
use quanta::{Clock, Instant};
use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    ptr::NonNull,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

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
            housekeeper: self.housekeeper.as_ref().map(|h| Arc::clone(&h)),
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
    S: BuildHasher + Clone + Send + 'static,
{
    pub(crate) fn new(
        max_capacity: usize,
        initial_capacity: Option<usize>,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let (r_snd, r_rcv) = crossbeam_channel::bounded(READ_LOG_SIZE);
        let (w_snd, w_rcv) = crossbeam_channel::bounded(WRITE_LOG_SIZE);
        let inner = Arc::new(Inner::new(
            max_capacity,
            initial_capacity,
            build_hasher,
            r_rcv,
            w_rcv,
            time_to_live,
            time_to_idle,
        ));
        *inner.invalidator.lock() = Some(Invalidator::new(Arc::downgrade(&Arc::clone(&inner))));
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
                let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), i.valid_after());
                let now = i.current_time_from_expiration_clock();

                if is_expired_entry_wo(ttl, va, &entry, now)
                    || is_expired_entry_ao(tti, va, &entry, now)
                {
                    // Expired entry. Record this access as a cache miss rather than a hit.
                    record(ReadOp::Miss(hash));
                    None
                } else {
                    // Valid entry.
                    let v = entry.value.clone();
                    record(ReadOp::Hit(hash, entry, now));
                    Some(v)
                }
            }
        }
    }

    #[inline]
    pub(crate) fn remove<Q>(&self, key: &Q) -> Option<Arc<ValueEntry<K, V>>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.remove(key)
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
    ) -> Result<u64, PredicateRegistrationError> {
        let now = self.inner.current_time_from_expiration_clock();
        self.inner.register_invalidation_predicate(predicate, now)
    }

    pub(crate) fn max_capacity(&self) -> usize {
        self.inner.max_capacity()
    }

    pub(crate) fn time_to_live(&self) -> Option<Duration> {
        self.inner.time_to_live()
    }

    pub(crate) fn time_to_idle(&self) -> Option<Duration> {
        self.inner.time_to_idle()
    }
}

//
// private methods
//
impl<K, V, S> BaseCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + 'static,
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
    pub(crate) fn do_insert_with_hash(&self, key: K, hash: u64, value: V) -> WriteOp<K, V> {
        let key = Arc::new(key);

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
            // on_insert
            || {
                let entry = Arc::new(ValueEntry::new(value.clone()));
                let cnt = op_cnt1.fetch_add(1, Ordering::Relaxed);
                op1 = Some((
                    cnt,
                    WriteOp::Upsert(KeyHash::new(Arc::clone(&key), hash), Arc::clone(&entry)),
                ));
                entry
            },
            // on_modify
            |_k, old_entry| {
                let entry = Arc::new(ValueEntry::new_with(value.clone(), old_entry));
                let cnt = op_cnt2.fetch_add(1, Ordering::Relaxed);
                op2 = Some((
                    cnt,
                    Arc::clone(&old_entry),
                    WriteOp::Upsert(KeyHash::new(Arc::clone(&key), hash), Arc::clone(&entry)),
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
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + 'static,
{
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }

    pub(crate) fn len(&self) -> usize {
        self.inner.len()
    }

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
    }

    pub(crate) fn set_expiration_clock(&self, clock: Option<quanta::Clock>) {
        self.inner.set_expiration_clock(clock);
    }
}

type CacheStore<K, V, S> = cht::SegmentedHashMap<Arc<K>, Arc<ValueEntry<K, V>>, S>;

pub(crate) struct Inner<K, V, S> {
    max_capacity: usize,
    cache: CacheStore<K, V, S>,
    build_hasher: S,
    deques: Mutex<Deques<K>>,
    frequency_sketch: RwLock<FrequencySketch>,
    read_op_ch: Receiver<ReadOp<K, V>>,
    write_op_ch: Receiver<WriteOp<K, V>>,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    valid_after: AtomicU64,
    invalidator: Mutex<Option<Invalidator<K, V, S>>>,
    has_expiration_clock: AtomicBool,
    expiration_clock: RwLock<Option<Clock>>,
}

// functions/methods used by BaseCache
impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    fn new(
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
            valid_after: AtomicU64::new(0),
            invalidator: Mutex::new(None),
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
    fn get<Q>(&self, key: &Q) -> Option<Arc<ValueEntry<K, V>>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cache.get(key)
    }

    #[inline]
    fn remove<Q>(&self, key: &Q) -> Option<Arc<ValueEntry<K, V>>>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cache.remove(key)
    }

    fn max_capacity(&self) -> usize {
        self.max_capacity
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
    fn valid_after(&self) -> Instant {
        let ts = self.valid_after.load(Ordering::Acquire);
        unsafe { std::mem::transmute(ts) }
    }

    #[inline]
    fn set_valid_after(&self, timestamp: Instant) {
        self.valid_after
            .store(timestamp.as_u64(), Ordering::Release);
    }

    #[inline]
    fn register_invalidation_predicate(
        &self,
        predicate: PredicateFun<K, V>,
        registered_at: Instant,
    ) -> Result<u64, PredicateRegistrationError> {
        if let Some(inv) = &*self.invalidator.lock() {
            inv.register_predicate(predicate, registered_at)
        } else {
            Err(PredicateRegistrationError::WriteOrderQueueDisabled)
        }
    }

    #[inline]
    fn has_valid_after(&self) -> bool {
        self.valid_after.load(Ordering::Acquire) > 0
    }

    #[inline]
    fn current_time_from_expiration_clock(&self) -> Instant {
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
}

impl<K, V, S> GetOrRemoveEntry<K, V> for Arc<Inner<K, V, S>>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    fn get_value_entry(&self, key: &Arc<K>) -> Option<Arc<ValueEntry<K, V>>> {
        self.cache.get(key)
    }

    fn remove_key_value_if<F>(&self, key: &Arc<K>, condition: F) -> Option<Arc<ValueEntry<K, V>>>
    where
        F: FnMut(&Arc<K>, &Arc<ValueEntry<K, V>>) -> bool,
    {
        self.cache.remove_if(key, condition)
    }
}

impl<K, V, S> InnerSync for Inner<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + 'static,
{
    fn sync(&self, max_repeats: usize) -> Option<SyncPace> {
        const EVICTION_BATCH_SIZE: usize = 500;
        const INVALIDATION_BATCH_SIZE: usize = 500;

        let mut deqs = self.deques.lock();
        let inv = self.invalidator.lock();
        let mut calls = 0;
        let mut should_sync = true;

        while should_sync && calls <= max_repeats {
            let r_len = self.read_op_ch.len();
            if r_len > 0 {
                self.apply_reads(&mut deqs, r_len);
            }

            let w_len = self.write_op_ch.len();
            if w_len > 0 {
                self.apply_writes(&mut deqs, w_len);
            }
            calls += 1;
            should_sync = self.read_op_ch.len() >= READ_LOG_FLUSH_POINT
                || self.write_op_ch.len() >= WRITE_LOG_FLUSH_POINT;
        }

        if self.has_expiry() || self.has_valid_after() {
            self.evict(&mut deqs, EVICTION_BATCH_SIZE);
        }

        if let Some(invalidator) = &*inv {
            if !invalidator.is_empty() && !invalidator.is_task_running() {
                self.invalidate_entries(invalidator, &mut deqs, INVALIDATION_BATCH_SIZE);
            }
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
}

//
// private methods
//
impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + 'static,
{
    fn apply_reads(&self, deqs: &mut Deques<K>, count: usize) {
        use ReadOp::*;
        let mut freq = self.frequency_sketch.write();
        let ch = &self.read_op_ch;
        for _ in 0..count {
            match ch.try_recv() {
                Ok(Hit(hash, mut entry, timestamp)) => {
                    freq.increment(hash);
                    entry.set_last_accessed(timestamp);
                    deqs.move_to_back_ao(&entry)
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
        let ts = self.current_time_from_expiration_clock();

        for _ in 0..count {
            match ch.try_recv() {
                Ok(Upsert(kh, entry)) => self.handle_upsert(kh, entry, ts, deqs, &freq),
                Ok(Remove(entry)) => Self::handle_remove(deqs, entry),
                Err(_) => break,
            };
        }
    }

    fn handle_upsert(
        &self,
        kh: KeyHash<K>,
        mut entry: Arc<ValueEntry<K, V>>,
        timestamp: Instant,
        deqs: &mut Deques<K>,
        freq: &FrequencySketch,
    ) {
        const MAX_RETRY: usize = 5;
        let mut tries = 0;
        let mut done = false;

        entry.set_last_accessed(timestamp);
        entry.set_last_modified(timestamp);
        let last_accessed = entry.raw_last_accessed();
        let last_modified = entry.raw_last_modified();

        while tries < MAX_RETRY {
            tries += 1;

            if entry.is_admitted() {
                // The entry has been already admitted, so treat this as an update.
                deqs.move_to_back_ao(&entry);
                deqs.move_to_back_wo(&entry);
                done = true;
                break;
            } else if self.cache.len() <= self.max_capacity {
                // There are some room in the cache. Add the candidate to the deques.
                self.handle_admit(kh.clone(), &entry, last_accessed, last_modified, deqs);
                done = true;
                break;
            } else {
                let victim = match Self::find_cache_victim(deqs, freq) {
                    // Found a victim.
                    Some(node) => node,
                    // Not found a victim. This condition should be unreachable
                    // because there was no room in the cache. But rather than
                    // panicking here, admit the candidate as there might be some
                    // room in te cache now.
                    None => {
                        self.handle_admit(kh.clone(), &entry, last_accessed, last_modified, deqs);
                        done = true;
                        break;
                    }
                };

                if Self::admit(kh.hash, victim, freq) {
                    // The candidate is admitted. Try to remove the victim from the
                    // cache (hash map).
                    if let Some(vic_entry) = self.cache.remove(&victim.element.key) {
                        // And then remove the victim from the deques.
                        Self::handle_remove(deqs, vic_entry);
                    } else {
                        // Could not remove the victim from the cache. Skip this
                        // victim node as its ValueEntry might have been
                        // invalidated. Since the invalidated ValueEntry (which
                        // should be still in the write op queue) has a pointer to
                        // this node, we move the node to the back of the deque
                        // instead of unlinking (dropping) it.
                        let victim = NonNull::from(victim);
                        unsafe { deqs.probation.move_to_back(victim) };

                        continue; // Retry
                    }
                    // Add the candidate to the deques.
                    self.handle_admit(
                        kh.clone(),
                        &entry,
                        Arc::clone(&last_accessed),
                        Arc::clone(&last_modified),
                        deqs,
                    );
                    done = true;
                    break;
                } else {
                    // The candidate is not admitted. Remove it from the cache (hash map).
                    self.cache.remove(&Arc::clone(&kh.key));
                    done = true;
                    break;
                }
            }
        }

        if !done {
            // Too mary retries. Remove the candidate from the cache.
            self.cache.remove(&Arc::clone(&kh.key));
        }
    }

    #[inline]
    fn find_cache_victim<'a>(
        deqs: &'a Deques<K>,
        _freq: &FrequencySketch,
    ) -> Option<&'a DeqNode<KeyHashDate<K>>> {
        // TODO: Check its frequency. If it is not very low, maybe we should
        // check frequencies of next few others and pick from them.
        deqs.probation.peek_front()
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

    fn handle_admit(
        &self,
        kh: KeyHash<K>,
        entry: &Arc<ValueEntry<K, V>>,
        raw_last_accessed: Arc<AtomicU64>,
        raw_last_modified: Arc<AtomicU64>,
        deqs: &mut Deques<K>,
    ) {
        let key = Arc::clone(&kh.key);
        deqs.push_back_ao(
            CacheRegion::MainProbation,
            KeyHashDate::new(kh, raw_last_accessed),
            entry,
        );
        if self.time_to_live.is_some() {
            deqs.push_back_wo(KeyDate::new(key, raw_last_modified), &entry);
        }
        entry.set_is_admitted(true);
    }

    fn handle_remove(deqs: &mut Deques<K>, entry: Arc<ValueEntry<K, V>>) {
        if entry.is_admitted() {
            entry.set_is_admitted(false);
            deqs.unlink_ao(&entry);
            Deques::unlink_wo(&mut deqs.write_order, &entry);
        }
        entry.unset_q_nodes();
    }

    fn handle_remove_with_deques(
        ao_deq_name: &str,
        ao_deq: &mut Deque<KeyHashDate<K>>,
        wo_deq: &mut Deque<KeyDate<K>>,
        entry: Arc<ValueEntry<K, V>>,
    ) {
        if entry.is_admitted() {
            entry.set_is_admitted(false);
            Deques::unlink_ao_from_deque(ao_deq_name, ao_deq, &entry);
            Deques::unlink_wo(wo_deq, &entry);
        }
        entry.unset_q_nodes();
    }

    fn evict(&self, deqs: &mut Deques<K>, batch_size: usize) {
        let now = self.current_time_from_expiration_clock();

        if self.time_to_live.is_some() {
            self.remove_expired_wo(deqs, batch_size, now);
        }

        if self.time_to_idle.is_some() || self.has_valid_after() {
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
        let tti = &self.time_to_idle;
        let va = self.valid_after();
        for _ in 0..batch_size {
            // Peek the front node of the deque and check if it is expired.
            let (key, _ts) = deq
                .peek_front()
                .and_then(|node| {
                    if is_expired_entry_ao(tti, va, &*node, now) {
                        Some((
                            Some(Arc::clone(&node.element.key)),
                            Some(&node.element.timestamp),
                        ))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

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

            if let Some(entry) = maybe_entry {
                Self::handle_remove_with_deques(deq_name, deq, write_order_deq, entry);
            } else if let Some(entry) = self.cache.get(key) {
                let ts = entry.last_accessed();
                if ts.is_none() {
                    // The key exists and the entry has been updated.
                    Deques::move_to_back_ao_in_deque(deq_name, deq, &entry);
                    Deques::move_to_back_wo_in_deque(write_order_deq, &entry);
                } else {
                    // The key exists but something unexpected. Break.
                    break;
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
            }
        }
    }

    #[inline]
    fn remove_expired_wo(&self, deqs: &mut Deques<K>, batch_size: usize, now: Instant) {
        let ttl = &self.time_to_live;
        let va = self.valid_after();
        for _ in 0..batch_size {
            let (key, _ts) = deqs
                .write_order
                .peek_front()
                .and_then(|node| {
                    if is_expired_entry_wo(ttl, va, &*node, now) {
                        Some((
                            Some(Arc::clone(&node.element.key)),
                            Some(&node.element.timestamp),
                        ))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            if key.is_none() {
                break;
            }

            let key = key.as_ref().unwrap();

            let maybe_entry = self
                .cache
                .remove_if(key, |_, v| is_expired_entry_wo(ttl, va, v, now));

            if let Some(entry) = maybe_entry {
                Self::handle_remove(deqs, entry);
            } else if let Some(entry) = self.cache.get(key) {
                let ts = entry.last_modified();
                if ts.is_none() {
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
    ) {
        self.process_invalidation_result(invalidator, deqs);
        self.submit_invalidation_task(invalidator, &mut deqs.write_order, batch_size);
    }

    fn process_invalidation_result(
        &self,
        invalidator: &Invalidator<K, V, S>,
        deqs: &mut Deques<K>,
    ) {
        if let Some(InvalidationResult {
            invalidated,
            is_done,
        }) = invalidator.task_result()
        {
            for entry in invalidated {
                Self::handle_remove(deqs, entry);
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
        let mut candidates = Vec::with_capacity(batch_size);
        let mut iter = write_order.peekable();
        let mut len = 0;

        while len < batch_size {
            if let Some(kd) = iter.next() {
                if let Some(ts) = kd.timestamp() {
                    candidates.push(KeyDateLite::new(&kd.key, ts));
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
    fn len(&self) -> usize {
        self.cache.len()
    }

    fn invalidation_predicate_count(&self) -> usize {
        self.invalidator
            .lock()
            .as_ref()
            .map(|inv| inv.predicate_count())
            .unwrap_or(0)
    }

    fn set_expiration_clock(&self, clock: Option<quanta::Clock>) {
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
    valid_after: Instant,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_accessed() {
        if ts < valid_after {
            return true;
        }
        if let Some(tti) = time_to_idle {
            return ts + *tti <= now;
        }
    }
    false
}

#[inline]
fn is_expired_entry_wo(
    time_to_live: &Option<Duration>,
    valid_after: Instant,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_modified() {
        if ts < valid_after {
            return true;
        }
        if let Some(ttl) = time_to_live {
            return ts + *ttl <= now;
        }
    }
    false
}
