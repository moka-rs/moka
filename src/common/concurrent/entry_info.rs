use std::sync::atomic::{self, AtomicBool, AtomicU16, AtomicU32, Ordering};

use super::{AccessTime, KeyHash};
use crate::common::time::{AtomicInstant, Instant};
use portable_atomic::AtomicU64;

#[derive(Debug)]
pub(crate) struct EntryInfo<K> {
    key_hash: KeyHash<K>,
    /// `is_admitted` indicates that the entry has been admitted to the cache. When
    /// `false`, it means the entry is _temporary_ admitted to the cache or evicted
    /// from the cache (so it should not have LRU nodes).
    is_admitted: AtomicBool,
    /// `entry_gen` (entry generation) is incremented every time the entry is updated
    /// in the concurrent hash table.
    entry_gen: AtomicU16,
    /// `policy_gen` (policy generation) is incremented every time entry's `WriteOp`
    /// is applied to the cache policies including the access-order queue (the LRU
    /// deque).
    policy_gen: AtomicU16,
    /// Packed expiration state: contains both `expiration_time` (upper 52 bits) and
    /// `expiry_gen` (lower 12 bits) in a single atomic u64 for consistent reads.
    /// Special value `u64::MAX` means expiration_time is None.
    expiration_state: AtomicU64,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
    policy_weight: AtomicU32,
}

impl<K> EntryInfo<K> {
    #[inline]
    pub(crate) fn new(key_hash: KeyHash<K>, timestamp: Instant, policy_weight: u32) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        super::debug_counters::InternalGlobalDebugCounters::entry_info_created();

        Self {
            key_hash,
            is_admitted: AtomicBool::default(),
            // `entry_gen` starts at 1 and `policy_gen` start at 0.
            entry_gen: AtomicU16::new(1),
            policy_gen: AtomicU16::new(0),
            expiration_state: AtomicU64::new(u64::MAX), // Initial state: None, gen=0
            last_accessed: AtomicInstant::new(timestamp),
            last_modified: AtomicInstant::new(timestamp),
            policy_weight: AtomicU32::new(policy_weight),
        }
    }

    #[inline]
    pub(crate) fn key_hash(&self) -> &KeyHash<K> {
        &self.key_hash
    }

    #[inline]
    pub(crate) fn is_admitted(&self) -> bool {
        self.is_admitted.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn set_admitted(&self, value: bool) {
        self.is_admitted.store(value, Ordering::Release);
    }

    /// Returns `true` if the `ValueEntry` having this `EntryInfo` is dirty.
    ///
    /// Dirty means that the entry has been updated in the concurrent hash table but
    /// not yet in the cache policies such as access-order queue.
    #[inline]
    pub(crate) fn is_dirty(&self) -> bool {
        let result =
            self.entry_gen.load(Ordering::Relaxed) != self.policy_gen.load(Ordering::Relaxed);
        atomic::fence(Ordering::Acquire);
        result
    }

    #[inline]
    pub(crate) fn entry_gen(&self) -> u16 {
        self.entry_gen.load(Ordering::Acquire)
    }

    /// Increments the entry generation and returns the new value.
    #[inline]
    pub(crate) fn incr_entry_gen(&self) -> u16 {
        // NOTE: This operation wraps around on overflow.
        let prev = self.entry_gen.fetch_add(1, Ordering::AcqRel);
        // Need to add `1` to the previous value to get the current value.
        prev.wrapping_add(1)
    }

    /// Sets the policy generation to the given value.
    #[inline]
    pub(crate) fn set_policy_gen(&self, value: u16) {
        let g = &self.policy_gen;
        loop {
            let current = g.load(Ordering::Acquire);

            // Do not set the given value if it is smaller than the current value of
            // `policy_gen`. Note that the current value may have been wrapped
            // around. If the value is much larger than the current value, it is
            // likely that the value of `policy_gen` has been wrapped around.
            if current >= value || value.wrapping_sub(current) > u16::MAX / 2 {
                break;
            }

            // Try to set the value.
            if g.compare_exchange_weak(current, value, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        self.policy_weight.load(Ordering::Acquire)
    }

    pub(crate) fn set_policy_weight(&self, size: u32) {
        self.policy_weight.store(size, Ordering::Release);
    }

    /// Atomically reads both `expiration_time` and `expiry_gen` as a single unit.
    /// Returns `(expiration_time, expiry_gen)` where expiration_time is None if unset.
    ///
    /// This provides a consistent snapshot of the expiration state, avoiding TOCTOU
    /// issues where the time and generation could be read separately while being
    /// modified by another thread.
    #[inline]
    pub(crate) fn expiration_state(&self) -> (Option<Instant>, u32) {
        const GEN_MASK: u64 = 0xFFF;
        const TIME_MASK: u64 = u64::MAX ^ GEN_MASK;

        let packed = self.expiration_state.load(Ordering::Acquire);

        if packed == u64::MAX {
            // Expiration time is None
            (None, (packed & GEN_MASK) as u32)
        } else {
            let time_nanos = packed & TIME_MASK;
            let gen = (packed & GEN_MASK) as u32;
            (Some(Instant::from_nanos(time_nanos)), gen)
        }
    }

    /// Sets the expiration time and returns the new expiry generation.
    pub(crate) fn set_expiration_time(&self, time: Option<Instant>) -> u32 {
        const GEN_MASK: u64 = 0xFFF;

        // Use compare_exchange to atomically update the expiration state.
        // This prevents race conditions where multiple threads try to update
        // the expiration time simultaneously.
        loop {
            let prev_packed = self.expiration_state.load(Ordering::Acquire);

            // Extract previous generation (handle unset state)
            let prev_gen = if prev_packed == u64::MAX {
                0
            } else {
                (prev_packed & GEN_MASK) as u32
            };
            let new_gen = prev_gen.wrapping_add(1) & GEN_MASK as u32;

            // Pack the new state
            let new_packed = if let Some(t) = time {
                let nanos = t.as_nanos();
                debug_assert!(
                    nanos != u64::MAX,
                    "Instant value conflicts with unset marker"
                );
                // Pack: store nanos in upper 52 bits, gen in lower 12 bits
                (nanos & !GEN_MASK) | (new_gen as u64)
            } else {
                u64::MAX // Special value for None
            };

            // Try to atomically update if the state hasn't changed
            match self.expiration_state.compare_exchange_weak(
                prev_packed,
                new_packed,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return new_gen, // Successfully updated
                Err(_) => continue,      // State changed, retry
            }
        }
    }
}

#[cfg(feature = "unstable-debug-counters")]
impl<K> Drop for EntryInfo<K> {
    fn drop(&mut self) {
        super::debug_counters::InternalGlobalDebugCounters::entry_info_dropped();
    }
}

impl<K> AccessTime for EntryInfo<K> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.last_accessed.instant()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.last_accessed.set_instant(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.last_modified.instant()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.last_modified.set_instant(timestamp);
    }
}
