//! Admission types and logic for concurrent caches.
//!
//! This module provides types and functions for cache admission decisions
//! in the TinyLFU eviction policy.

use crate::{
    cht::SegmentedHashMap,
    common::{
        concurrent::{arc::MiniArc, deques::Deques, AccessTime, KeyHash, KeyHashDate, ValueEntry},
        deque::{DeqNode, Deque},
        frequency_sketch::FrequencySketch,
        time::Instant,
        timer_wheel::{ReschedulingResult, TimerWheel},
        CacheRegion,
    },
};

use smallvec::SmallVec;
use std::{hash::BuildHasher, sync::Arc};

// ============================================================================
// Types
// ============================================================================

/// Counters for tracking cache eviction statistics.
pub(crate) struct EvictionCounters {
    pub(crate) entry_count: u64,
    pub(crate) weighted_size: u64,
    pub(crate) eviction_count: u64,
}

impl EvictionCounters {
    #[inline]
    pub(crate) fn new(entry_count: u64, weighted_size: u64) -> Self {
        Self {
            entry_count,
            weighted_size,
            eviction_count: 0,
        }
    }

    #[inline]
    pub(crate) fn saturating_add(&mut self, entry_count: u64, weight: u32) {
        self.entry_count += entry_count;
        let total = &mut self.weighted_size;
        *total = total.saturating_add(weight as u64);
    }

    #[inline]
    pub(crate) fn saturating_sub(&mut self, entry_count: u64, weight: u32) {
        self.entry_count -= entry_count;
        let total = &mut self.weighted_size;
        *total = total.saturating_sub(weight as u64);
    }

    #[inline]
    pub(crate) fn incr_eviction_count(&mut self) {
        let count = &mut self.eviction_count;
        *count = count.saturating_add(1);
    }
}

/// Aggregated metrics for admission decisions.
#[derive(Default)]
pub(crate) struct EntrySizeAndFrequency {
    pub(crate) policy_weight: u64,
    pub(crate) freq: u32,
}

impl EntrySizeAndFrequency {
    pub(crate) fn new(policy_weight: u32) -> Self {
        Self {
            policy_weight: policy_weight as u64,
            ..Default::default()
        }
    }

    pub(crate) fn add_policy_weight(&mut self, weight: u32) {
        self.policy_weight += weight as u64;
    }

    pub(crate) fn add_frequency(&mut self, freq: &FrequencySketch, hash: u64) {
        self.freq += freq.frequency(hash) as u32;
    }
}

/// Result of an admission decision.
///
/// NOTE: Clippy found that the `Admitted` variant contains at least a few hundred
/// bytes of data and the `Rejected` variant contains no data at all. It suggested to
/// box the `SmallVec`.
///
/// We ignore the suggestion because (1) the `SmallVec` is used to avoid heap
/// allocation as it will be used in a performance hot spot, and (2) this enum has a
/// very short lifetime and there will only one instance at a time.
#[allow(clippy::large_enum_variant)]
pub(crate) enum AdmissionResult<K> {
    Admitted {
        /// A vec of pairs of `KeyHash` and `last_accessed`.
        victim_keys: SmallVec<[(KeyHash<K>, Option<Instant>); 8]>,
    },
    Rejected,
}

// ============================================================================
// Logic Functions
// ============================================================================

