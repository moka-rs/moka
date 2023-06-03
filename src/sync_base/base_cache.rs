use super::{
    invalidator::{GetOrRemoveEntry, InvalidationResult, Invalidator, KeyDateLite, PredicateFun},
    iter::ScanningGet,
    key_lock::{KeyLock, KeyLockMap},
    PredicateId,
};

use crate::{
    common::{
        self,
        concurrent::{
            atomic_time::AtomicInstant,
            constants::{
                READ_LOG_FLUSH_POINT, READ_LOG_SIZE, WRITE_LOG_FLUSH_POINT,
                WRITE_LOG_LOW_WATER_MARK, WRITE_LOG_SIZE,
            },
            deques::Deques,
            entry_info::EntryInfo,
            housekeeper::{self, Housekeeper, InnerSync, SyncPace},
            AccessTime, KeyHash, KeyHashDate, KvEntry, ReadOp, ValueEntry, Weigher, WriteOp,
        },
        deque::{DeqNode, Deque},
        frequency_sketch::FrequencySketch,
        time::{CheckedTimeOps, Clock, Instant},
        timer_wheel::{ReschedulingResult, TimerWheel},
        CacheRegion,
    },
    notification::{
        self,
        notifier::{RemovalNotifier, RemovedEntry},
        EvictionListener, RemovalCause,
    },
    policy::ExpirationPolicy,
    stats::{stats_counter::StatsCounter, CacheStats},
    Entry, Expiry, Policy, PredicateError,
};

#[cfg(feature = "unstable-debug-counters")]
use common::concurrent::debug_counters::CacheDebugStats;

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
    time::{Duration, Instant as StdInstant},
};
use triomphe::Arc as TrioArc;

pub(crate) type HouseKeeperArc<K, V, S, CS> = Arc<Housekeeper<Inner<K, V, S, CS>>>;

pub(crate) struct BaseCache<K, V, S = RandomState, CS = CacheStats> {
    pub(crate) inner: Arc<Inner<K, V, S, CS>>,
    read_op_ch: Sender<ReadOp<K, V>>,
    pub(crate) write_op_ch: Sender<WriteOp<K, V>>,
    pub(crate) housekeeper: Option<HouseKeeperArc<K, V, S, CS>>,
}

impl<K, V, S, CS> Clone for BaseCache<K, V, S, CS> {
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

impl<K, V, S, CS> Drop for BaseCache<K, V, S, CS> {
    fn drop(&mut self) {
        // The housekeeper needs to be dropped before the inner is dropped.
        std::mem::drop(self.housekeeper.take());
    }
}

impl<K, V, S, CS> BaseCache<K, V, S, CS> {
    pub(crate) fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    pub(crate) fn policy(&self) -> Policy {
        self.inner.policy()
    }

    pub(crate) fn stats(&self) -> CS {
        self.inner.stats()
    }

    /// Returns a reference to the stats counter.
    pub(crate) fn sc(&self) -> &dyn StatsCounter<Stats = CS> {
        self.inner.sc()
    }

    pub(crate) fn now(&self) -> Instant {
        self.inner.current_time_from_expiration_clock()
    }

    pub(crate) fn elapsed_nanos_since(&self, start: Instant) -> u64 {
        use std::convert::TryInto;
        self.now()
            .checked_duration_since(start)
            .unwrap_or_default()
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    pub(crate) fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    pub(crate) fn weighted_size(&self) -> u64 {
        self.inner.weighted_size()
    }

    pub(crate) fn is_map_disabled(&self) -> bool {
        self.inner.max_capacity == Some(0)
    }

    #[inline]
    pub(crate) fn is_removal_notifier_enabled(&self) -> bool {
        self.inner.is_removal_notifier_enabled()
    }

    #[inline]
    #[cfg(feature = "sync")]
    pub(crate) fn is_blocking_removal_notification(&self) -> bool {
        self.inner.is_blocking_removal_notification()
    }

    #[inline]
    pub(crate) fn current_time_from_expiration_clock(&self) -> Instant {
        self.inner.current_time_from_expiration_clock()
    }

    pub(crate) fn notify_invalidation(&self, key: &Arc<K>, entry: &TrioArc<ValueEntry<K, V>>)
    where
        K: Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        self.inner.notify_invalidation(key, entry);
    }

    #[cfg(feature = "unstable-debug-counters")]
    pub fn debug_stats(&self) -> CacheDebugStats {
        self.inner.debug_stats()
    }
}

impl<K, V, S, CS> BaseCache<K, V, S, CS>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    pub(crate) fn maybe_key_lock(&self, key: &Arc<K>) -> Option<KeyLock<'_, K, S>> {
        self.inner.maybe_key_lock(key)
    }
}

impl<K, V, S, CS> BaseCache<K, V, S, CS>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
    CS: 'static,
{
    // https://rust-lang.github.io/rust-clippy/master/index.html#too_many_arguments
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        name: Option<String>,
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        eviction_listener: Option<EvictionListener<K, V>>,
        eviction_listener_conf: Option<notification::Configuration>,
        expiration_policy: ExpirationPolicy<K, V>,
        stats_counter: Arc<dyn StatsCounter<Stats = CS> + Send + Sync>,
        invalidator_enabled: bool,
        housekeeper_conf: housekeeper::Configuration,
    ) -> Self {
        let (r_size, w_size) = if max_capacity == Some(0) {
            (0, 0)
        } else {
            (READ_LOG_SIZE, WRITE_LOG_SIZE)
        };

        let (r_snd, r_rcv) = crossbeam_channel::bounded(r_size);
        let (w_snd, w_rcv) = crossbeam_channel::bounded(w_size);

        let inner = Arc::new(Inner::new(
            name,
            max_capacity,
            initial_capacity,
            build_hasher,
            weigher,
            eviction_listener,
            eviction_listener_conf,
            stats_counter,
            r_rcv,
            w_rcv,
            expiration_policy,
            invalidator_enabled,
        ));
        if invalidator_enabled {
            inner.set_invalidator(&inner);
        }
        let housekeeper = Housekeeper::new(Arc::downgrade(&inner), housekeeper_conf);
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
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.hash(key)
    }

    pub(crate) fn contains_key_with_hash<Q>(&self, key: &Q, hash: u64) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner
            .get_key_value_and(key, hash, |k, entry| {
                let i = &self.inner;
                let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), &i.valid_after());
                let now = self.current_time_from_expiration_clock();

                !is_expired_by_per_entry_ttl(entry.entry_info(), now)
                    && !is_expired_entry_wo(ttl, va, entry, now)
                    && !is_expired_entry_ao(tti, va, entry, now)
                    && !i.is_invalidated_entry(k, entry)
            })
            .unwrap_or_default() // `false` is the default for `bool` type.
    }

    pub(crate) fn get_with_hash<Q>(&self, key: &Q, hash: u64, need_key: bool) -> Option<Entry<K, V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        // Define a closure to record a read op.
        let record = |op, now| {
            self.record_read_op(op, now)
                .expect("Failed to record a get op")
        };
        let ignore_if = None as Option<&mut fn(&V) -> bool>;
        let (maybe_entry, is_recorded) =
            self.do_get_with_hash(key, hash, record, ignore_if, need_key);
        // When `is_recorded` is `false`, we need to call a `StatsCounter`'s method
        // now. (If `true`, it will be called later by `apply_reads`)
        if !is_recorded {
            if maybe_entry.is_some() {
                self.inner.sc().record_hits(1);
            } else {
                self.inner.sc().record_misses(1);
            }
        }
        maybe_entry
    }

    pub(crate) fn get_with_hash_and_ignore_if<Q, I>(
        &self,
        key: &Q,
        hash: u64,
        ignore_if: Option<&mut I>,
        need_key: bool,
    ) -> Option<Entry<K, V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        I: FnMut(&V) -> bool,
    {
        // Define a closure to record a read op.
        let record = |op, now| {
            self.record_read_op(op, now)
                .expect("Failed to record a get op")
        };
        let (maybe_entry, is_recorded) =
            self.do_get_with_hash(key, hash, record, ignore_if, need_key);
        // When `is_recorded` is `false`, we need to call a `StatsCounter`'s method
        // now. (If `true`, it will be called later by `apply_reads`)
        if !is_recorded {
            if maybe_entry.is_some() {
                self.inner.sc().record_hits(1);
            } else {
                self.inner.sc().record_misses(1);
            }
        }
        maybe_entry
    }

    pub(crate) fn get_with_hash_without_recording<Q, I>(
        &self,
        key: &Q,
        hash: u64,
        ignore_if: Option<&mut I>,
    ) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        I: FnMut(&V) -> bool,
    {
        // Define a closure that skips to record a read op.
        let record = |_op, _now| true;
        self.do_get_with_hash(key, hash, record, ignore_if, false)
            .0
            .map(Entry::into_value)
    }

    /// Returns `(Option<Entry<K, V>>, bool)` where `.0` is the unexpired entry and
    /// `.1` is whether read op is recorded (`true`) or not (`false`).
    fn do_get_with_hash<Q, R, I>(
        &self,
        key: &Q,
        hash: u64,
        read_recorder: R,
        mut ignore_if: Option<&mut I>,
        need_key: bool,
    ) -> (Option<Entry<K, V>>, bool)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        R: Fn(ReadOp<K, V>, Instant) -> bool,
        I: FnMut(&V) -> bool,
    {
        if self.is_map_disabled() {
            return (None, true);
        }

        let mut now = self.current_time_from_expiration_clock();

        let maybe_entry = self
            .inner
            .get_key_value_and_then(key, hash, move |k, entry| {
                if let Some(ignore_if) = &mut ignore_if {
                    if ignore_if(&entry.value) {
                        // Ignore the entry.
                        return None;
                    }
                }

                let i = &self.inner;
                let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), &i.valid_after());

                if is_expired_by_per_entry_ttl(entry.entry_info(), now)
                    || is_expired_entry_wo(ttl, va, entry, now)
                    || is_expired_entry_ao(tti, va, entry, now)
                    || i.is_invalidated_entry(k, entry)
                {
                    // Expired or invalidated entry.
                    None
                } else {
                    // Valid entry.
                    let maybe_key = if need_key { Some(Arc::clone(k)) } else { None };
                    Some((maybe_key, TrioArc::clone(entry)))
                }
            });

        if let Some((maybe_key, entry)) = maybe_entry {
            let mut is_expiry_modified = false;

            // Call the user supplied `expire_after_read` method if any.
            if let Some(expiry) = &self.inner.expiration_policy.expiry() {
                let lm = entry.last_modified().expect("Last modified is not set");
                // Check if the `last_modified` of entry is earlier than or equals to
                // `now`. If not, update the `now` to `last_modified`. This is needed
                // because there is a small chance that other threads have inserted
                // the entry _after_ we obtained `now`.
                now = now.max(lm);

                // Convert `last_modified` from `moka::common::time::Instant` to
                // `std::time::Instant`.
                let lm = self.inner.clocks().to_std_instant(lm);

                // Call the user supplied `expire_after_read` method.
                //
                // We will put the return value (`is_expiry_modified: bool`) to a
                // `ReadOp` so that `apply_reads` method can determine whether or not
                // to reschedule the timer for the entry.
                //
                // NOTE: It is not guaranteed that the `ReadOp` is passed to
                // `apply_reads`. Here are the corner cases that the `ReadOp` will
                // not be passed to `apply_reads`:
                //
                // - If the bounded `read_op_ch` channel is full, the `ReadOp` will
                //   be discarded.
                // - If we were called by `get_with_hash_without_recording` method,
                //   the `ReadOp` will not be recorded at all.
                //
                // These cases are okay because when the timer wheel tries to expire
                // the entry, it will check if the entry is actually expired. If not,
                // the timer wheel will reschedule the expiration timer for the
                // entry.
                is_expiry_modified = Self::expire_after_read_or_update(
                    |k, v, t, d| expiry.expire_after_read(k, v, t, d, lm),
                    &entry.entry_info().key_hash().key,
                    &entry,
                    self.inner.expiration_policy.time_to_live(),
                    self.inner.expiration_policy.time_to_idle(),
                    now,
                    self.inner.clocks(),
                );
            }

            let v = entry.value.clone();
            let op = ReadOp::Hit {
                value_entry: entry,
                timestamp: now,
                is_expiry_modified,
            };
            let is_recorded = read_recorder(op, now);
            (Some(Entry::new(maybe_key, v, false)), is_recorded)
        } else {
            let is_recorded = read_recorder(ReadOp::Miss(hash), now);
            (None, is_recorded)
        }
    }

    #[cfg(feature = "sync")]
    pub(crate) fn get_key_with_hash<Q>(&self, key: &Q, hash: u64) -> Option<Arc<K>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner
            .get_key_value_and(key, hash, |k, _entry| Arc::clone(k))
    }

    #[inline]
    pub(crate) fn remove_entry<Q>(&self, key: &Q, hash: u64) -> Option<KvEntry<K, V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.remove_entry(key, hash)
    }

    #[inline]
    pub(crate) fn apply_reads_writes_if_needed(
        inner: &impl InnerSync,
        ch: &Sender<WriteOp<K, V>>,
        now: Instant,
        housekeeper: Option<&HouseKeeperArc<K, V, S, CS>>,
    ) {
        let w_len = ch.len();

        if let Some(hk) = housekeeper {
            if Self::should_apply_writes(hk, w_len, now) {
                hk.try_sync(inner);
            }
        }
    }

    pub(crate) fn invalidate_all(&self) {
        let now = self.current_time_from_expiration_clock();
        self.inner.set_valid_after(now);
    }

    pub(crate) fn invalidate_entries_if(
        &self,
        predicate: PredicateFun<K, V>,
    ) -> Result<PredicateId, PredicateError> {
        let now = self.current_time_from_expiration_clock();
        self.inner.register_invalidation_predicate(predicate, now)
    }
}

