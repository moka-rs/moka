use crate::{
    deque::{CacheRegion, DeqNode, Deque},
    frequency_sketch::FrequencySketch,
    sync::{ConcurrentCache, ConcurrentCacheExt},
    thread_pool::{ThreadPool, ThreadPoolRegistry},
};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::{Mutex, MutexGuard, RwLock};
use quanta::{Clock, Instant};
use scheduled_thread_pool::JobHandle;
use std::{
    cell::UnsafeCell,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Weak,
    },
    time::Duration,
};

const MAX_SYNC_REPEATS: usize = 4;

const READ_LOG_FLUSH_POINT: usize = 512;
const READ_LOG_SIZE: usize = READ_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

const WRITE_LOG_FLUSH_POINT: usize = 512;
const WRITE_LOG_LOW_WATER_MARK: usize = WRITE_LOG_FLUSH_POINT / 2;
const WRITE_LOG_HIGH_WATER_MARK: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS - 1);
const WRITE_LOG_SIZE: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

const WRITE_THROTTLE_MICROS: u64 = 15;
const WRITE_RETRY_INTERVAL_MICROS: u64 = 50;

const PERIODICAL_SYNC_INITIAL_DELAY_MILLIS: u64 = 500;
const PERIODICAL_SYNC_NORMAL_PACE_MILLIS: u64 = 300;
const PERIODICAL_SYNC_FAST_PACE_NANOS: u64 = 500;

pub struct Cache<K, V, S = RandomState> {
    inner: Arc<Inner<K, V, S>>,
    read_op_ch: Sender<ReadOp<K, V>>,
    write_op_ch: Sender<WriteOp<K, V>>,
    housekeeper: Option<Arc<Housekeeper<K, V, S>>>,
}

impl<K, V, S> Drop for Cache<K, V, S> {
    fn drop(&mut self) {
        // The housekeeper needs to be dropped before the inner is dropped.
        std::mem::drop(self.housekeeper.take());
    }
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

impl<K, V, S> Clone for Cache<K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            read_op_ch: self.read_op_ch.clone(),
            write_op_ch: self.write_op_ch.clone(),
            housekeeper: self.housekeeper.as_ref().map(|h| Arc::clone(&h)),
        }
    }
}

impl<K, V> Cache<K, V, RandomState>
where
    K: Eq + Hash,
{
    pub fn new(capacity: usize) -> Self {
        let build_hasher = RandomState::default();
        Self::with_hasher(capacity, build_hasher)
    }
}

