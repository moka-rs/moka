use super::{
    deque::{CacheRegion, DeqNode, Deque},
    deques::Deques,
    frequency_sketch::FrequencySketch,
    housekeeper::{Housekeeper, InnerSync, SyncPace},
    AccessTime, KeyDate, KeyHash, KeyHashDate, ReadOp, ValueEntry, WriteOp,
};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::{Mutex, MutexGuard, RwLock};
use quanta::{Clock, Instant};
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

pub(crate) const MAX_SYNC_REPEATS: usize = 4;

const READ_LOG_FLUSH_POINT: usize = 512;
const READ_LOG_SIZE: usize = READ_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

const WRITE_LOG_FLUSH_POINT: usize = 512;
const WRITE_LOG_LOW_WATER_MARK: usize = WRITE_LOG_FLUSH_POINT / 2;
pub(crate) const WRITE_LOG_HIGH_WATER_MARK: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS - 1);
const WRITE_LOG_SIZE: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

pub(crate) const WRITE_THROTTLE_MICROS: u64 = 15;
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
        let housekeeper = Housekeeper::new(Arc::downgrade(&inner));

        Self {
            inner,
            read_op_ch: r_snd,
            write_op_ch: w_snd,
            housekeeper: Some(Arc::new(housekeeper)),
        }
    }

    pub(crate) fn get_with_hash<Q>(&self, key: &Q, hash: u64) -> Option<V>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let record = |entry, ts| {
            self.record_read_op(hash, entry, ts)
                .expect("Failed to record a get op");
        };

        match (self.inner.get(key), self.inner.has_expiry()) {
            // Value not found.
            (None, _) => {
                record(None, None);
                None
            }
            // Value found, no expiry.
            (Some(entry), false) => {
                let v = entry.value.clone();
                record(Some(entry), None);
                Some(v)
            }
            // Value found, need to check if expired.
            (Some(entry), true) => {
                let now = self.inner.current_time_from_expiration_clock();
                if self.inner.is_expired_entry_wo(&entry, now)
                    || self.inner.is_expired_entry_ao(&entry, now)
                {
                    // Expired entry. Record this access as a cache miss rather than a hit.
                    record(None, None);
                    None
                } else {
                    // Valid entry.
                    let v = entry.value.clone();
                    record(Some(entry), Some(now));
                    Some(v)
                }
            }
        }
    }

    #[inline]
    fn record_read_op(
        &self,
        hash: u64,
        entry: Option<Arc<ValueEntry<K, V>>>,
        timestamp: Option<Instant>,
    ) -> Result<(), TrySendError<ReadOp<K, V>>> {
        use ReadOp::*;
        self.apply_reads_if_needed();
        let ch = &self.read_op_ch;
        let op = if let Some(entry) = entry {
            Hit(hash, entry, timestamp)
        } else {
            Miss(hash)
        };
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
                let mut last_accessed = None;
                let mut last_modified = None;
                if self.inner.has_expiry() {
                    let ts = unsafe { std::mem::transmute(std::u64::MAX) };
                    if self.inner.time_to_idle.is_some() {
                        last_accessed = Some(ts);
                    }
                    if self.inner.time_to_live.is_some() {
                        last_modified = Some(ts);
                    }
                }
                let entry = Arc::new(ValueEntry::new(
                    value.clone(),
                    last_accessed,
                    last_modified,
                    None,
                    None,
                ));
                let cnt = op_cnt1.fetch_add(1, Ordering::Relaxed);
                op1 = Some((
                    cnt,
                    WriteOp::Insert(KeyHash::new(key, hash), Arc::clone(&entry)),
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
                    WriteOp::Update(Arc::clone(&entry)),
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

    #[inline]
    fn should_apply_reads(ch_len: usize) -> bool {
        ch_len >= READ_LOG_FLUSH_POINT
    }

    #[inline]
    fn should_apply_writes(ch_len: usize) -> bool {
        ch_len >= WRITE_LOG_FLUSH_POINT
    }
}

#[cfg(test)]
impl<K, V, S> BaseCache<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
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
    }

    pub(crate) fn set_expiration_clock(&self, clock: Option<quanta::Clock>) {
        let mut exp_clock = self.inner.expiration_clock.write();
        if let Some(clock) = clock {
            *exp_clock = Some(clock);
            self.inner
                .has_expiration_clock
                .store(true, Ordering::SeqCst);
        } else {
            self.inner
                .has_expiration_clock
                .store(false, Ordering::SeqCst);
            *exp_clock = None;
        }
    }
}

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