/// Type alias for the cache hash map store.
type CacheStore<K, V, S> = SegmentedHashMap<Arc<K>, MiniArc<ValueEntry<K, V>>, S>;

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
/// Returns `AdmissionResult::Admitted` with victim keys if the candidate should be
/// admitted, or `AdmissionResult::Rejected` otherwise.
#[inline]
pub(crate) fn admit<K, V, S>(
    candidate: &EntrySizeAndFrequency,
    cache: &CacheStore<K, V, S>,
    deqs: &mut Deques<K>,
    freq: &FrequencySketch,
) -> AdmissionResult<K>
where
    K: std::hash::Hash + Eq,
    S: BuildHasher,
{
    const MAX_CONSECUTIVE_RETRIES: usize = 5;
    let mut retries = 0;

    let mut victims = EntrySizeAndFrequency::default();
    let mut victim_keys = SmallVec::default();

    let deq = &mut deqs.probation;

    // Get first potential victim at the LRU position.
    let mut next_victim = deq.peek_front_ptr();

    // Aggregate potential victims.
    while victims.policy_weight < candidate.policy_weight
        && victims.freq <= candidate.freq
        && retries <= MAX_CONSECUTIVE_RETRIES
    {
        let Some(victim) = next_victim.take() else {
            // No more potential victims.
            break;
        };
        next_victim = DeqNode::next_node_ptr(victim);

        let vic_elem = &unsafe { victim.as_ref() }.element;
        if vic_elem.is_dirty() {
            // Skip this node as its ValueEntry have been updated or invalidated.
            unsafe { deq.move_to_back(victim) };
            retries += 1;
            continue;
        }

        let key = vic_elem.key();
        let hash = vic_elem.hash();
        let last_accessed = vic_elem.entry_info().last_accessed();

        if let Some(vic_entry) = cache.get(hash, |k| k == key) {
            victims.add_policy_weight(vic_entry.policy_weight());
            victims.add_frequency(freq, hash);
            victim_keys.push((KeyHash::new(Arc::clone(key), hash), last_accessed));
            retries = 0;
        } else {
            // Could not get the victim from the cache (hash map). Skip this node
            // as its ValueEntry might have been invalidated (after we checked
            // `is_dirty` above`).
            unsafe { deq.move_to_back(victim) };
            retries += 1;
        }
    }

    // Admit or reject the candidate.

    // TODO: Implement some randomness to mitigate hash DoS attack.
    // See Caffeine's implementation.

    if victims.policy_weight >= candidate.policy_weight && candidate.freq > victims.freq {
        AdmissionResult::Admitted { victim_keys }
    } else {
        AdmissionResult::Rejected
    }
}

/// Handles admission of an entry to the cache.
///
/// Updates counters, timer wheel, and deques for the newly admitted entry.
#[inline]
pub(crate) fn handle_admit<K, V>(
    entry: &MiniArc<ValueEntry<K, V>>,
    policy_weight: u32,
    is_write_order_queue_enabled: bool,
    deqs: &mut Deques<K>,
    timer_wheel: &mut TimerWheel<K>,
    counters: &mut EvictionCounters,
) {
    counters.saturating_add(1, policy_weight);

    update_timer_wheel(entry, timer_wheel);

    // Update the deques.
    deqs.push_back_ao(
        CacheRegion::MainProbation,
        KeyHashDate::new(entry.entry_info()),
        entry,
    );
    if is_write_order_queue_enabled {
        deqs.push_back_wo(KeyHashDate::new(entry.entry_info()), entry);
    }
    entry.set_admitted(true);
}