impl<K, V, S> Cache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    pub fn with_hasher(capacity: usize, build_hasher: S) -> Self {
        Self::with_everything(capacity, build_hasher, None, None)
    }

    // TODO: Instead of taking the capacity as an argument, take the followings:
    // - initial_capacity of the cache (hashmap)
    // - max_capacity of the cache (hashmap)
    // - estimated_max_unique_keys (for the frequency sketch)
    pub(crate) fn with_everything(
        capacity: usize,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let (r_snd, r_rcv) = crossbeam_channel::bounded(READ_LOG_SIZE);
        let (w_snd, w_rcv) = crossbeam_channel::bounded(WRITE_LOG_SIZE);
        let inner = Arc::new(Inner::new(
            capacity,
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

    pub(crate) fn get_with_hash(&self, key: &K, hash: u64) -> Option<Arc<V>> {
        let record = |entry, ts| {
            self.record_read_op(hash, entry, ts)
                .expect("Failed to record a get op")
        };

        match (self.inner.get(key), self.inner.has_expiry()) {
            // Value not found.
            (None, _) => {
                record(None, None);
                None
            }
            // Value found, no expiry.
            (Some(entry), false) => {
                let v = Arc::clone(&entry.value);
                record(Some(entry), None);
                Some(v)
            }
            // Value found, need to check if expired.
            (Some(entry), true) => {
                let now = self.inner.current_time_from_expiration_clock();
                if self.inner.is_expired_entry(&entry, now) {
                    // Record this access as a cache miss rather than a hit.
                    record(None, None);
                    None
                } else {
                    let v = Arc::clone(&entry.value);
                    record(Some(entry), Some(now));
                    Some(v)
                }
            }
        }
    }

    pub(crate) fn insert_with_hash(&self, key: K, hash: u64, value: V) -> Arc<V> {
        self.throttle_write_pace();

        let key = Arc::new(key);
        let value = Arc::new(value);

        let op_cnt1 = Rc::new(AtomicU8::new(0));
        let op_cnt2 = Rc::clone(&op_cnt1);
        let mut op1 = None;
        let mut op2 = None;

        // Since the cache (cht::SegmentedHashMap) employs optimistic locking
        // strategy, insert_with_or_modify() may get an insert/modify operation
        // conflicted with other concurrent hash table operations. In that case,
        // it has to retry the insertion or modification, so on_insert and/or
        // on_modify closures can be executed more than once. In order to
        // identify the last call of these closures, we use a shared counter
        // (op_cnt{1,2}) here to record a serial number on a WriteOp, and
        // consider the WriteOp with the largest serial number is the one made
        // by the last call of the closures.
        self.inner.cache.insert_with_or_modify(
            Arc::clone(&key),
            // on_insert
            || {
                let entry = Arc::new(ValueEntry::new(Arc::clone(&value), None));
                let cnt = op_cnt1.fetch_add(1, Ordering::Relaxed);
                op1 = Some((cnt, WriteOp::Insert(KeyHash::new(key, hash), entry.clone())));
                entry
            },
            // on_modify
            |_k, old_entry| {
                // Clear the deq_node of the old_entry. This is needed as an Arc
                // pointer to the old (stale) entry might be in the read op queue.
                let deq_node = unsafe { (*old_entry.deq_node.get()).take() };
                // Create a new entry using the new value and the deq_node taken
                // from the old_entry.
                let entry = Arc::new(ValueEntry::new(Arc::clone(&value), deq_node));
                let cnt = op_cnt2.fetch_add(1, Ordering::Relaxed);
                op2 = Some((cnt, WriteOp::Update(entry.clone())));
                entry
            },
        );

        match (op1, op2) {
            (Some((_cnt, op)), None) => self.schedule_insert_op(op),
            (None, Some((_cnt, op))) => self.schedule_insert_op(op),
            (Some((cnt1, op1)), Some((cnt2, op2))) => {
                if cnt1 > cnt2 {
                    self.schedule_insert_op(op1)
                } else {
                    self.schedule_insert_op(op2)
                }
            }
            (None, None) => unreachable!(),
        }
        .expect("Failed to insert");

        value
    }
}

impl<K, V, S> ConcurrentCache<K, V> for Cache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn get(&self, key: &K) -> Option<Arc<V>> {
        self.get_with_hash(key, self.inner.hash(key))
    }

    // fn get_or_insert(&self, _key: K, _default: V) -> Arc<V> {
    //     todo!()
    // }

    // fn get_or_insert_with<F>(&self, _key: K, _default: F) -> Arc<V>
    // where
    //     F: FnOnce() -> V,
    // {
    //     todo!()
    // }

    fn insert(&self, key: K, value: V) -> Arc<V> {
        let hash = self.inner.hash(&key);
        self.insert_with_hash(key, hash, value)
    }

    fn remove(&self, key: &K) -> Option<Arc<V>> {
        self.throttle_write_pace();
        self.inner.cache.remove(key).map(|entry| {
            let value = Arc::clone(&entry.value);
            self.schedule_remove_op(entry).expect("Failed to remove");
            value
        })
    }

    fn capacity(&self) -> usize {
        self.inner.capacity
    }

    fn time_to_live(&self) -> Option<Duration> {
        self.inner.time_to_live
    }

    fn time_to_idle(&self) -> Option<Duration> {
        self.inner.time_to_idle
    }

    fn num_segments(&self) -> usize {
        1
    }
}

impl<K, V, S> ConcurrentCacheExt<K, V> for Cache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn sync(&self) {
        self.inner.sync(MAX_SYNC_REPEATS);
    }
}

