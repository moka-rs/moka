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
    ///
    /// This is distinct from [`is_retired`](Self::is_retired): `is_admitted`
    /// tracks deque membership, while `is_retired` tracks CHT membership. A
    /// freshly inserted entry is **not retired** and **not admitted** until its
    /// `WriteOp::Upsert` is drained; a retired entry may still be `is_admitted`
    /// until `handle_remove*` unlinks it.
    is_admitted: AtomicBool,
    /// CHT-membership flag: `false` while the entry is alive (still backed by
    /// the concurrent hash table), `true` once the entry has been removed.
    ///
    /// The flag is set by `retire()` at the moment the entry is unlinked from
    /// the CHT and is never cleared (`false → true`, monotonic). The
    /// invariant it enforces is:
    ///
    /// > If `is_retired == true`, no policy-tier mutation may run against
    /// > this `EntryInfo`.
    ///
    /// Examples of policy-tier mutations:
    ///
    /// - admitting the entry to an LRU deque,
    /// - moving its deque node to the back,
    /// - scheduling a timer-wheel node,
    /// - incrementing `weighted_size`.
    ///
    /// ### Why we need it
    ///
    /// moka stores entries in a lock-free CHT (source of truth) and mirrors
    /// their ordering into LRU deques updated asynchronously via a bounded
    /// `WriteOp` channel. Without this flag, a `WriteOp::Upsert` can sit in the
    /// channel long enough that the backing CHT entry is evicted before the op
    /// is drained; the drain then pushes an "orphan" deque node that the
    /// eviction loop cannot reach, blocking progress and stalling eviction
    /// indefinitely (see <https://github.com/moka-rs/moka/issues/590>). The
    /// retire store is performed inside the CHT's post-success
    /// `with_previous_entry` callback so the transition linearises with the
    /// bucket-pointer CAS that physically unlinked the entry; any consumer
    /// draining a stale op thereafter observes `is_retired == true` and
    /// declines to touch policy state.
    ///
    /// ### Why a single bit suffices (no `Dead` state)
    ///
    /// Caffeine encodes a three-state lifecycle (`Alive → Retired → Dead`)
    /// because its `makeDead()` can be called twice for the same node: once
    /// inline from `evictEntry()` during admission, and again later from
    /// `RemovalTask.run()` draining the write buffer. The `isDead()` guard at
    /// the top of `makeDead` prevents double-subtraction of `weightedSize`.
    ///
    /// moka has no such double-enter path: every CHT removal site gates its
    /// `handle_remove_*` call on `if let Some(entry) = …`; i.e. on winning
    /// the CHT bucket CAS. So `handle_remove_*` runs exactly once per
    /// retirement, structurally.
    ///
    /// ### When a third state would become necessary
    ///
    /// A third state (`Dead`) earns its keep when there are **multiple
    /// finalization paths** that may both target the same entry. Currently,
    /// moka has one at the CAS-gated inline `handle_remove_*`.
    ///
    /// If moka ever grows a second finalization path that runs alongside the
    /// `WriteOp::Remove` drain (e.g., an inline emergency-evict, an
    /// async load-completion that touches policy state, or any code that
    /// can re-enter `handle_remove_*` for an already-retired entry), the
    /// idempotency requirement returns and a third state (or an equivalent
    /// "weight already subtracted" marker) becomes necessary. Until then, a
    /// single bit is sufficient.
    is_retired: AtomicBool,
    /// `entry_gen` (entry generation) is incremented every time the entry is updated
    /// in the concurrent hash table.
    entry_gen: AtomicU16,
    /// `policy_gen` (policy generation) is incremented every time entry's `WriteOp`
    /// is applied to the cache policies including the access-order queue (the LRU
    /// deque).
    policy_gen: AtomicU16,
    /// Packed expiration state: contains both `expiration_time` (upper 52 bits) and
    /// `expiry_gen` (lower 12 bits) in a single atomic u64 for consistent reads.
    ///
    /// Encoding:
    /// - Bits 0-11: 12-bit expiry_gen (wraps from 4095 to 0)
    /// - Bits 12-63: 52-bit expiration timestamp (nanoseconds from a monotonic clock/Instant)
    /// - Sentinel: When time field (bits 12-63) is all 1s (0xFFFF_FFFF_FFFF_F000),
    ///   represents "None" (no expiration). Gen bits are always preserved.
    /// - Valid timestamps are clamped to 52-bit range to avoid collisions with sentinel.
    ///
    /// NOTE: The time value is relative to the runtime's monotonic clock (not Unix epoch).
    /// It is obtained from `Instant` and should not be interpreted as an absolute time
    /// suitable for serialization or cross-process comparison.
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
            is_retired: AtomicBool::default(),
            // `entry_gen` starts at 1 and `policy_gen` start at 0.
            entry_gen: AtomicU16::new(1),
            policy_gen: AtomicU16::new(0),
            // Initial state: None (time field all 1s), gen=0
            expiration_state: AtomicU64::new(0xFFFF_FFFF_FFFF_F000),
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

    /// Returns `true` iff the entry has been retired (removed from the CHT).
    #[inline]
    pub(crate) fn is_retired(&self) -> bool {
        self.is_retired.load(Ordering::Acquire)
    }

    /// Returns `true` iff the entry is still alive (not yet retired).
    /// Consumers of buffered operations (`WriteOp::Upsert`, `ReadOp::Hit`,
    /// TinyLFU inline victim scans) MUST check this before mutating policy
    /// state; see the `is_retired` field docs.
    #[inline]
    pub(crate) fn is_alive(&self) -> bool {
        !self.is_retired()
    }

    /// Transitions the entry to the retired state. Must be called inside the
    /// post-CAS `with_previous_entry` callback of `cht::remove_entry_if_and`
    /// (or an equivalent linearisation point) so the transition is atomic
    /// with the CHT unlink.
    ///
    /// The callsite is exclusive: the `with_previous_entry` closure fires at
    /// most once per `EntryInfo` (it only runs for the thread that won the
    /// bucket-pointer CAS), so no other thread can be transitioning the same
    /// field in parallel. A plain `Release` store is therefore sufficient
    /// (no RMW needed) and matches the `is_admitted` pattern. Double-retire
    /// would indicate a broken invariant (caught by a debug-mode assertion).
    #[inline]
    pub(crate) fn retire(&self) {
        debug_assert!(
            !self.is_retired.load(Ordering::Acquire),
            "retire() called on already-retired entry; lifecycle invariant broken",
        );
        self.is_retired.store(true, Ordering::Release);
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
        const TIME_MASK: u64 = 0xFFFF_FFFF_FFFF_F000;

        let packed = self.expiration_state.load(Ordering::Acquire);

        // Extract time field and gen bits
        let time_nanos = packed & TIME_MASK;
        let gen = (packed & GEN_MASK) as u32;

        // Check if time field (upper 52 bits) is all 1s (sentinel for None)
        if time_nanos == TIME_MASK {
            (None, gen)
        } else {
            (Some(Instant::from_nanos(time_nanos)), gen)
        }
    }

    /// Sets the expiration time and returns the new expiry generation.
    pub(crate) fn set_expiration_time(&self, time: Option<Instant>) -> u32 {
        const GEN_MASK: u64 = 0xFFF;
        const TIME_MASK: u64 = 0xFFFF_FFFF_FFFF_F000;

        // Use compare_exchange to atomically update the expiration state.
        // This prevents race conditions where multiple threads try to update
        // the expiration time simultaneously.
        loop {
            let prev_packed = self.expiration_state.load(Ordering::Acquire);

            // Extract previous generation (always preserved, even for None state)
            let prev_gen = (prev_packed & GEN_MASK) as u32;
            let new_gen = prev_gen.wrapping_add(1) & GEN_MASK as u32;

            // Pack the new state
            let new_packed = if let Some(t) = time {
                // Clamp timestamp to 52-bit range. Ensure it's strictly less than TIME_MASK
                // to avoid collision with the sentinel (None) value.
                let mut nanos = t.as_nanos() & TIME_MASK;
                // If nanos equals TIME_MASK, adjust it down by one time unit (4096 nanos)
                // to avoid corrupting the generation counter bits (should never happen in practice).
                if nanos == TIME_MASK {
                    nanos = TIME_MASK - 0x1000; // Subtract one 12-bit unit to keep lower bits clear
                }
                debug_assert!(nanos < TIME_MASK, "Timestamp value collides with sentinel");
                // Pack: store nanos in upper 52 bits, gen in lower 12 bits
                nanos | (new_gen as u64)
            } else {
                // Sentinel: time field all 1s, gen preserved in lower bits
                TIME_MASK | (new_gen as u64)
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

#[cfg(test)]
mod tests {
    use super::{EntryInfo, KeyHash};
    use crate::common::time::Instant;
    use std::sync::Arc;

    fn make_entry_info() -> EntryInfo<u64> {
        let kh = KeyHash::new(Arc::new(42u64), 0xdead_beef);
        EntryInfo::new(kh, Instant::from_nanos(0), 1)
    }

    #[test]
    fn lifecycle_initial_is_alive() {
        let info = make_entry_info();
        assert!(info.is_alive());
        assert!(!info.is_retired());
    }

    #[test]
    fn lifecycle_retire_transitions_to_retired() {
        let info = make_entry_info();
        info.retire();
        assert!(!info.is_alive());
        assert!(info.is_retired());
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "retire() called on already-retired entry")]
    fn lifecycle_double_retire_debug() {
        let info = make_entry_info();
        info.retire();
        // Second retire violates the exclusive-call invariant; debug-asserted.
        info.retire();
    }
}