/// Updates the timer wheel for an entry.
///
/// NOTE: This function may enable the timer wheel.
#[inline]
pub(crate) fn update_timer_wheel<K, V>(
    entry: &MiniArc<ValueEntry<K, V>>,
    timer_wheel: &mut TimerWheel<K>,
) {
    // Atomically read both expiration_time and expiry_gen as a single unit
    // to ensure consistent state and avoid TOCTOU issues.
    let (expiration_time, current_expiry_gen) = entry.entry_info().expiration_state();
    // Enable the timer wheel if needed.
    if expiration_time.is_some() && !timer_wheel.is_enabled() {
        timer_wheel.enable();
    }

    // Get timer_node with its expiry generation to detect stale pointers.
    let (timer_node, expected_expiry_gen) = entry.timer_node_with_expiry_gen();

    // Update the timer wheel.
    match (expiration_time.is_some(), timer_node) {
        // Do nothing; the cache entry has no expiration time and not registered
        // to the timer wheel.
        (false, None) => (),
        // Register the cache entry to the timer wheel; the cache entry has an
        // expiration time and not registered to the timer wheel.
        (true, None) => {
            let timer = timer_wheel.schedule(
                MiniArc::clone(entry.entry_info()),
                MiniArc::clone(entry.deq_nodes()),
                current_expiry_gen,
            );
            entry.set_timer_node(timer, current_expiry_gen);
        }
        // Reschedule the cache entry in the timer wheel; the cache entry has an
        // expiration time and already registered to the timer wheel.
        (true, Some(tn)) => {
            // Reschedule with generation validation to prevent use-after-free
            match timer_wheel.reschedule(tn, expected_expiry_gen) {
                Some(ReschedulingResult::Removed(removed_tn)) => {
                    // The timer node was removed from the timer wheel because the
                    // expiration time has been unset by other thread after we
                    // checked.
                    entry.set_timer_node(None, current_expiry_gen);
                    drop(removed_tn);
                }
                Some(ReschedulingResult::Rescheduled) => {
                    // Successfully rescheduled, nothing to do.
                }
                None => {
                    // The timer node was invalid (stale - expiry gen mismatch).
                    // Clear the timer_node to prevent further issues.
                    entry.set_timer_node(None, current_expiry_gen);
                }
            }
        }
        // Unregister the cache entry from the timer wheel; the cache entry has
        // no expiration time but registered to the timer wheel.
        (false, Some(tn)) => {
            entry.set_timer_node(None, current_expiry_gen);
            // Returns false if the node was stale, but we've already
            // cleared timer_node above, so we can ignore the return value.
            let _ = timer_wheel.deschedule(tn, expected_expiry_gen);
        }
    }
}

/// Handles removal of an entry from the cache.
///
/// Removes the entry from timer wheel, deques, and updates counters.
#[inline]
pub(crate) fn handle_remove<K, V>(
    deqs: &mut Deques<K>,
    timer_wheel: &mut TimerWheel<K>,
    entry: MiniArc<ValueEntry<K, V>>,
    gen: Option<u16>,
    counters: &mut EvictionCounters,
) {
    // Take the timer node along with its stored expiry generation for validation.
    let (timer_node, expiry_gen) = entry.take_timer_node();
    if let Some(tn) = timer_node {
        // Returns false if the node was stale, but we've already
        // taken (cleared) the timer_node, so we can ignore the return value.
        let _ = timer_wheel.deschedule(tn, expiry_gen);
    }
    handle_remove_without_timer_wheel(deqs, entry, gen, counters);
}

/// Handles removal of an entry without updating the timer wheel.
#[inline]
pub(crate) fn handle_remove_without_timer_wheel<K, V>(
    deqs: &mut Deques<K>,
    entry: MiniArc<ValueEntry<K, V>>,
    gen: Option<u16>,
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
    if let Some(g) = gen {
        entry.entry_info().set_policy_gen(g);
    }
}

/// Handles removal of an entry with explicit deque references.
///
/// Used when the specific deques are already borrowed.
#[inline]
pub(crate) fn handle_remove_with_deques<K, V>(
    ao_deq_name: &str,
    ao_deq: &mut Deque<KeyHashDate<K>>,
    wo_deq: &mut Deque<KeyHashDate<K>>,
    timer_wheel: &mut TimerWheel<K>,
    entry: MiniArc<ValueEntry<K, V>>,
    counters: &mut EvictionCounters,
) {
    // Take the timer node along with its stored expiry generation for validation.
    let (timer_node, expiry_gen) = entry.take_timer_node();
    if let Some(timer) = timer_node {
        // Deschedule with generation validation to prevent use-after-free.
        // Returns false if the node was stale, but we've already
        // taken (cleared) the timer_node, so we can ignore the return value.
        let _ = timer_wheel.deschedule(timer, expiry_gen);
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