// private methods
impl<K, V, S> Cache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
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
    fn schedule_insert_op(&self, op: WriteOp<K, V>) -> Result<(), TrySendError<WriteOp<K, V>>> {
        let ch = &self.write_op_ch;
        let mut op = op;

        // NOTES:
        // - This will block when the channel is full.
        // - We are doing a busy-loop here. We were originally calling `ch.send(op)?`,
        //   but we got a notable performance degradation.
        loop {
            self.apply_reads_writes_if_needed();
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

    #[inline]
    fn schedule_remove_op(
        &self,
        entry: Arc<ValueEntry<K, V>>,
    ) -> Result<(), TrySendError<WriteOp<K, V>>> {
        let ch = &self.write_op_ch;
        let mut op = WriteOp::Remove(entry);

        // NOTES:
        // - This will block when the channel is full.
        // - For the reason why we are doing a busy-loop here, the comments in
        //   `schedule_insert_op()`.
        loop {
            self.apply_reads_writes_if_needed();
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

    #[inline]
    fn apply_reads_if_needed(&self) {
        let len = self.read_op_ch.len();

        if self.should_apply_reads(len) {
            // if let Some(ref mut deqs) = self.inner.deques.try_lock() {
            //     self.inner.apply_reads(deqs, len);
            // }
            if let Some(h) = &self.housekeeper {
                h.try_schedule_sync();
            }
        }
    }

    #[inline]
    fn apply_reads_writes_if_needed(&self) {
        let w_len = self.write_op_ch.len();

        if self.should_apply_writes(w_len) {
            // let r_len = self.read_op_ch.len();
            // if let Some(ref mut deqs) = self.inner.deques.try_lock() {
            //     self.inner.apply_reads(deqs, r_len);
            //     self.inner.apply_writes(deqs, w_len);
            // }
            if let Some(h) = &self.housekeeper {
                h.try_schedule_sync();
            }
        }
    }

    #[inline]
    fn should_apply_reads(&self, ch_len: usize) -> bool {
        ch_len >= READ_LOG_FLUSH_POINT
    }

    #[inline]
    fn should_apply_writes(&self, ch_len: usize) -> bool {
        ch_len >= WRITE_LOG_FLUSH_POINT
    }

    #[inline]
    fn throttle_write_pace(&self) {
        if self.write_op_ch.len() >= WRITE_LOG_HIGH_WATER_MARK {
            std::thread::sleep(Duration::from_micros(WRITE_THROTTLE_MICROS))
        }
    }
}

// For unit tests.
#[cfg(test)]
impl<K, V, S> Cache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn reconfigure_for_testing(&mut self) {
        // Stop the housekeeping job that may cause sync() method to return earlier.
        if let Some(housekeeper) = &self.housekeeper {
            let mut job = housekeeper.periodical_sync_job.lock();
            if let Some(job) = job.take() {
                job.cancel();
            }
        }
    }

    fn set_expiration_clock(&self, clock: Option<Clock>) {
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

struct KeyHash<K> {
    key: Arc<K>,
    hash: u64,
}

impl<K> KeyHash<K> {
    fn new(key: Arc<K>, hash: u64) -> Self {
        Self { key, hash }
    }
}

enum ReadOp<K, V> {
    Hit(u64, Arc<ValueEntry<K, V>>, Option<Instant>),
    Miss(u64),
}

enum WriteOp<K, V> {
    Insert(KeyHash<K>, Arc<ValueEntry<K, V>>),
    Update(Arc<ValueEntry<K, V>>),
    Remove(Arc<ValueEntry<K, V>>),
}

trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    // fn last_modified(&self) -> Option<Instant>;
    fn set_last_accessed(&mut self, timestamp: Instant);
}

type KeyDeqNode<K> = Option<NonNull<DeqNode<KeyHashDate<K>>>>;

struct ValueEntry<K, V> {
    value: Arc<V>,
    deq_node: UnsafeCell<KeyDeqNode<K>>,
}

impl<K, V> ValueEntry<K, V> {
    fn new(value: Arc<V>, deq_node: KeyDeqNode<K>) -> Self {
        Self {
            value,
            deq_node: UnsafeCell::new(deq_node),
        }
    }
}

impl<K, V> AccessTime for Arc<ValueEntry<K, V>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        unsafe { (*self.deq_node.get()).map(|node| node.as_ref().element.timestamp) }
    }

    // #[inline]
    // fn last_modified(&self) -> Option<Instant> {
    //     todo!()
    // }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        unsafe {
            if let Some(mut node) = *self.deq_node.get() {
                node.as_mut().element.timestamp = timestamp;
            }
        }
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        Some(self.element.timestamp)
    }

    // #[inline]
    // fn last_modified(&self) -> Option<Instant> {
    //     todo!()
    // }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.element.timestamp = timestamp;
    }
}