//
// Iterator support
//
impl<K, V, S, CS> ScanningGet<K, V> for BaseCache<K, V, S, CS>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
    CS: 'static,
{
    fn num_cht_segments(&self) -> usize {
        self.inner.num_cht_segments()
    }

    fn scanning_get(&self, key: &Arc<K>) -> Option<V> {
        let hash = self.hash(key);
        self.inner.get_key_value_and_then(key, hash, |k, entry| {
            let i = &self.inner;
            let (ttl, tti, va) = (&i.time_to_live(), &i.time_to_idle(), &i.valid_after());
            let now = self.current_time_from_expiration_clock();

            if is_expired_by_per_entry_ttl(entry.entry_info(), now)
                || is_expired_entry_wo(ttl, va, entry, now)
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
impl<K, V, S, CS> BaseCache<K, V, S, CS>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
    CS: 'static,
{
    /// Tries to push the given `ReadOp` to the channel, and returns the one of the
    /// followings:
    ///
    /// - `Ok(true)` when the `ReadOp` is recorded.
    /// - `Ok(false)` when the `ReadOp` is discarded because the channel is full.
    /// - `Err(TrySendError::Disconnected(_))` when the `ReadOp` is discarded because
    ///   the channel is disconnected.
    #[inline]
    fn record_read_op(
        &self,
        op: ReadOp<K, V>,
        now: Instant,
    ) -> Result<bool, TrySendError<ReadOp<K, V>>> {
        self.apply_reads_if_needed(&self.inner, now);
        let ch = &self.read_op_ch;
        match ch.try_send(op) {
            Ok(()) => Ok(true),
            // Discard the ReadOp when the channel is full.
            Err(TrySendError::Full(_)) => {
                self.sc().record_read_drop();
                Ok(false)
            }
            Err(e @ TrySendError::Disconnected(_)) => Err(e),
        }
    }

    #[inline]
    pub(crate) fn do_insert_with_hash(
        &self,
        key: Arc<K>,
        hash: u64,
        value: V,
    ) -> (WriteOp<K, V>, Instant) {
        let ts = self.current_time_from_expiration_clock();
        let weight = self.inner.weigh(&key, &value);
        let op_cnt1 = Rc::new(AtomicU8::new(0));
        let op_cnt2 = Rc::clone(&op_cnt1);
        let mut op1 = None;
        let mut op2 = None;

        // Lock the key for update if blocking removal notification is enabled.
        let kl = self.maybe_key_lock(&key);
        let _klg = &kl.as_ref().map(|kl| kl.lock());

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
                let entry = self.new_value_entry(&key, hash, value.clone(), ts, weight);
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
                let old_timestamps = (old_entry.last_accessed(), old_entry.last_modified());
                let entry = self.new_value_entry_from(value.clone(), ts, weight, old_entry);
                let cnt = op_cnt2.fetch_add(1, Ordering::Relaxed);
                op2 = Some((
                    cnt,
                    TrioArc::clone(old_entry),
                    old_timestamps,
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
            (Some((_cnt, ins_op)), None) => {
                if let (Some(expiry), WriteOp::Upsert { value_entry, .. }) =
                    (&self.inner.expiration_policy.expiry(), &ins_op)
                {
                    Self::expire_after_create(expiry, &key, value_entry, ts, self.inner.clocks());
                }
                (ins_op, ts)
            }
            (None, Some((_cnt, old_entry, (old_last_accessed, old_last_modified), upd_op))) => {
                if let (Some(expiry), WriteOp::Upsert { value_entry, .. }) =
                    (&self.inner.expiration_policy.expiry(), &upd_op)
                {
                    Self::expire_after_read_or_update(
                        |k, v, t, d| expiry.expire_after_update(k, v, t, d),
                        &key,
                        value_entry,
                        self.inner.expiration_policy.time_to_live(),
                        self.inner.expiration_policy.time_to_idle(),
                        ts,
                        self.inner.clocks(),
                    );
                }

                if self.is_removal_notifier_enabled()
                    || self.sc().is_recording_evictions_supported()
                {
                    self.inner
                        .notify_upsert(key, &old_entry, old_last_accessed, old_last_modified);
                }
                crossbeam_epoch::pin().flush();
                (upd_op, ts)
            }
            (
                Some((cnt1, ins_op)),
                Some((cnt2, old_entry, (old_last_accessed, old_last_modified), upd_op)),
            ) => {
                if cnt1 > cnt2 {
                    if let (Some(expiry), WriteOp::Upsert { value_entry, .. }) =
                        (&self.inner.expiration_policy.expiry(), &ins_op)
                    {
                        Self::expire_after_create(
                            expiry,
                            &key,
                            value_entry,
                            ts,
                            self.inner.clocks(),
                        );
                    }
                    (ins_op, ts)
                } else {
                    if let (Some(expiry), WriteOp::Upsert { value_entry, .. }) =
                        (&self.inner.expiration_policy.expiry(), &upd_op)
                    {
                        Self::expire_after_read_or_update(
                            |k, v, t, d| expiry.expire_after_update(k, v, t, d),
                            &key,
                            value_entry,
                            self.inner.expiration_policy.time_to_live(),
                            self.inner.expiration_policy.time_to_idle(),
                            ts,
                            self.inner.clocks(),
                        );
                    }

                    if self.is_removal_notifier_enabled()
                        || self.sc().is_recording_evictions_supported()
                    {
                        self.inner.notify_upsert(
                            key,
                            &old_entry,
                            old_last_accessed,
                            old_last_modified,
                        );
                    }
                    crossbeam_epoch::pin().flush();
                    (upd_op, ts)
                }
            }
            (None, None) => unreachable!(),
        }
    }

    #[inline]
    fn apply_reads_if_needed(&self, inner: &Inner<K, V, S, CS>, now: Instant) {
        let len = self.read_op_ch.len();

        if let Some(hk) = &self.housekeeper {
            if Self::should_apply_reads(hk, len, now) {
                hk.try_sync(inner);
            }
        }
    }

    #[inline]
    fn should_apply_reads(hk: &HouseKeeperArc<K, V, S, CS>, ch_len: usize, now: Instant) -> bool {
        hk.should_apply_reads(ch_len, now)
    }

    #[inline]
    fn should_apply_writes(hk: &HouseKeeperArc<K, V, S, CS>, ch_len: usize, now: Instant) -> bool {
        hk.should_apply_writes(ch_len, now)
    }
}

impl<K, V, S, CS> BaseCache<K, V, S, CS> {
    #[inline]
    fn new_value_entry(
        &self,
        key: &Arc<K>,
        hash: u64,
        value: V,
        timestamp: Instant,
        policy_weight: u32,
    ) -> TrioArc<ValueEntry<K, V>> {
        let key_hash = KeyHash::new(Arc::clone(key), hash);
        let info = TrioArc::new(EntryInfo::new(key_hash, timestamp, policy_weight));
        TrioArc::new(ValueEntry::new(value, info))
    }

    #[inline]
    fn new_value_entry_from(
        &self,
        value: V,
        timestamp: Instant,
        policy_weight: u32,
        other: &ValueEntry<K, V>,
    ) -> TrioArc<ValueEntry<K, V>> {
        let info = TrioArc::clone(other.entry_info());
        // To prevent this updated ValueEntry from being evicted by an expiration policy,
        // set the dirty flag to true. It will be reset to false when the write is applied.
        info.set_dirty(true);
        info.set_last_accessed(timestamp);
        info.set_last_modified(timestamp);
        info.set_policy_weight(policy_weight);
        TrioArc::new(ValueEntry::new_from(value, info, other))
    }

    fn expire_after_create(
        expiry: &Arc<dyn Expiry<K, V> + Send + Sync + 'static>,
        key: &K,
        value_entry: &ValueEntry<K, V>,
        ts: Instant,
        clocks: &Clocks,
    ) {
        let duration =
            expiry.expire_after_create(key, &value_entry.value, clocks.to_std_instant(ts));
        let expiration_time = duration.map(|duration| ts.checked_add(duration).expect("Overflow"));
        value_entry
            .entry_info()
            .set_expiration_time(expiration_time);
    }

    fn expire_after_read_or_update(
        expiry: impl FnOnce(&K, &V, StdInstant, Option<Duration>) -> Option<Duration>,
        key: &K,
        value_entry: &ValueEntry<K, V>,
        ttl: Option<Duration>,
        tti: Option<Duration>,
        ts: Instant,
        clocks: &Clocks,
    ) -> bool {
        let current_time = clocks.to_std_instant(ts);
        let ei = &value_entry.entry_info();

        let exp_time = IntoIterator::into_iter([
            ei.expiration_time(),
            ttl.and_then(|dur| ei.last_modified().and_then(|ts| ts.checked_add(dur))),
            tti.and_then(|dur| ei.last_accessed().and_then(|ts| ts.checked_add(dur))),
        ])
        .flatten()
        .min();

        let current_duration = exp_time.and_then(|time| {
            let std_time = clocks.to_std_instant(time);
            std_time.checked_duration_since(current_time)
        });

        let duration = expiry(key, &value_entry.value, current_time, current_duration);

        if duration != current_duration {
            let expiration_time =
                duration.map(|duration| ts.checked_add(duration).expect("Overflow"));
            value_entry
                .entry_info()
                .set_expiration_time(expiration_time);
            // The `expiration_time` has changed from `None` to `Some` or vice versa.
            true
        } else {
            false
        }
    }
}

//
// for testing
//
#[cfg(test)]
impl<K, V, S, CS> BaseCache<K, V, S, CS>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
    CS: 'static,
{
    pub(crate) fn invalidation_predicate_count(&self) -> usize {
        self.inner.invalidation_predicate_count()
    }

    pub(crate) fn reconfigure_for_testing(&mut self) {
        // Stop the housekeeping job that may cause sync() method to return earlier.
        if let Some(housekeeper) = &self.housekeeper {
            housekeeper.stop_periodical_sync_job();
        }
        // Enable the frequency sketch.
        self.inner.enable_frequency_sketch_for_testing();
    }

    pub(crate) fn set_expiration_clock(&self, clock: Option<Clock>) {
        self.inner.set_expiration_clock(clock);
    }
}

struct EvictionState<'a, K, V> {
    counters: EvictionCounters,
    notifier: Option<&'a RemovalNotifier<K, V>>,
    removed_entries: Option<Vec<RemovedEntry<K, V>>>,
}

impl<'a, K, V> EvictionState<'a, K, V> {
    fn new(
        entry_count: u64,
        weighted_size: u64,
        notifier: Option<&'a RemovalNotifier<K, V>>,
    ) -> Self {
        let removed_entries = notifier.and_then(|n| {
            if n.is_batching_supported() {
                Some(Vec::new())
            } else {
                None
            }
        });

        Self {
            counters: EvictionCounters::new(entry_count, weighted_size),
            notifier,
            removed_entries,
        }
    }

    fn is_notifier_enabled(&self) -> bool {
        self.notifier.is_some()
    }

    fn add_removed_entry(
        &mut self,
        key: Arc<K>,
        entry: &TrioArc<ValueEntry<K, V>>,
        cause: RemovalCause,
    ) where
        K: Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        debug_assert!(self.is_notifier_enabled());

        if let Some(removed) = &mut self.removed_entries {
            removed.push(RemovedEntry::new(key, entry.value.clone(), cause));
        } else if let Some(notifier) = self.notifier {
            notifier.notify(key, entry.value.clone(), cause);
        }
    }

    fn notify_multiple_removals(&mut self)
    where
        K: Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        if let (Some(notifier), Some(removed)) = (self.notifier, self.removed_entries.take()) {
            notifier.batch_notify(removed);
            notifier.sync();
        }
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

struct Clocks {
    has_expiration_clock: AtomicBool,
    expiration_clock: RwLock<Option<Clock>>,
    /// The time (`moka::common::time`) when this timer wheel was created.
    origin: Instant,
    /// The time (`StdInstant`) when this timer wheel was created.
    origin_std: StdInstant,
    /// Mutable version of `origin` and `origin_std`. Used when the
    /// `expiration_clock` is set.
    mutable_origin: RwLock<Option<(Instant, StdInstant)>>,
}

impl Clocks {
    fn new(time: Instant, std_time: StdInstant) -> Self {
        Self {
            has_expiration_clock: Default::default(),
            expiration_clock: Default::default(),
            origin: time,
            origin_std: std_time,
            mutable_origin: Default::default(),
        }
    }

    fn to_std_instant(&self, time: Instant) -> StdInstant {
        let (origin, origin_std) = if self.has_expiration_clock.load(Ordering::Relaxed) {
            self.mutable_origin
                .read()
                .expect("mutable_origin is not set")
        } else {
            (self.origin, self.origin_std)
        };
        origin_std + (time.checked_duration_since(origin).unwrap())
    }

    #[cfg(test)]
    fn set_origin(&self, time: Instant, std_time: StdInstant) {
        *self.mutable_origin.write() = Some((time, std_time));
    }
}

pub(crate) struct Inner<K, V, S, CS> {
    name: Option<String>,
    max_capacity: Option<u64>,
    entry_count: AtomicCell<u64>,
    weighted_size: AtomicCell<u64>,
    cache: CacheStore<K, V, S>,
    build_hasher: S,
    deques: Mutex<Deques<K>>,
    timer_wheel: Mutex<TimerWheel<K>>,
    frequency_sketch: RwLock<FrequencySketch>,
    frequency_sketch_enabled: AtomicBool,
    read_op_ch: Receiver<ReadOp<K, V>>,
    write_op_ch: Receiver<WriteOp<K, V>>,
    expiration_policy: ExpirationPolicy<K, V>,
    valid_after: AtomicInstant,
    weigher: Option<Weigher<K, V>>,
    removal_notifier: Option<RemovalNotifier<K, V>>,
    key_locks: Option<KeyLockMap<K, S>>,
    invalidator_enabled: bool,
    invalidator: RwLock<Option<Invalidator<K, V, S, CS>>>,
    stats_counter: Arc<dyn StatsCounter<Stats = CS> + Send + Sync>,
    clocks: Clocks,
}

//
// functions/methods used by BaseCache
//

impl<K, V, S, CS> Inner<K, V, S, CS> {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn policy(&self) -> Policy {
        let exp = &self.expiration_policy;
        Policy::new(self.max_capacity, 1, exp.time_to_live(), exp.time_to_idle())
    }

    fn stats(&self) -> CS {
        self.stats_counter.snapshot()
    }

    fn sc(&self) -> &dyn StatsCounter<Stats = CS> {
        &*self.stats_counter
    }

    #[inline]
    fn entry_count(&self) -> u64 {
        self.entry_count.load()
    }

    #[inline]
    fn weighted_size(&self) -> u64 {
        self.weighted_size.load()
    }

    #[inline]
    fn is_removal_notifier_enabled(&self) -> bool {
        self.removal_notifier.is_some()
    }

    #[inline]
    #[cfg(feature = "sync")]
    fn is_blocking_removal_notification(&self) -> bool {
        self.removal_notifier
            .as_ref()
            .map(|rn| rn.is_blocking())
            .unwrap_or_default()
    }

    fn maybe_key_lock(&self, key: &Arc<K>) -> Option<KeyLock<'_, K, S>>
    where
        K: Hash + Eq,
        S: BuildHasher,
    {
        self.key_locks.as_ref().map(|kls| kls.key_lock(key))
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

    #[inline]
    fn current_time_from_expiration_clock(&self) -> Instant {
        if self.clocks.has_expiration_clock.load(Ordering::Relaxed) {
            Instant::new(
                self.clocks
                    .expiration_clock
                    .read()
                    .as_ref()
                    .expect("Cannot get the expiration clock")
                    .now(),
            )
        } else {
            Instant::now()
        }
    }

    fn clocks(&self) -> &Clocks {
        &self.clocks
    }

    fn num_cht_segments(&self) -> usize {
        self.cache.actual_num_segments()
    }

    #[inline]
    fn time_to_live(&self) -> Option<Duration> {
        self.expiration_policy.time_to_live()
    }

    #[inline]
    fn time_to_idle(&self) -> Option<Duration> {
        self.expiration_policy.time_to_idle()
    }

    #[inline]
    fn has_expiry(&self) -> bool {
        let exp = &self.expiration_policy;
        exp.time_to_live().is_some() || exp.time_to_idle().is_some()
    }

    #[inline]
    fn is_write_order_queue_enabled(&self) -> bool {
        self.expiration_policy.time_to_live().is_some() || self.invalidator_enabled
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
}

impl<K, V, S, CS> Inner<K, V, S, CS>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone,
{
    // Disable a Clippy warning for having more than seven arguments.
    // https://rust-lang.github.io/rust-clippy/master/index.html#too_many_arguments
    #[allow(clippy::too_many_arguments)]
    fn new(
        name: Option<String>,
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        eviction_listener: Option<EvictionListener<K, V>>,
        eviction_listener_conf: Option<notification::Configuration>,
        stats_counter: Arc<dyn StatsCounter<Stats = CS> + Send + Sync>,
        read_op_ch: Receiver<ReadOp<K, V>>,
        write_op_ch: Receiver<WriteOp<K, V>>,
        expiration_policy: ExpirationPolicy<K, V>,
        invalidator_enabled: bool,
    ) -> Self {
        let (num_segments, initial_capacity) = if max_capacity == Some(0) {
            (1, 0)
        } else {
            let ic = initial_capacity
                .map(|cap| cap + WRITE_LOG_SIZE)
                .unwrap_or_default();
            (64, ic)
        };
        let cache = crate::cht::SegmentedHashMap::with_num_segments_capacity_and_hasher(
            num_segments,
            initial_capacity,
            build_hasher.clone(),
        );

        // Assume that getting `moka::common::Instant::now` has lower latency than
        // `StdInstant::now`.
        let now_std = StdInstant::now();
        let now = Instant::now();
        let clocks = Clocks::new(now, now_std);
        let timer_wheel = Mutex::new(TimerWheel::new(now));

        let (removal_notifier, key_locks) = if let Some(listener) = eviction_listener {
            let rn = RemovalNotifier::new(
                listener,
                eviction_listener_conf.unwrap_or_default(),
                name.clone(),
            );
            if rn.is_blocking() {
                let kl = KeyLockMap::with_hasher(build_hasher.clone());
                (Some(rn), Some(kl))
            } else {
                (Some(rn), None)
            }
        } else {
            (None, None)
        };

        Self {
            name,
            max_capacity,
            entry_count: Default::default(),
            weighted_size: Default::default(),
            cache,
            build_hasher,
            deques: Default::default(),
            timer_wheel,
            frequency_sketch: RwLock::new(Default::default()),
            frequency_sketch_enabled: Default::default(),
            read_op_ch,
            write_op_ch,
            expiration_policy,
            valid_after: Default::default(),
            weigher,
            removal_notifier,
            key_locks,
            invalidator_enabled,
            // When enabled, this field will be set later via the set_invalidator method.
            invalidator: Default::default(),
            stats_counter,
            clocks,
        }
    }

    fn set_invalidator(&self, self_ref: &Arc<Self>) {
        *self.invalidator.write() = Some(Invalidator::new(Arc::downgrade(&Arc::clone(self_ref))));
    }

    #[inline]
    fn hash<Q>(&self, key: &Q) -> u64
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut hasher = self.build_hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[inline]
    fn get_key_value_and<Q, F, T>(&self, key: &Q, hash: u64, with_entry: F) -> Option<T>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        F: FnOnce(&Arc<K>, &TrioArc<ValueEntry<K, V>>) -> T,
    {
        self.cache
            .get_key_value_and(hash, |k| (k as &K).borrow() == key, with_entry)
    }

    #[inline]
    fn get_key_value_and_then<Q, F, T>(&self, key: &Q, hash: u64, with_entry: F) -> Option<T>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        F: FnOnce(&Arc<K>, &TrioArc<ValueEntry<K, V>>) -> Option<T>,
    {
        self.cache
            .get_key_value_and_then(hash, |k| (k as &K).borrow() == key, with_entry)
    }

    #[inline]
    fn remove_entry<Q>(&self, key: &Q, hash: u64) -> Option<KvEntry<K, V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cache
            .remove_entry(hash, |k| (k as &K).borrow() == key)
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

    /// Returns `true` if the entry is invalidated by `invalidate_entries_if` method.
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
}

impl<K, V, S, CS> GetOrRemoveEntry<K, V> for Arc<Inner<K, V, S, CS>>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    fn get_value_entry(&self, key: &Arc<K>, hash: u64) -> Option<TrioArc<ValueEntry<K, V>>> {
        self.cache.get(hash, |k| k == key)
    }

    fn remove_key_value_if(
        &self,
        key: &Arc<K>,
        hash: u64,
        condition: impl FnMut(&Arc<K>, &TrioArc<ValueEntry<K, V>>) -> bool,
    ) -> Option<TrioArc<ValueEntry<K, V>>>
    where
        K: Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        // Lock the key for removal if blocking removal notification is enabled.
        let kl = self.maybe_key_lock(key);
        let _klg = &kl.as_ref().map(|kl| kl.lock());

        let maybe_entry = self.cache.remove_if(hash, |k| k == key, condition);
        if let Some(entry) = &maybe_entry {
            if self.is_removal_notifier_enabled() {
                self.notify_single_removal(Arc::clone(key), entry, RemovalCause::Explicit);
            }
        }
        maybe_entry
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
impl<K, V, S, CS> InnerSync for Inner<K, V, S, CS>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
    CS: 'static,
{
    fn sync(&self, max_repeats: usize) -> Option<SyncPace> {
        if self.max_capacity == Some(0) {
            return None;
        }

        let mut deqs = self.deques.lock();
        let mut timer_wheel = self.timer_wheel.lock();
        let mut calls = 0;
        let current_ec = self.entry_count.load();
        let current_ws = self.weighted_size.load();
        let mut eviction_state =
            EvictionState::new(current_ec, current_ws, self.removal_notifier.as_ref());

        let mut should_process_logs = true;

        while should_process_logs && calls <= max_repeats {
            let r_len = self.read_op_ch.len();
            if r_len > 0 {
                self.apply_reads(&mut deqs, &mut timer_wheel, r_len);
            }

            let w_len = self.write_op_ch.len();
            if w_len > 0 {
                self.apply_writes(&mut deqs, &mut timer_wheel, w_len, &mut eviction_state);
            }

            if self.should_enable_frequency_sketch(&eviction_state.counters) {
                self.enable_frequency_sketch(&eviction_state.counters);
            }

            calls += 1;
            should_process_logs = self.read_op_ch.len() >= READ_LOG_FLUSH_POINT
                || self.write_op_ch.len() >= WRITE_LOG_FLUSH_POINT;
        }

        if timer_wheel.is_enabled() {
            self.evict_expired_entries_using_timers(
                &mut timer_wheel,
                &mut deqs,
                &mut eviction_state,
            );
        }

        if self.has_expiry() || self.has_valid_after() {
            self.evict_expired_entries_using_deqs(
                &mut deqs,
                &mut timer_wheel,
                batch_size::EVICTION_BATCH_SIZE,
                &mut eviction_state,
            );
        }

        if self.invalidator_enabled {
            if let Some(invalidator) = &*self.invalidator.read() {
                if !invalidator.is_empty() && !invalidator.is_task_running() {
                    self.invalidate_entries(
                        invalidator,
                        &mut deqs,
                        &mut timer_wheel,
                        batch_size::INVALIDATION_BATCH_SIZE,
                        &mut eviction_state,
                    );
                }
            }
        }

        // Evict if this cache has more entries than its capacity.
        let weights_to_evict = self.weights_to_evict(&eviction_state.counters);
        if weights_to_evict > 0 {
            self.evict_lru_entries(
                &mut deqs,
                &mut timer_wheel,
                batch_size::EVICTION_BATCH_SIZE,
                weights_to_evict,
                &mut eviction_state,
            );
        }

        eviction_state.notify_multiple_removals();

        debug_assert_eq!(self.entry_count.load(), current_ec);
        debug_assert_eq!(self.weighted_size.load(), current_ws);
        self.entry_count.store(eviction_state.counters.entry_count);
        self.weighted_size
            .store(eviction_state.counters.weighted_size);

        crossbeam_epoch::pin().flush();

        if should_process_logs {
            Some(SyncPace::Fast)
        } else if self.write_op_ch.len() <= WRITE_LOG_LOW_WATER_MARK {
            Some(SyncPace::Normal)
        } else {
            // Keep the current pace.
            None
        }
    }

    fn now(&self) -> Instant {
        self.current_time_from_expiration_clock()
    }
}

//
// private methods
//
impl<K, V, S, CS> Inner<K, V, S, CS>
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
        match self.max_capacity {
            None | Some(0) => false,
            Some(max_cap) => {
                if self.frequency_sketch_enabled.load(Ordering::Acquire) {
                    false // The frequency sketch is already enabled.
                } else {
                    counters.weighted_size >= max_cap / 2
                }
            }
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

    fn apply_reads(&self, deqs: &mut Deques<K>, timer_wheel: &mut TimerWheel<K>, count: usize) {
        use ReadOp::*;
        let mut freq = self.frequency_sketch.write();
        let ch = &self.read_op_ch;
        let mut hit_count = 0;
        let mut miss_count = 0;

        for _ in 0..count {
            match ch.try_recv() {
                Ok(Hit {
                    value_entry,
                    timestamp,
                    is_expiry_modified,
                }) => {
                    hit_count += 1;
                    let kh = value_entry.entry_info().key_hash();
                    freq.increment(kh.hash);
                    value_entry.set_last_accessed(timestamp);
                    if is_expiry_modified {
                        self.update_timer_wheel(&value_entry, timer_wheel);
                    }
                    deqs.move_to_back_ao(&value_entry);
                }
                Ok(Miss(hash)) => {
                    miss_count += 1;
                    freq.increment(hash);
                }
                Err(_) => break,
            }
        }
        self.stats_counter.record_hits(hit_count);
        self.stats_counter.record_misses(miss_count);
    }

    fn apply_writes(
        &self,
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        count: usize,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
    {
        use WriteOp::*;
        let freq = self.frequency_sketch.read();
        let ch = &self.write_op_ch;
        let mut insertion_count = 0;

        for _ in 0..count {
            match ch.try_recv() {
                Ok(Upsert {
                    key_hash: kh,
                    value_entry: entry,
                    old_weight,
                    new_weight,
                }) => {
                    insertion_count += 1;
                    self.handle_upsert(
                        kh,
                        entry,
                        old_weight,
                        new_weight,
                        deqs,
                        timer_wheel,
                        &freq,
                        eviction_state,
                    );
                }
                Ok(Remove(KvEntry { key: _key, entry })) => {
                    Self::handle_remove(deqs, timer_wheel, entry, &mut eviction_state.counters)
                }
                Err(_) => break,
            };
        }
        self.stats_counter.record_insertions(insertion_count);
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_upsert(
        &self,
        kh: KeyHash<K>,
        entry: TrioArc<ValueEntry<K, V>>,
        old_weight: u32,
        new_weight: u32,
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        freq: &FrequencySketch,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
    {
        entry.set_dirty(false);

        {
            let counters = &mut eviction_state.counters;

            if entry.is_admitted() {
                // The entry has been already admitted, so treat this as an update.
                counters.saturating_sub(0, old_weight);
                counters.saturating_add(0, new_weight);
                self.update_timer_wheel(&entry, timer_wheel);
                deqs.move_to_back_ao(&entry);
                deqs.move_to_back_wo(&entry);
                return;
            }

            if self.has_enough_capacity(new_weight, counters) {
                // There are enough room in the cache (or the cache is unbounded).
                // Add the candidate to the deques.
                self.handle_admit(&entry, new_weight, deqs, timer_wheel, counters);
                return;
            }
        }

        if let Some(max) = self.max_capacity {
            if new_weight as u64 > max {
                // The candidate is too big to fit in the cache. Reject it.

                // Lock the key for removal if blocking removal notification is enabled.
                let kl = self.maybe_key_lock(&kh.key);
                let _klg = &kl.as_ref().map(|kl| kl.lock());

                let removed = self.cache.remove(kh.hash, |k| k == &kh.key);
                if let Some(entry) = removed {
                    let cause = RemovalCause::Size;
                    self.stats_counter.record_eviction(new_weight, cause);
                    if eviction_state.is_notifier_enabled() {
                        let key = Arc::clone(&kh.key);
                        eviction_state.add_removed_entry(key, &entry, cause);
                    }
                }
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

                    // Lock the key for removal if blocking removal notification is enabled.
                    let kl = self.maybe_key_lock(element.key());
                    let _klg = &kl.as_ref().map(|kl| kl.lock());

                    if let Some((vic_key, vic_entry)) = self
                        .cache
                        .remove_entry(element.hash(), |k| k == element.key())
                    {
                        let cause = RemovalCause::Size;
                        self.stats_counter
                            .record_eviction(vic_entry.policy_weight(), cause);
                        if eviction_state.is_notifier_enabled() {
                            eviction_state.add_removed_entry(vic_key, &vic_entry, cause);
                        }
                        // And then remove the victim from the deques.
                        Self::handle_remove(
                            deqs,
                            timer_wheel,
                            vic_entry,
                            &mut eviction_state.counters,
                        );
                    } else {
                        // Could not remove the victim from the cache. Skip this
                        // victim node as its ValueEntry might have been
                        // invalidated. Add it to the skipped nodes.
                        skipped.push(victim);
                    }
                }
                skipped_nodes = skipped;

                // Add the candidate to the deques.
                self.handle_admit(
                    &entry,
                    new_weight,
                    deqs,
                    timer_wheel,
                    &mut eviction_state.counters,
                );
            }
            AdmissionResult::Rejected { skipped_nodes: s } => {
                skipped_nodes = s;

                // Lock the key for removal if blocking removal notification is enabled.
                let kl = self.maybe_key_lock(&kh.key);
                let _klg = &kl.as_ref().map(|kl| kl.lock());

                // Remove the candidate from the cache (hash map).
                let key = Arc::clone(&kh.key);
                self.cache.remove(kh.hash, |k| k == &key);

                let cause = RemovalCause::Size;
                self.stats_counter.record_eviction(new_weight, cause);
                if eviction_state.is_notifier_enabled() {
                    eviction_state.add_removed_entry(key, &entry, cause);
                }
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
        let mut next_victim = deqs.probation.peek_front_ptr();

        // Aggregate potential victims.
        while victims.policy_weight < candidate.policy_weight {
            if candidate.freq < victims.freq {
                break;
            }
            if let Some(victim) = next_victim.take() {
                next_victim = DeqNode::next_node_ptr(victim);
                let vic_elem = &unsafe { victim.as_ref() }.element;

                if let Some(vic_entry) = cache.get(vic_elem.hash(), |k| k == vic_elem.key()) {
                    victims.add_policy_weight(vic_entry.policy_weight());
                    victims.add_frequency(freq, vic_elem.hash());
                    victim_nodes.push(victim);
                    retries = 0;
                } else {
                    // Could not get the victim from the cache (hash map). Skip this node
                    // as its ValueEntry might have been invalidated.
                    skipped_nodes.push(victim);

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
        entry: &TrioArc<ValueEntry<K, V>>,
        policy_weight: u32,
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        counters: &mut EvictionCounters,
    ) {
        counters.saturating_add(1, policy_weight);

        self.update_timer_wheel(entry, timer_wheel);

        // Update the deques.
        deqs.push_back_ao(
            CacheRegion::MainProbation,
            KeyHashDate::new(entry.entry_info()),
            entry,
        );
        if self.is_write_order_queue_enabled() {
            deqs.push_back_wo(KeyHashDate::new(entry.entry_info()), entry);
        }
        entry.set_admitted(true);
    }

    /// NOTE: This method may enable the timer wheel.
    fn update_timer_wheel(
        &self,
        entry: &TrioArc<ValueEntry<K, V>>,
        timer_wheel: &mut TimerWheel<K>,
    ) {
        // Enable the timer wheel if needed.
        if entry.entry_info().expiration_time().is_some() && !timer_wheel.is_enabled() {
            timer_wheel.enable();
        }

        // Update the timer wheel.
        match (
            entry.entry_info().expiration_time().is_some(),
            entry.timer_node(),
        ) {
            // Do nothing; the cache entry has no expiration time and not registered
            // to the timer wheel.
            (false, None) => (),
            // Register the cache entry to the timer wheel; the cache entry has an
            // expiration time and not registered to the timer wheel.
            (true, None) => {
                let timer = timer_wheel.schedule(
                    TrioArc::clone(entry.entry_info()),
                    TrioArc::clone(entry.deq_nodes()),
                );
                entry.set_timer_node(timer);
            }
            // Reschedule the cache entry in the timer wheel; the cache entry has an
            // expiration time and already registered to the timer wheel.
            (true, Some(tn)) => {
                let result = timer_wheel.reschedule(tn);
                if let ReschedulingResult::Removed(removed_tn) = result {
                    // The timer node was removed from the timer wheel because the
                    // expiration time has been unset by other thread after we
                    // checked.
                    entry.set_timer_node(None);
                    drop(removed_tn);
                }
            }
            // Unregister the cache entry from the timer wheel; the cache entry has
            // no expiration time but registered to the timer wheel.
            (false, Some(tn)) => {
                entry.set_timer_node(None);
                timer_wheel.deschedule(tn);
            }
        }
    }

    fn handle_remove(
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        entry: TrioArc<ValueEntry<K, V>>,
        counters: &mut EvictionCounters,
    ) {
        if let Some(timer_node) = entry.take_timer_node() {
            timer_wheel.deschedule(timer_node);
        }
        Self::handle_remove_without_timer_wheel(deqs, entry, counters);
    }

    fn handle_remove_without_timer_wheel(
        deqs: &mut Deques<K>,
        entry: TrioArc<ValueEntry<K, V>>,
        counters: &mut EvictionCounters,
    ) {
        if entry.is_admitted() {
            entry.set_admitted(false);
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
        wo_deq: &mut Deque<KeyHashDate<K>>,
        timer_wheel: &mut TimerWheel<K>,
        entry: TrioArc<ValueEntry<K, V>>,
        counters: &mut EvictionCounters,
    ) {
        if let Some(timer) = entry.take_timer_node() {
            timer_wheel.deschedule(timer);
        }
        if entry.is_admitted() {
            entry.set_admitted(false);
            counters.saturating_sub(1, entry.policy_weight());
            // The following two unlink_* functions will unset the deq nodes.
            Deques::unlink_ao_from_deque(ao_deq_name, ao_deq, &entry);
            Deques::unlink_wo(wo_deq, &entry);
        } else {
            entry.unset_q_nodes();
        }
    }

    fn evict_expired_entries_using_timers(
        &self,
        timer_wheel: &mut TimerWheel<K>,
        deqs: &mut Deques<K>,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
    {
        use crate::common::timer_wheel::TimerEvent;

        let now = self.current_time_from_expiration_clock();

        // NOTE: When necessary, the iterator returned from advance() will unset the
        // timer node pointer in the `ValueEntry`, so we do not have to do it here.
        for event in timer_wheel.advance(now) {
            // We do not have to do anything if event is `TimerEvent::Descheduled(_)`
            // or `TimerEvent::Rescheduled(_)`.
            if let TimerEvent::Expired(node) = event {
                let entry_info = node.element.entry_info();
                let kh = entry_info.key_hash();
                let key = &kh.key;
                let hash = kh.hash;

                // Lock the key for removal if blocking removal notification is
                // enabled.
                let kl = self.maybe_key_lock(key);
                let _klg = &kl.as_ref().map(|kl| kl.lock());

                // Remove the key from the map only when the entry is really
                // expired.
                let maybe_entry = self.cache.remove_if(
                    hash,
                    |k| k == key,
                    |_, v| is_expired_by_per_entry_ttl(v.entry_info(), now),
                );

                if let Some(entry) = maybe_entry {
                    let cause = RemovalCause::Expired;
                    self.stats_counter
                        .record_eviction(entry.policy_weight(), cause);
                    if eviction_state.is_notifier_enabled() {
                        let key = Arc::clone(key);
                        eviction_state.add_removed_entry(key, &entry, cause);
                    }
                    Self::handle_remove_without_timer_wheel(
                        deqs,
                        entry,
                        &mut eviction_state.counters,
                    );
                } else {
                    // Other thread might have updated or invalidated the entry
                    // already. We have nothing to do here as the `advance()`
                    // iterator has unset the timer node pointer in the old
                    // `ValueEntry`. (In the case of update, the timer node will be
                    // recreated for the new `ValueEntry` when it is processed by the
                    // `handle_upsert` method.)
                }
            }
        }
    }

    fn evict_expired_entries_using_deqs(
        &self,
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        batch_size: usize,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
    {
        let now = self.current_time_from_expiration_clock();

        if self.is_write_order_queue_enabled() {
            self.remove_expired_wo(deqs, timer_wheel, batch_size, now, eviction_state);
        }

        if self.expiration_policy.time_to_idle().is_some() || self.has_valid_after() {
            let (window, probation, protected, wo) = (
                &mut deqs.window,
                &mut deqs.probation,
                &mut deqs.protected,
                &mut deqs.write_order,
            );

            let mut rm_expired_ao = |name, deq| {
                self.remove_expired_ao(name, deq, wo, timer_wheel, batch_size, now, eviction_state)
            };

            rm_expired_ao("window", window);
            rm_expired_ao("probation", probation);
            rm_expired_ao("protected", protected);
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn remove_expired_ao(
        &self,
        deq_name: &str,
        deq: &mut Deque<KeyHashDate<K>>,
        write_order_deq: &mut Deque<KeyHashDate<K>>,
        timer_wheel: &mut TimerWheel<K>,
        batch_size: usize,
        now: Instant,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
    {
        let tti = &self.expiration_policy.time_to_idle();
        let va = &self.valid_after();
        for _ in 0..batch_size {
            // Peek the front node of the deque and check if it is expired.
            let key_hash_cause = deq.peek_front().and_then(|node| {
                // TODO: Skip the entry if it is dirty. See `evict_lru_entries` method as an example.
                match is_entry_expired_ao_or_invalid(tti, va, node, now) {
                    (true, _) => Some((
                        Arc::clone(node.element.key()),
                        node.element.hash(),
                        RemovalCause::Expired,
                    )),
                    (false, true) => Some((
                        Arc::clone(node.element.key()),
                        node.element.hash(),
                        RemovalCause::Explicit,
                    )),
                    (false, false) => None,
                }
            });

            if key_hash_cause.is_none() {
                break;
            }

            let (key, hash, cause) = key_hash_cause
                .as_ref()
                .map(|(k, h, c)| (k, *h, *c))
                .unwrap();

            // Lock the key for removal if blocking removal notification is enabled.
            let kl = self.maybe_key_lock(key);
            let _klg = &kl.as_ref().map(|kl| kl.lock());

            // Remove the key from the map only when the entry is really
            // expired. This check is needed because it is possible that the entry in
            // the map has been updated or deleted but its deque node we checked
            // above has not been updated yet.
            let maybe_entry = self.cache.remove_if(
                hash,
                |k| k == key,
                |_, v| is_expired_entry_ao(tti, va, v, now),
            );

            if let Some(entry) = maybe_entry {
                self.stats_counter
                    .record_eviction(entry.policy_weight(), cause);
                if eviction_state.is_notifier_enabled() {
                    let key = Arc::clone(key);
                    eviction_state.add_removed_entry(key, &entry, cause);
                }
                Self::handle_remove_with_deques(
                    deq_name,
                    deq,
                    write_order_deq,
                    timer_wheel,
                    entry,
                    &mut eviction_state.counters,
                );
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
        write_order_deq: &mut Deque<KeyHashDate<K>>,
    ) -> bool {
        if let Some(entry) = self.cache.get(hash, |k| (k.borrow() as &K) == key) {
            if entry.is_dirty() {
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
            // invalidated ValueEntry (which should be still in the write op queue)
            // has a pointer to this node, move the node to the back of the deque
            // instead of popping (dropping) it.
            deq.move_front_to_back();
            true
        }
    }

    #[inline]
    fn remove_expired_wo(
        &self,
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        batch_size: usize,
        now: Instant,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
    {
        let ttl = &self.expiration_policy.time_to_live();
        let va = &self.valid_after();
        for _ in 0..batch_size {
            let key_cause = deqs.write_order.peek_front().and_then(
                // TODO: Skip the entry if it is dirty. See `evict_lru_entries` method as an example.
                |node| match is_entry_expired_wo_or_invalid(ttl, va, node, now) {
                    (true, _) => Some((Arc::clone(node.element.key()), RemovalCause::Expired)),
                    (false, true) => Some((Arc::clone(node.element.key()), RemovalCause::Explicit)),
                    (false, false) => None,
                },
            );

            if key_cause.is_none() {
                break;
            }

            let (key, cause) = key_cause.as_ref().unwrap();
            let hash = self.hash(key);

            // Lock the key for removal if blocking removal notification is enabled.
            let kl = self.maybe_key_lock(key);
            let _klg = &kl.as_ref().map(|kl| kl.lock());

            let maybe_entry = self.cache.remove_if(
                hash,
                |k| k == key,
                |_, v| is_expired_entry_wo(ttl, va, v, now),
            );

            if let Some(entry) = maybe_entry {
                self.stats_counter
                    .record_eviction(entry.policy_weight(), *cause);
                if eviction_state.is_notifier_enabled() {
                    let key = Arc::clone(key);
                    eviction_state.add_removed_entry(key, &entry, *cause);
                }
                Self::handle_remove(deqs, timer_wheel, entry, &mut eviction_state.counters);
            } else if let Some(entry) = self.cache.get(hash, |k| k == key) {
                // TODO: CHECKME: Should we check `entry.is_dirty()` instead?
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
                deqs.write_order.move_front_to_back();
            }
        }
    }

    fn invalidate_entries(
        &self,
        invalidator: &Invalidator<K, V, S, CS>,
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        batch_size: usize,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
        CS: 'static,
    {
        self.process_invalidation_result(invalidator, deqs, timer_wheel, eviction_state);
        self.submit_invalidation_task(invalidator, &mut deqs.write_order, batch_size);
    }

    fn process_invalidation_result(
        &self,
        invalidator: &Invalidator<K, V, S, CS>,
        deqs: &mut Deques<K>,
        timer_wheel: &mut TimerWheel<K>,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
    {
        if let Some(InvalidationResult {
            invalidated,
            is_done,
        }) = invalidator.task_result()
        {
            for KvEntry { key: _key, entry } in invalidated {
                Self::handle_remove(deqs, timer_wheel, entry, &mut eviction_state.counters);
            }
            if is_done {
                deqs.write_order.reset_cursor();
            }
        }
    }

    fn submit_invalidation_task(
        &self,
        invalidator: &Invalidator<K, V, S, CS>,
        write_order: &mut Deque<KeyHashDate<K>>,
        batch_size: usize,
    ) where
        V: Clone,
        CS: 'static,
    {
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
                if !kd.is_dirty() {
                    if let Some(ts) = kd.last_modified() {
                        let key = kd.key();
                        let hash = self.hash(key);
                        candidates.push(KeyDateLite::new(key, hash, ts));
                        len += 1;
                    }
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
        timer_wheel: &mut TimerWheel<K>,
        batch_size: usize,
        weights_to_evict: u64,
        eviction_state: &mut EvictionState<'_, K, V>,
    ) where
        V: Clone,
        CS: 'static,
    {
        const DEQ_NAME: &str = "probation";
        let mut evicted = 0u64;
        let (deq, write_order_deq) = (&mut deqs.probation, &mut deqs.write_order);

        for _ in 0..batch_size {
            if evicted >= weights_to_evict {
                break;
            }

            let maybe_key_hash_ts = deq.peek_front().map(|node| {
                let entry_info = node.element.entry_info();
                (
                    Arc::clone(node.element.key()),
                    node.element.hash(),
                    entry_info.is_dirty(),
                    entry_info.last_modified(),
                )
            });

            let (key, hash, ts) = match maybe_key_hash_ts {
                Some((key, hash, false, Some(ts))) => (key, hash, ts),
                // TODO: Remove the second pattern `Some((_key, false, None))` once we change
                // `last_modified` and `last_accessed` in `EntryInfo` from `Option<Instant>` to
                // `Instant`.
                Some((key, hash, true, _)) | Some((key, hash, false, None)) => {
                    if self.try_skip_updated_entry(&key, hash, DEQ_NAME, deq, write_order_deq) {
                        continue;
                    } else {
                        break;
                    }
                }
                None => break,
            };

            // Lock the key for removal if blocking removal notification is enabled.
            let kl = self.maybe_key_lock(&key);
            let _klg = &kl.as_ref().map(|kl| kl.lock());

            let maybe_entry = self.cache.remove_if(
                hash,
                |k| k == &key,
                |_, v| {
                    if let Some(lm) = v.last_modified() {
                        lm == ts
                    } else {
                        false
                    }
                },
            );

            if let Some(entry) = maybe_entry {
                let cause = RemovalCause::Size;
                self.stats_counter
                    .record_eviction(entry.policy_weight(), cause);
                if eviction_state.is_notifier_enabled() {
                    eviction_state.add_removed_entry(key, &entry, cause);
                }
                let weight = entry.policy_weight();
                Self::handle_remove_with_deques(
                    DEQ_NAME,
                    deq,
                    write_order_deq,
                    timer_wheel,
                    entry,
                    &mut eviction_state.counters,
                );
                evicted = evicted.saturating_add(weight as u64);
            } else if !self.try_skip_updated_entry(&key, hash, DEQ_NAME, deq, write_order_deq) {
                break;
            }
        }
    }
}

impl<K, V, S, CS> Inner<K, V, S, CS>
where
    K: Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn notify_single_removal(
        &self,
        key: Arc<K>,
        entry: &TrioArc<ValueEntry<K, V>>,
        cause: RemovalCause,
    ) {
        self.stats_counter
            .record_eviction(entry.policy_weight(), cause);
        if let Some(notifier) = &self.removal_notifier {
            notifier.notify(key, entry.value.clone(), cause)
        }
    }

    #[inline]
    fn notify_upsert(
        &self,
        key: Arc<K>,
        entry: &TrioArc<ValueEntry<K, V>>,
        last_accessed: Option<Instant>,
        last_modified: Option<Instant>,
    ) {
        let now = self.current_time_from_expiration_clock();
        let exp = &self.expiration_policy;

        let mut cause = RemovalCause::Replaced;

        if let Some(last_accessed) = last_accessed {
            if is_expired_by_tti(&exp.time_to_idle(), last_accessed, now) {
                cause = RemovalCause::Expired;
            }
        }

        if let Some(last_modified) = last_modified {
            if is_expired_by_ttl(&exp.time_to_live(), last_modified, now) {
                cause = RemovalCause::Expired;
            } else if is_invalid_entry(&self.valid_after(), last_modified) {
                cause = RemovalCause::Explicit;
            }
        }

        self.notify_single_removal(key, entry, cause);
    }

    #[inline]
    fn notify_invalidation(&self, key: &Arc<K>, entry: &TrioArc<ValueEntry<K, V>>) {
        let now = self.current_time_from_expiration_clock();
        let exp = &self.expiration_policy;

        let mut cause = RemovalCause::Explicit;

        if let Some(last_accessed) = entry.last_accessed() {
            if is_expired_by_tti(&exp.time_to_idle(), last_accessed, now) {
                cause = RemovalCause::Expired;
            }
        }

        if let Some(last_modified) = entry.last_modified() {
            if is_expired_by_ttl(&exp.time_to_live(), last_modified, now) {
                cause = RemovalCause::Expired;
            }
        }

        self.notify_single_removal(Arc::clone(key), entry, cause);
    }
}

//
// for testing
//
#[cfg(test)]
impl<K, V, S, CS> Inner<K, V, S, CS>
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
        let mut exp_clock = self.clocks.expiration_clock.write();
        if let Some(clock) = clock {
            let std_now = StdInstant::now();
            let now = Instant::new(clock.now());
            *exp_clock = Some(clock);
            self.clocks
                .has_expiration_clock
                .store(true, Ordering::SeqCst);
            self.clocks.set_origin(now, std_now);
            self.timer_wheel.lock().set_origin(now);
        } else {
            self.clocks
                .has_expiration_clock
                .store(false, Ordering::SeqCst);
            *exp_clock = None;
        }
    }
}

//
// private free-standing functions
//

/// Returns `true` if this entry is expired by its per-entry TTL.
#[inline]
fn is_expired_by_per_entry_ttl<K>(entry_info: &TrioArc<EntryInfo<K>>, now: Instant) -> bool {
    if let Some(ts) = entry_info.expiration_time() {
        return ts <= now;
    }
    false
}

/// Returns `true` when one of the followings conditions is met:
///
/// - This entry is expired by the time-to-idle config of this cache instance.
/// - Or, it is invalidated by the `invalidate_all` method.
#[inline]
fn is_expired_entry_ao(
    time_to_idle: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_accessed() {
        if is_invalid_entry(valid_after, ts) || is_expired_by_tti(time_to_idle, ts, now) {
            return true;
        }
    }
    false
}

/// Returns `true` when one of the following conditions is met:
///
/// - This entry is expired by the time-to-live (TTL) config of this cache instance.
/// - Or, it is invalidated by the `invalidate_all` method.
#[inline]
fn is_expired_entry_wo(
    time_to_live: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_modified() {
        if is_invalid_entry(valid_after, ts) || is_expired_by_ttl(time_to_live, ts, now) {
            return true;
        }
    }
    false
}

#[inline]
fn is_entry_expired_ao_or_invalid(
    time_to_idle: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> (bool, bool) {
    if let Some(ts) = entry.last_accessed() {
        let expired = is_expired_by_tti(time_to_idle, ts, now);
        let invalid = is_invalid_entry(valid_after, ts);
        return (expired, invalid);
    }
    (false, false)
}

#[inline]
fn is_entry_expired_wo_or_invalid(
    time_to_live: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> (bool, bool) {
    if let Some(ts) = entry.last_modified() {
        let expired = is_expired_by_ttl(time_to_live, ts, now);
        let invalid = is_invalid_entry(valid_after, ts);
        return (expired, invalid);
    }
    (false, false)
}

#[inline]
fn is_invalid_entry(valid_after: &Option<Instant>, entry_ts: Instant) -> bool {
    if let Some(va) = valid_after {
        if entry_ts < *va {
            return true;
        }
    }
    false
}

#[inline]
fn is_expired_by_tti(
    time_to_idle: &Option<Duration>,
    entry_last_accessed: Instant,
    now: Instant,
) -> bool {
    if let Some(tti) = time_to_idle {
        let checked_add = entry_last_accessed.checked_add(*tti);
        if checked_add.is_none() {
            panic!("tti overflow")
        }
        return checked_add.unwrap() <= now;
    }
    false
}

#[inline]
fn is_expired_by_ttl(
    time_to_live: &Option<Duration>,
    entry_last_modified: Instant,
    now: Instant,
) -> bool {
    if let Some(ttl) = time_to_live {
        let checked_add = entry_last_modified.checked_add(*ttl);
        if checked_add.is_none() {
            panic!("ttl overflow");
        }
        return checked_add.unwrap() <= now;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        common::concurrent::housekeeper, policy::ExpirationPolicy,
        stats::stats_counter::DisabledStatsCounter,
    };

    use super::BaseCache;

    #[cfg_attr(target_pointer_width = "16", ignore)]
    #[test]
    fn test_skt_capacity_will_not_overflow() {
        use std::collections::hash_map::RandomState;

        // power of two
        let pot = |exp| 2u64.pow(exp);

        let ensure_sketch_len = |max_capacity, len, name| {
            let stats_counter = Arc::<DisabledStatsCounter>::default();
            let cache = BaseCache::<u8, u8>::new(
                None,
                Some(max_capacity),
                None,
                RandomState::default(),
                None,
                None,
                None,
                Default::default(),
                stats_counter,
                false,
                housekeeper::Configuration::new_thread_pool(true),
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

    #[test]
    fn test_per_entry_expiration() {
        use super::InnerSync;
        use crate::{common::time::Clock, Entry, Expiry};

        use std::{
            collections::hash_map::RandomState,
            sync::{Arc, Mutex},
            time::{Duration, Instant as StdInstant},
        };

        type Key = u32;
        type Value = char;

        fn current_time(cache: &BaseCache<Key, Value>) -> StdInstant {
            cache
                .inner
                .clocks()
                .to_std_instant(cache.current_time_from_expiration_clock())
        }

        fn insert(cache: &BaseCache<Key, Value>, key: Key, hash: u64, value: Value) {
            let (op, _now) = cache.do_insert_with_hash(Arc::new(key), hash, value);
            cache.write_op_ch.send(op).expect("Failed to send");
        }

        macro_rules! assert_params_eq {
            ($left:expr, $right:expr, $param_name:expr, $line:expr) => {
                assert_eq!(
                    $left, $right,
                    "Mismatched `{}`s. line: {}",
                    $param_name, $line
                );
            };
        }

        macro_rules! assert_expiry {
            ($cache:ident, $key:ident, $hash:ident, $mock:ident, $duration_secs:expr) => {
                // Increment the time.
                $mock.increment(Duration::from_millis($duration_secs * 1000 - 1));
                $cache.inner.sync(1);
                assert!($cache.contains_key_with_hash(&$key, $hash));
                assert_eq!($cache.entry_count(), 1);

                // Increment the time by 1ms (3). The entry should be expired.
                $mock.increment(Duration::from_millis(1));
                $cache.inner.sync(1);
                assert!(!$cache.contains_key_with_hash(&$key, $hash));

                // Increment the time again to ensure the entry has been evicted from the
                // cache.
                $mock.increment(Duration::from_secs(1));
                $cache.inner.sync(1);
                assert_eq!($cache.entry_count(), 0);
            };
        }

        /// Contains expected call parameters and also a return value.
        #[derive(Debug)]
        enum ExpiryExpectation {
            NoCall,
            AfterCreate {
                caller_line: u32,
                key: Key,
                value: Value,
                current_time: StdInstant,
                new_duration_secs: Option<u64>,
            },
            AfterRead {
                caller_line: u32,
                key: Key,
                value: Value,
                current_time: StdInstant,
                current_duration_secs: Option<u64>,
                last_modified_at: StdInstant,
                new_duration_secs: Option<u64>,
            },
            AfterUpdate {
                caller_line: u32,
                key: Key,
                value: Value,
                current_time: StdInstant,
                current_duration_secs: Option<u64>,
                new_duration_secs: Option<u64>,
            },
        }

        impl ExpiryExpectation {
            fn after_create(
                caller_line: u32,
                key: Key,
                value: Value,
                current_time: StdInstant,
                new_duration_secs: Option<u64>,
            ) -> Self {
                Self::AfterCreate {
                    caller_line,
                    key,
                    value,
                    current_time,
                    new_duration_secs,
                }
            }

            fn after_read(
                caller_line: u32,
                key: Key,
                value: Value,
                current_time: StdInstant,
                current_duration_secs: Option<u64>,
                last_modified_at: StdInstant,
                new_duration_secs: Option<u64>,
            ) -> Self {
                Self::AfterRead {
                    caller_line,
                    key,
                    value,
                    current_time,
                    current_duration_secs,
                    last_modified_at,
                    new_duration_secs,
                }
            }

            fn after_update(
                caller_line: u32,
                key: Key,
                value: Value,
                current_time: StdInstant,
                current_duration_secs: Option<u64>,
                new_duration_secs: Option<u64>,
            ) -> Self {
                Self::AfterUpdate {
                    caller_line,
                    key,
                    value,
                    current_time,
                    current_duration_secs,
                    new_duration_secs,
                }
            }
        }

        let expectation = Arc::new(Mutex::new(ExpiryExpectation::NoCall));

        struct MyExpiry {
            expectation: Arc<Mutex<ExpiryExpectation>>,
        }

        impl Expiry<u32, char> for MyExpiry {
            fn expire_after_create(
                &self,
                actual_key: &u32,
                actual_value: &char,
                actual_current_time: StdInstant,
            ) -> Option<Duration> {
                use ExpiryExpectation::*;

                let lock = &mut *self.expectation.lock().unwrap();
                let expected = std::mem::replace(lock, NoCall);
                match expected {
                    AfterCreate {
                        caller_line,
                        key,
                        value,
                        current_time,
                        new_duration_secs: new_duration,
                    } => {
                        assert_params_eq!(*actual_key, key, "key", caller_line);
                        assert_params_eq!(*actual_value, value, "value", caller_line);
                        assert_params_eq!(
                            actual_current_time,
                            current_time,
                            "current_time",
                            caller_line
                        );
                        new_duration.map(Duration::from_secs)
                    }
                    expected => {
                        panic!("Unexpected call to expire_after_create: caller_line {}, expected: {:?}",
                            line!(), expected
                        );
                    }
                }
            }

            fn expire_after_read(
                &self,
                actual_key: &u32,
                actual_value: &char,
                actual_current_time: StdInstant,
                actual_current_duration: Option<Duration>,
                actual_last_modified_at: StdInstant,
            ) -> Option<Duration> {
                use ExpiryExpectation::*;

                let lock = &mut *self.expectation.lock().unwrap();
                let expected = std::mem::replace(lock, NoCall);
                match expected {
                    AfterRead {
                        caller_line,
                        key,
                        value,
                        current_time,
                        current_duration_secs,
                        last_modified_at,
                        new_duration_secs,
                    } => {
                        assert_params_eq!(*actual_key, key, "key", caller_line);
                        assert_params_eq!(*actual_value, value, "value", caller_line);
                        assert_params_eq!(
                            actual_current_time,
                            current_time,
                            "current_time",
                            caller_line
                        );
                        assert_params_eq!(
                            actual_current_duration,
                            current_duration_secs.map(Duration::from_secs),
                            "current_duration",
                            caller_line
                        );
                        assert_params_eq!(
                            actual_last_modified_at,
                            last_modified_at,
                            "last_modified_at",
                            caller_line
                        );
                        new_duration_secs.map(Duration::from_secs)
                    }
                    expected => {
                        panic!(
                            "Unexpected call to expire_after_read: caller_line {}, expected: {:?}",
                            line!(),
                            expected
                        );
                    }
                }
            }

            fn expire_after_update(
                &self,
                actual_key: &u32,
                actual_value: &char,
                actual_current_time: StdInstant,
                actual_current_duration: Option<Duration>,
            ) -> Option<Duration> {
                use ExpiryExpectation::*;

                let lock = &mut *self.expectation.lock().unwrap();
                let expected = std::mem::replace(lock, NoCall);
                match expected {
                    AfterUpdate {
                        caller_line,
                        key,
                        value,
                        current_time,
                        current_duration_secs,
                        new_duration_secs,
                    } => {
                        assert_params_eq!(*actual_key, key, "key", caller_line);
                        assert_params_eq!(*actual_value, value, "value", caller_line);
                        assert_params_eq!(
                            actual_current_time,
                            current_time,
                            "current_time",
                            caller_line
                        );
                        assert_params_eq!(
                            actual_current_duration,
                            current_duration_secs.map(Duration::from_secs),
                            "current_duration",
                            caller_line
                        );
                        new_duration_secs.map(Duration::from_secs)
                    }
                    expected => {
                        panic!("Unexpected call to expire_after_update: caller_line {}, expected: {:?}",
                            line!(), expected
                        );
                    }
                }
            }
        }

        const TTL: u64 = 16;
        const TTI: u64 = 7;
        let expiry: Option<Arc<dyn Expiry<_, _> + Send + Sync + 'static>> =
            Some(Arc::new(MyExpiry {
                expectation: Arc::clone(&expectation),
            }));

        let stats_counter = Arc::<DisabledStatsCounter>::default();
        let mut cache = BaseCache::<Key, Value>::new(
            None,
            None,
            None,
            RandomState::default(),
            None,
            None,
            None,
            ExpirationPolicy::new(
                Some(Duration::from_secs(TTL)),
                Some(Duration::from_secs(TTI)),
                expiry,
            ),
            stats_counter,
            false,
            housekeeper::Configuration::new_blocking(),
        );
        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        mock.increment(Duration::from_millis(10));

        // ----------------------------------------------------
        // Case 1
        //
        // 1.  0s: Insert with per-entry TTL 1s.
        // 2. +1s: Expires.
        // ----------------------------------------------------

        // Insert an entry (1). It will have a per-entry TTL of 1 second.
        let key = 1;
        let hash = cache.hash(&key);
        let value = 'a';

        *expectation.lock().unwrap() =
            ExpiryExpectation::after_create(line!(), key, value, current_time(&cache), Some(1));

        insert(&cache, key, hash, value);
        // Run a sync to register the entry to the internal data structures including
        // the timer wheel.
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        assert_expiry!(cache, key, hash, mock, 1);

        // ----------------------------------------------------
        // Case 2
        //
        // 1.  0s: Insert with no per-entry TTL.
        // 2. +1s: Get with per-entry TTL 3s.
        // 3. +3s: Expires.
        // ----------------------------------------------------

        // Insert an entry (1).
        let key = 2;
        let hash = cache.hash(&key);
        let value = 'b';

        *expectation.lock().unwrap() =
            ExpiryExpectation::after_create(line!(), key, value, current_time(&cache), None);
        let inserted_at = current_time(&cache);
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        // Increment the time.
        mock.increment(Duration::from_secs(1));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));

        // Read the entry (2).
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 1),
            inserted_at,
            Some(3),
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        assert_expiry!(cache, key, hash, mock, 3);

        // ----------------------------------------------------
        // Case 3
        //
        // 1.  0s: Insert with no per-entry TTL.
        // 2. +1s: Get with no per-entry TTL.
        // 3. +2s: Update with per-entry TTL 3s.
        // 4. +3s: Expires.
        // ----------------------------------------------------

        // Insert an entry (1).
        let key = 3;
        let hash = cache.hash(&key);
        let value = 'c';

        *expectation.lock().unwrap() =
            ExpiryExpectation::after_create(line!(), key, value, current_time(&cache), None);
        let inserted_at = current_time(&cache);
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        // Increment the time.
        mock.increment(Duration::from_secs(1));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));

        // Read the entry (2).
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 1),
            inserted_at,
            None,
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        // Increment the time.
        mock.increment(Duration::from_secs(2));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Update the entry (3).
        *expectation.lock().unwrap() = ExpiryExpectation::after_update(
            line!(),
            key,
            value,
            current_time(&cache),
            // TTI should be reset by this update.
            Some(TTI),
            Some(3),
        );
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        assert_expiry!(cache, key, hash, mock, 3);

        // ----------------------------------------------------
        // Case 4
        //
        // 1.  0s: Insert with no per-entry TTL.
        // 2. +1s: Get with no per-entry TTL.
        // 3. +2s: Update with no per-entry TTL.
        // 4. +7s: Expires by TTI (7s from step 3).
        // ----------------------------------------------------

        // Insert an entry (1).
        let key = 4;
        let hash = cache.hash(&key);
        let value = 'd';

        *expectation.lock().unwrap() =
            ExpiryExpectation::after_create(line!(), key, value, current_time(&cache), None);
        let inserted_at = current_time(&cache);
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        // Increment the time.
        mock.increment(Duration::from_secs(1));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Read the entry (2).
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 1),
            inserted_at,
            None,
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        // Increment the time.
        mock.increment(Duration::from_secs(2));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Update the entry (3).
        *expectation.lock().unwrap() = ExpiryExpectation::after_update(
            line!(),
            key,
            value,
            current_time(&cache),
            // TTI should be reset by this update.
            Some(TTI),
            None,
        );
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        assert_expiry!(cache, key, hash, mock, 7);

        // ----------------------------------------------------
        // Case 5
        //
        // 1.  0s: Insert with per-entry TTL 8s.
        // 2. +5s: Get with per-entry TTL 8s.
        // 3. +7s: Expires by TTI (7s).
        // ----------------------------------------------------

        // Insert an entry.
        let key = 5;
        let hash = cache.hash(&key);
        let value = 'e';

        *expectation.lock().unwrap() =
            ExpiryExpectation::after_create(line!(), key, value, current_time(&cache), Some(8));
        let inserted_at = current_time(&cache);
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        // Increment the time.
        mock.increment(Duration::from_secs(5));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Read the entry.
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 5),
            inserted_at,
            Some(8),
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        assert_expiry!(cache, key, hash, mock, 7);

        // ----------------------------------------------------
        // Case 6
        //
        // 1.  0s: Insert with per-entry TTL 8s.
        // 2. +5s: Get with per-entry TTL 9s.
        // 3. +6s: Get with per-entry TTL 10s.
        // 4. +5s: Expires by TTL (16s).
        // ----------------------------------------------------

        // Insert an entry.
        let key = 6;
        let hash = cache.hash(&key);
        let value = 'f';

        *expectation.lock().unwrap() =
            ExpiryExpectation::after_create(line!(), key, value, current_time(&cache), Some(8));
        let inserted_at = current_time(&cache);
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        // Increment the time.
        mock.increment(Duration::from_secs(5));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Read the entry.
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 5),
            inserted_at,
            Some(9),
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        // Increment the time.
        mock.increment(Duration::from_secs(6));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Read the entry.
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 6),
            inserted_at,
            Some(10),
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        assert_expiry!(cache, key, hash, mock, 5);

        // ----------------------------------------------------
        // Case 7
        //
        // 1.   0s: Insert with per-entry TTL 9s.
        // 2.  +6s: Update with per-entry TTL 8s.
        // 3.  +6s: Get with per-entry TTL 9s
        // 4.  +6s: Get with per-entry TTL 5s.
        // 5.  +4s: Expires by TTL (16s from step 2).
        // ----------------------------------------------------
        // Insert an entry.
        let key = 7;
        let hash = cache.hash(&key);
        let value = 'g';

        *expectation.lock().unwrap() =
            ExpiryExpectation::after_create(line!(), key, value, current_time(&cache), Some(9));
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        // Increment the time.
        mock.increment(Duration::from_secs(6));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Update the entry (3).
        *expectation.lock().unwrap() = ExpiryExpectation::after_update(
            line!(),
            key,
            value,
            current_time(&cache),
            // From the per-entry TTL.
            Some(9 - 6),
            Some(8),
        );
        let updated_at = current_time(&cache);
        insert(&cache, key, hash, value);
        cache.inner.sync(1);
        assert_eq!(cache.entry_count(), 1);

        // Increment the time.
        mock.increment(Duration::from_secs(6));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Read the entry.
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 6),
            updated_at,
            Some(9),
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        // Increment the time.
        mock.increment(Duration::from_secs(6));
        cache.inner.sync(1);
        assert!(cache.contains_key_with_hash(&key, hash));
        assert_eq!(cache.entry_count(), 1);

        // Read the entry.
        *expectation.lock().unwrap() = ExpiryExpectation::after_read(
            line!(),
            key,
            value,
            current_time(&cache),
            Some(TTI - 6),
            updated_at,
            Some(5),
        );
        assert_eq!(
            cache
                .get_with_hash(&key, hash, false)
                .map(Entry::into_value),
            Some(value)
        );
        cache.inner.sync(1);

        assert_expiry!(cache, key, hash, mock, 4);
    }
}