type CacheStore<K, V, S> = cht::SegmentedHashMap<Arc<K>, Arc<ValueEntry<K, V>>, S>;

struct Inner<K, V, S> {
    capacity: usize,
    cache: CacheStore<K, V, S>,
    build_hasher: S,
    deques: Mutex<Deques<K>>,
    frequency_sketch: RwLock<FrequencySketch>,
    read_op_ch: Receiver<ReadOp<K, V>>,
    write_op_ch: Receiver<WriteOp<K, V>>,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    has_expiration_clock: AtomicBool,
    expiration_clock: RwLock<Option<Clock>>,
}

// functions/methods used by Cache
impl<K, V, S> Inner<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn new(
        capacity: usize,
        build_hasher: S,
        read_op_ch: Receiver<ReadOp<K, V>>,
        write_op_ch: Receiver<WriteOp<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        // TODO: Make this much smaller.
        let initial_capacity = ((capacity as f64) * 1.4) as usize;
        let num_segments = 64;
        let cache = cht::SegmentedHashMap::with_num_segments_capacity_and_hasher(
            num_segments,
            initial_capacity,
            build_hasher.clone(),
        );
        let skt_capacity = usize::max(capacity * 32, 100);
        let frequency_sketch = FrequencySketch::with_capacity(skt_capacity);
        Self {
            capacity,
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
    fn hash(&self, key: &K) -> u64 {
        let mut hasher = self.build_hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[inline]
    fn get(&self, key: &K) -> Option<Arc<ValueEntry<K, V>>> {
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
                    deqs.move_to_back(entry)
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
                    }
                    deqs.move_to_back(entry)
                }
                Ok(Remove(entry)) => deqs.unlink(entry),
                Err(_) => break,
            };
        }
    }

    fn evict(&self, deqs: &mut Deques<K>, batch_size: usize) {
        debug_assert!(self.has_expiry());

        let now = self.current_time_from_expiration_clock();
        let mut expired_entries = Vec::default();

        self.find_expired(&mut deqs.window, &mut expired_entries, batch_size, now); //    Empty yet.
        self.find_expired(&mut deqs.probation, &mut expired_entries, batch_size, now);
        self.find_expired(&mut deqs.protected, &mut expired_entries, batch_size, now);
        // Empty yet.

        // for entry in expired_entries {
        //     deqs.unlink(entry);
        // }
    }

    #[inline]
    fn find_expired(
        &self,
        deq: &mut Deque<KeyHashDate<K>>,
        _expired_entries: &mut Vec<Arc<ValueEntry<K, V>>>,
        batch_size: usize,
        now: Instant,
    ) {
        for _ in 0..batch_size {
            let is_expired = deq
                .peek_front()
                .map(|node| self.is_expired_entry(node, now))
                .unwrap_or(false);
            if is_expired {
                if let Some(node) = deq.pop_front() {
                    if let Some(_vic_entry) = self.cache.remove(&node.element.key) {
                        // expired_entries.push(vic_entry);
                    }
                }
            } else {
                break;
            }
        }
    }

    fn sync(&self, max_repeats: usize) -> Option<SyncPace> {
        if self.read_op_ch.is_empty() && self.write_op_ch.is_empty() && !self.has_expiry() {
            return None;
        }

        let deqs = self.deques.lock();
        self.do_sync(deqs, max_repeats)
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

    #[inline]
    fn has_expiry(&self) -> bool {
        self.time_to_live.is_some() || self.time_to_idle.is_some()
    }

    #[inline]
    fn is_expired_entry(&self, entry: &impl AccessTime, now: Instant) -> bool {
        debug_assert!(self.has_expiry());

        // Time-to-idle
        if let (Some(ts), Some(tti)) = (entry.last_accessed(), self.time_to_idle) {
            if ts + tti <= now {
                return true;
            }
        }

        // Time-to-live
        // if let (Some(ts), Some(ttl)) = (entry.last_modified(), self.time_to_live) {
        //     if ts + ttl <= now {
        //         return true;
        //     }
        // }

        // Not expired
        false
    }
}

// private methods
impl<K, V, S> Inner<K, V, S>
where
    K: Eq + Hash,
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
        if self.cache.len() <= self.capacity {
            // Add the candidate to the deque.
            deqs.push_back(
                CacheRegion::MainProbation,
                KeyHashDate::new(kh, timestamp),
                &entry,
            );
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
                    deqs.unlink(vic_entry);
                } else {
                    let victim = NonNull::from(victim);
                    deqs.unlink_node(victim)
                }
                // Add the candidate to the deque.
                deqs.push_back(
                    CacheRegion::MainProbation,
                    KeyHashDate::new(kh, timestamp),
                    &entry,
                );
            } else {
                // Remove the candidate from the cache.
                self.cache.remove(&kh.key);
            }
        }
    }
}

struct KeyHashDate<K> {
    key: Arc<K>,
    hash: u64,
    // Optional. Instant(0u64) for None.
    timestamp: Instant,
}

impl<K> KeyHashDate<K> {
    fn new(kh: KeyHash<K>, timestamp: Option<Instant>) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            timestamp: timestamp.unwrap_or_else(|| unsafe { std::mem::transmute(0u64) }),
        }
    }
}

struct Deques<K> {
    window: Deque<KeyHashDate<K>>, //    Not used yet.
    probation: Deque<KeyHashDate<K>>,
    protected: Deque<KeyHashDate<K>>, // Not used yet.
}

impl<K> Default for Deques<K> {
    fn default() -> Self {
        Self {
            window: Deque::new(CacheRegion::Window),
            probation: Deque::new(CacheRegion::MainProbation),
            protected: Deque::new(CacheRegion::MainProtected),
        }
    }
}

impl<K> Deques<K> {
    fn push_back<V>(
        &mut self,
        region: CacheRegion,
        kh: KeyHashDate<K>,
        entry: &Arc<ValueEntry<K, V>>,
    ) {
        use CacheRegion::*;
        let node = Box::new(DeqNode::new(region, kh));
        let node = match node.as_ref().region {
            Window => self.window.push_back(node),
            MainProbation => self.probation.push_back(node),
            MainProtected => self.protected.push_back(node),
        };
        unsafe { *(entry.deq_node.get()) = Some(node) };
    }

    fn move_to_back<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        use CacheRegion::*;
        unsafe {
            if let Some(node) = *entry.deq_node.get() {
                let p = node.as_ref();
                match &p.region {
                    Window if self.window.contains(p) => self.window.move_to_back(node),
                    MainProbation if self.probation.contains(p) => {
                        self.probation.move_to_back(node)
                    }
                    MainProtected if self.protected.contains(p) => {
                        self.protected.move_to_back(node)
                    }
                    region => eprintln!(
                        "move_to_back - node is not a member of {:?} deque. {:?}",
                        region, p
                    ),
                }
            }
        }
    }

    fn unlink<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        unsafe {
            if let Some(node) = (*entry.deq_node.get()).take() {
                self.unlink_node(node);
            }
        }
    }

    fn unlink_node(&mut self, node: NonNull<DeqNode<KeyHashDate<K>>>) {
        use CacheRegion::*;
        unsafe {
            let p = node.as_ref();
            match &p.region {
                Window if self.window.contains(p) => self.window.unlink(node),
                MainProbation if self.probation.contains(p) => self.probation.unlink(node),
                MainProtected if self.protected.contains(p) => self.protected.unlink(node),
                region => eprintln!(
                    "unlink_node - node is not a member of {:?} deque. {:?}",
                    region, p
                ),
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum SyncPace {
    Normal,
    Fast,
}

impl SyncPace {
    fn make_duration(&self) -> Duration {
        use SyncPace::*;
        match self {
            Normal => Duration::from_millis(PERIODICAL_SYNC_NORMAL_PACE_MILLIS),
            Fast => Duration::from_nanos(PERIODICAL_SYNC_FAST_PACE_NANOS),
        }
    }
}

struct Housekeeper<K, V, S> {
    inner: Arc<Mutex<UnsafeWeakPointer>>,
    thread_pool: Arc<ThreadPool>,
    is_shutting_down: Arc<AtomicBool>,
    periodical_sync_job: Mutex<Option<JobHandle>>,
    periodical_sync_running: Arc<Mutex<()>>,
    on_demand_sync_scheduled: Arc<AtomicBool>,
    _marker: PhantomData<(K, V, S)>,
}

impl<K, V, S> Drop for Housekeeper<K, V, S> {
    fn drop(&mut self) {
        // Disallow to create and/or run sync jobs by now.
        self.is_shutting_down.store(true, Ordering::Release);

        // Cancel the periodical sync job. (This will not abort the job if it is
        // already running)
        if let Some(j) = self.periodical_sync_job.lock().take() {
            j.cancel()
        }

        // Wait for the periodical sync job to finish.
        let _ = self.periodical_sync_running.lock();

        // Wait for the on-demand sync job to finish. (busy loop)
        while self.on_demand_sync_scheduled.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }

        // All sync jobs should have been finished by now. Clean other stuff up.
        ThreadPoolRegistry::release_pool(&self.thread_pool);
        std::mem::drop(unsafe { self.inner.lock().as_weak_arc::<K, V, S>() });
    }
}

// functions/methods used by Cache
impl<K, V, S> Housekeeper<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn new(inner: Weak<Inner<K, V, S>>) -> Self {
        let thread_pool = ThreadPoolRegistry::acquire_default_pool();

        let inner_ptr = Arc::new(Mutex::new(UnsafeWeakPointer::from_weak_arc(inner)));
        let is_shutting_down = Arc::new(AtomicBool::new(false));
        let periodical_sync_running = Arc::new(Mutex::new(()));
        let periodical_sync_pace = Arc::new(Mutex::new(SyncPace::Normal));

        let housekeeper_closure = {
            // The following Arc clones will be moved into the housekeeper closure.
            let unsafe_weak_ptr = Arc::clone(&inner_ptr);
            let shutting_down = Arc::clone(&is_shutting_down);
            let sync_running = Arc::clone(&periodical_sync_running);
            let sync_pace = Arc::clone(&periodical_sync_pace);

            move || {
                // To avoid dead-locking with other thread, acquire the lock at very beginning.
                let mut sync_pace = sync_pace.lock();
                if !shutting_down.load(Ordering::Acquire) {
                    let _lock = sync_running.lock();
                    if let Some(new_pace) = Self::call_sync(&unsafe_weak_ptr) {
                        if *sync_pace != new_pace {
                            *sync_pace = new_pace
                        }
                    }
                }
                // TODO: Check what would happen if the closure returns None.
                Some(sync_pace.make_duration())
            }
        };

        let initial_delay = Duration::from_millis(PERIODICAL_SYNC_INITIAL_DELAY_MILLIS);

        // Execute a task in a worker thread.
        let job = thread_pool
            .pool
            .execute_with_dynamic_delay(initial_delay, housekeeper_closure);

        Self {
            inner: inner_ptr,
            thread_pool,
            is_shutting_down,
            periodical_sync_job: Mutex::new(Some(job)),
            periodical_sync_running,
            on_demand_sync_scheduled: Arc::new(AtomicBool::new(false)),
            _marker: PhantomData::default(),
        }
    }

    fn try_schedule_sync(&self) -> bool {
        // TODO: Check if these `Orderings` are correct.

        // If shutting down, do not schedule the task.
        if self.is_shutting_down.load(Ordering::Acquire) {
            return false;
        }

        // Try to flip the value of sync_scheduled from false to true.
        match self.on_demand_sync_scheduled.compare_exchange(
            false,
            true,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                let unsafe_weak_ptr = Arc::clone(&self.inner);
                let sync_scheduled = Arc::clone(&self.on_demand_sync_scheduled);
                // Execute a task in a worker thread.
                self.thread_pool.pool.execute(move || {
                    Self::call_sync(&unsafe_weak_ptr);
                    sync_scheduled.store(false, Ordering::Release);
                });
                true
            }
            Err(_) => false,
        }
    }
}

// private functions/methods
impl<K, V, S> Housekeeper<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn call_sync(unsafe_weak_ptr: &Arc<Mutex<UnsafeWeakPointer>>) -> Option<SyncPace> {
        let lock = unsafe_weak_ptr.lock();
        // Restore the Weak pointer to Inner<K, V, S>.
        let weak = unsafe { lock.as_weak_arc::<K, V, S>() };
        if let Some(inner) = weak.upgrade() {
            // TODO: Protect this call with catch_unwind().
            let sync_pace = inner.sync(MAX_SYNC_REPEATS);
            // Avoid to drop the Arc<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_arc(inner);
            sync_pace
        } else {
            // Avoid to drop the Weak<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_weak_arc(weak);
            None
        }
    }
}

/// WARNING: Do not use this struct unless you are absolutely sure
/// what you are doing. Using this struct is unsafe and may cause
/// memory related crashes and/or security vulnerabilities.
///
/// This struct exists with the sole purpose of avoiding compile
/// errors relevant to the thread pool usages. The thread pool
/// requires that the generic parameters on the `Cache` and `Inner`
/// structs to have trait bounds `Send`, `Sync` and `'static`. This
/// will be unacceptable for many cache usages.
///
/// This struct avoids the trait bounds by transmuting a pointer
/// between `std::sync::Weak<Inner<K, V, S>>` and `usize`.
///
/// If you know a better solution than this, we would love te hear it.
struct UnsafeWeakPointer {
    // This is a std::sync::Weak pointer to Inner<K, V, S>.
    raw_ptr: usize,
}

impl UnsafeWeakPointer {
    fn from_weak_arc<K, V, S>(p: Weak<Inner<K, V, S>>) -> Self {
        Self {
            raw_ptr: unsafe { std::mem::transmute(p) },
        }
    }

    unsafe fn as_weak_arc<K, V, S>(&self) -> Weak<Inner<K, V, S>> {
        std::mem::transmute(self.raw_ptr)
    }

    fn forget_arc<K, V, S>(p: Arc<Inner<K, V, S>>) {
        // Downgrade the Arc to Weak, then forget.
        let weak = Arc::downgrade(&p);
        std::mem::forget(weak);
    }

    fn forget_weak_arc<K, V, S>(p: Weak<Inner<K, V, S>>) {
        std::mem::forget(p);
    }
}

/// `clone()` simply creates a copy of the `raw_ptr`, effectively
/// creating many copies of the same `Weak` pointer. We are doing this
/// for a good reason for our use case.
///
/// When you want to drop the Weak pointer, ensure that you drop it
/// only once for the same `raw_ptr` across clones.
impl Clone for UnsafeWeakPointer {
    fn clone(&self) -> Self {
        Self {
            raw_ptr: self.raw_ptr,
        }
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{Cache, ConcurrentCache, ConcurrentCacheExt};
    use crate::sync::Builder;

    use quanta::Clock;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn basic_single_thread() {
        let mut cache = Cache::new(3);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        assert_eq!(cache.insert("a", "alice"), Arc::new("alice"));
        assert_eq!(cache.insert("b", "bob"), Arc::new("bob"));
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 1, b -> 1

        assert_eq!(cache.insert("c", "cindy"), Arc::new("cindy"));
        assert_eq!(cache.get(&"c"), Some(Arc::new("cindy")));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        assert_eq!(cache.insert("d", "david"), Arc::new("david")); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1

        assert_eq!(cache.insert("d", "david"), Arc::new("david"));
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        assert_eq!(cache.insert("d", "dennis"), Arc::new("dennis"));
        cache.sync();
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(Arc::new("dennis")));

        assert_eq!(cache.remove(&"b"), Some(Arc::new("bob")));
    }

    #[test]
    fn basic_multi_threads() {
        let num_threads = 4;

        let mut cache = Cache::new(100);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        let handles = (0..num_threads)
            .map(|id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    cache.insert(10, format!("{}-100", id));
                    cache.get(&10);
                    cache.sync();
                    cache.insert(20, format!("{}-200", id));
                    cache.remove(&10);
                })
            })
            .collect::<Vec<_>>();

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        cache.sync();

        assert!(cache.get(&10).is_none());
        assert!(cache.get(&20).is_some());
    }

    #[test]
    fn expirations() {
        let mut cache = Builder::new(100)
            .time_to_idle(Duration::from_secs(10))
            .build();

        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        assert_eq!(cache.insert("a", "alice"), Arc::new("alice"));
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 5 secs from the start.

        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 10 secs.

        assert_eq!(cache.insert("b", "bob"), Arc::new("bob"));
        assert_eq!(cache.inner.cache.len(), 2);
        cache.sync();

        mock.increment(Duration::from_secs(5)); // 15 secs.
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        assert_eq!(cache.inner.cache.len(), 1);

        mock.increment(Duration::from_secs(10)); // 25 secs
        cache.sync();

        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), None);
        assert!(cache.inner.cache.is_empty());
    }
}
