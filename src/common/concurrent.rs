use crate::common::{
    concurrent::arc::MiniArc, deque::DeqNode, frequency_sketch::FrequencySketch, time::Instant,
};
use crate::policy::EvictionPolicyConfig;

use parking_lot::Mutex;
use std::{fmt, ptr::NonNull, sync::Arc};
use tagptr::TagNonNull;

/// The cost of an entry when no cost closure is set or the eviction policy does not
/// consult costs. It weights the entry's frequency by `1`, i.e. leaves it as-is.
pub(crate) const DEFAULT_COST: u32 = 1;

pub(crate) mod arc;
pub(crate) mod constants;
pub(crate) mod deques;
pub(crate) mod entry_info;

#[cfg(feature = "sync")]
pub(crate) mod housekeeper;

#[cfg(feature = "unstable-debug-counters")]
pub(crate) mod debug_counters;

use self::entry_info::EntryInfo;

use super::timer_wheel::TimerNode;

pub(crate) type Weigher<K, V> = Arc<dyn Fn(&K, &V) -> u32 + Send + Sync + 'static>;

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn set_last_accessed(&self, timestamp: Instant);
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_modified(&self, timestamp: Instant);
}

#[derive(Debug)]
pub(crate) struct KeyHash<K> {
    pub(crate) key: Arc<K>,
    pub(crate) hash: u64,
}

impl<K> KeyHash<K> {
    pub(crate) fn new(key: Arc<K>, hash: u64) -> Self {
        Self { key, hash }
    }
}

impl<K> Clone for KeyHash<K> {
    fn clone(&self) -> Self {
        Self {
            key: Arc::clone(&self.key),
            hash: self.hash,
        }
    }
}

pub(crate) struct KeyHashDate<K> {
    entry_info: MiniArc<EntryInfo<K>>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(entry_info: &MiniArc<EntryInfo<K>>) -> Self {
        Self {
            entry_info: MiniArc::clone(entry_info),
        }
    }

    pub(crate) fn key(&self) -> &Arc<K> {
        &self.entry_info.key_hash().key
    }

    pub(crate) fn hash(&self) -> u64 {
        self.entry_info.key_hash().hash
    }

    pub(crate) fn entry_info(&self) -> &EntryInfo<K> {
        &self.entry_info
    }

    pub(crate) fn last_modified(&self) -> Option<Instant> {
        self.entry_info.last_modified()
    }

    pub(crate) fn last_accessed(&self) -> Option<Instant> {
        self.entry_info.last_accessed()
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.entry_info.is_dirty()
    }
}

pub(crate) struct KvEntry<K, V> {
    pub(crate) key: Arc<K>,
    pub(crate) entry: MiniArc<ValueEntry<K, V>>,
}

impl<K, V> KvEntry<K, V> {
    pub(crate) fn new(key: Arc<K>, entry: MiniArc<ValueEntry<K, V>>) -> Self {
        Self { key, entry }
    }
}

impl<K, V> Clone for KvEntry<K, V> {
    fn clone(&self) -> Self {
        Self {
            key: Arc::clone(&self.key),
            entry: MiniArc::clone(&self.entry),
        }
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.element.entry_info.last_accessed()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.element.entry_info.set_last_accessed(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.element.entry_info.last_modified()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.element.entry_info.set_last_modified(timestamp);
    }
}

// DeqNode for an access order queue.
type KeyDeqNodeAo<K> = TagNonNull<DeqNode<KeyHashDate<K>>, 2>;

// DeqNode for the write order queue.
type KeyDeqNodeWo<K> = NonNull<DeqNode<KeyHashDate<K>>>;

// DeqNode for the timer wheel.
type DeqNodeTimer<K> = NonNull<DeqNode<TimerNode<K>>>;

pub(crate) struct DeqNodes<K> {
    access_order_q_node: Option<KeyDeqNodeAo<K>>,
    write_order_q_node: Option<KeyDeqNodeWo<K>>,
    timer_node: Option<DeqNodeTimer<K>>,
    /// The expiry generation when timer_node was set.
    /// Used to validate the timer_node hasn't become stale.
    timer_node_expiry_gen: u32,
}

impl<K> Default for DeqNodes<K> {
    fn default() -> Self {
        Self {
            access_order_q_node: None,
            write_order_q_node: None,
            timer_node: None,
            timer_node_expiry_gen: 0,
        }
    }
}

// We need this `unsafe impl` as DeqNodes have NonNull pointers.
unsafe impl<K> Send for DeqNodes<K> {}

impl<K> DeqNodes<K> {
    pub(crate) fn set_timer_node(&mut self, timer_node: Option<DeqNodeTimer<K>>, expiry_gen: u32) {
        self.timer_node = timer_node;
        self.timer_node_expiry_gen = expiry_gen;
    }

    pub(crate) fn timer_node_with_expiry_gen(&self) -> (Option<DeqNodeTimer<K>>, u32) {
        (self.timer_node, self.timer_node_expiry_gen)
    }
}

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: V,
    info: MiniArc<EntryInfo<K>>,
    nodes: MiniArc<Mutex<DeqNodes<K>>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(value: V, entry_info: MiniArc<EntryInfo<K>>) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        self::debug_counters::InternalGlobalDebugCounters::value_entry_created();

        Self {
            value,
            info: entry_info,
            nodes: MiniArc::new(Mutex::new(DeqNodes::default())),
        }
    }

    pub(crate) fn new_from(value: V, entry_info: MiniArc<EntryInfo<K>>, other: &Self) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        self::debug_counters::InternalGlobalDebugCounters::value_entry_created();
        Self {
            value,
            info: entry_info,
            nodes: MiniArc::clone(&other.nodes),
        }
    }

    pub(crate) fn entry_info(&self) -> &MiniArc<EntryInfo<K>> {
        &self.info
    }

    pub(crate) fn is_admitted(&self) -> bool {
        self.info.is_admitted()
    }

    pub(crate) fn set_admitted(&self, value: bool) {
        self.info.set_admitted(value);
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.info.is_dirty()
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        self.info.policy_weight()
    }

    #[inline]
    pub(crate) fn policy_cost(&self) -> u32 {
        self.info.policy_cost()
    }

    pub(crate) fn deq_nodes(&self) -> &MiniArc<Mutex<DeqNodes<K>>> {
        &self.nodes
    }

    pub(crate) fn access_order_q_node(&self) -> Option<KeyDeqNodeAo<K>> {
        self.nodes.lock().access_order_q_node
    }

    pub(crate) fn set_access_order_q_node(&self, node: Option<KeyDeqNodeAo<K>>) {
        self.nodes.lock().access_order_q_node = node;
    }

    pub(crate) fn take_access_order_q_node(&self) -> Option<KeyDeqNodeAo<K>> {
        self.nodes.lock().access_order_q_node.take()
    }

    pub(crate) fn write_order_q_node(&self) -> Option<KeyDeqNodeWo<K>> {
        self.nodes.lock().write_order_q_node
    }

    pub(crate) fn set_write_order_q_node(&self, node: Option<KeyDeqNodeWo<K>>) {
        self.nodes.lock().write_order_q_node = node;
    }

    pub(crate) fn take_write_order_q_node(&self) -> Option<KeyDeqNodeWo<K>> {
        self.nodes.lock().write_order_q_node.take()
    }

    /// Returns the timer node and its expected expiry generation for validation.
    pub(crate) fn timer_node_with_expiry_gen(&self) -> (Option<DeqNodeTimer<K>>, u32) {
        self.nodes.lock().timer_node_with_expiry_gen()
    }

    pub(crate) fn set_timer_node(&self, node: Option<DeqNodeTimer<K>>, expiry_gen: u32) {
        self.nodes.lock().set_timer_node(node, expiry_gen);
    }

    /// Takes the timer node and returns it along with its stored expiry generation.
    pub(crate) fn take_timer_node(&self) -> (Option<DeqNodeTimer<K>>, u32) {
        let mut nodes = self.nodes.lock();
        let expiry_gen = nodes.timer_node_expiry_gen;
        nodes.timer_node_expiry_gen = 0;
        (nodes.timer_node.take(), expiry_gen)
    }

    pub(crate) fn unset_q_nodes(&self) {
        let mut nodes = self.nodes.lock();
        nodes.access_order_q_node = None;
        nodes.write_order_q_node = None;
    }
}

#[cfg(feature = "unstable-debug-counters")]
impl<K, V> Drop for ValueEntry<K, V> {
    fn drop(&mut self) {
        self::debug_counters::InternalGlobalDebugCounters::value_entry_dropped();
    }
}

/// A running aggregate of policy weight and frequency, used by the admission logic
/// to compare a candidate entry against its potential victims. For the cost-aware
/// policy the frequency is weighted by each entry's cost.
#[derive(Default)]
pub(crate) struct EntrySizeAndFrequency {
    // The total policy weight (size) of the aggregated entries.
    pub(crate) policy_weight: u64,
    // Accumulated in a `u64` so that the cost-weighted frequency cannot overflow.
    // (The sketch frequency is capped at 15 and the cost is a `u32`.)
    pub(crate) freq: u64,
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

    /// Adds the entry's frequency to the running aggregate. When `cost` is `Some`,
    /// the frequency is weighted by the cost (for the cost-aware policy); when it is
    /// `None`, the frequency is added as-is.
    pub(crate) fn add_frequency(&mut self, freq: &FrequencySketch, hash: u64, cost: Option<u64>) {
        self.freq += freq.frequency(hash) as u64 * cost.unwrap_or(DEFAULT_COST as u64);
    }
}

impl EvictionPolicyConfig {
    /// Returns the cost to weight an entry's frequency by, or `None` when the policy
    /// ignores cost (every policy other than the cost-aware one).
    pub(crate) fn entry_cost<K, V>(&self, entry: &ValueEntry<K, V>) -> Option<u64> {
        match self {
            Self::CostAwareLfu => Some(entry.policy_cost() as u64),
            Self::TinyLfu | Self::Lru => None,
        }
    }

    /// Records a potential victim into the running aggregate `esf`, adding both its
    /// policy weight and its (cost-weighted) frequency. This lets the admission logic
    /// stay agnostic to which policy is in effect.
    pub(crate) fn add_entry<K, V>(
        &self,
        esf: &mut EntrySizeAndFrequency,
        freq: &FrequencySketch,
        hash: u64,
        entry: &ValueEntry<K, V>,
    ) {
        esf.add_policy_weight(entry.policy_weight());
        esf.add_frequency(freq, hash, self.entry_cost(entry));
    }
}

impl<K, V> AccessTime for MiniArc<ValueEntry<K, V>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.info.last_accessed()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.info.set_last_accessed(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.info.last_modified()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.info.set_last_modified(timestamp);
    }
}

pub(crate) enum ReadOp<K, V> {
    Hit {
        value_entry: MiniArc<ValueEntry<K, V>>,
        is_expiry_modified: bool,
    },
    // u64 is the hash of the key.
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Upsert {
        key_hash: KeyHash<K>,
        value_entry: MiniArc<ValueEntry<K, V>>,
        /// Entry generation after the operation.
        entry_gen: u16,
        old_weight: u32,
        new_weight: u32,
    },
    Remove {
        kv_entry: KvEntry<K, V>,
        entry_gen: u16,
    },
}

/// Cloning a `WriteOp` is safe and cheap because it uses `Arc` and `MiniArc` pointers to
/// the actual data.
impl<K, V> Clone for WriteOp<K, V> {
    fn clone(&self) -> Self {
        match self {
            Self::Upsert {
                key_hash,
                value_entry,
                entry_gen,
                old_weight,
                new_weight,
            } => Self::Upsert {
                key_hash: key_hash.clone(),
                value_entry: MiniArc::clone(value_entry),
                entry_gen: *entry_gen,
                old_weight: *old_weight,
                new_weight: *new_weight,
            },
            Self::Remove {
                kv_entry,
                entry_gen,
            } => Self::Remove {
                kv_entry: kv_entry.clone(),
                entry_gen: *entry_gen,
            },
        }
    }
}

impl<K, V> fmt::Debug for WriteOp<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upsert { .. } => f.debug_struct("Upsert").finish(),
            Self::Remove { .. } => f.debug_tuple("Remove").finish(),
        }
    }
}

impl<K, V> WriteOp<K, V> {
    pub(crate) fn new_upsert(
        key: &Arc<K>,
        hash: u64,
        value_entry: &MiniArc<ValueEntry<K, V>>,
        entry_generation: u16,
        old_weight: u32,
        new_weight: u32,
    ) -> Self {
        let key_hash = KeyHash::new(Arc::clone(key), hash);
        let value_entry = MiniArc::clone(value_entry);
        Self::Upsert {
            key_hash,
            value_entry,
            entry_gen: entry_generation,
            old_weight,
            new_weight,
        }
    }
}

pub(crate) struct OldEntryInfo<K, V> {
    pub(crate) entry: MiniArc<ValueEntry<K, V>>,
    pub(crate) last_accessed: Option<Instant>,
    pub(crate) last_modified: Option<Instant>,
}

impl<K, V> OldEntryInfo<K, V> {
    pub(crate) fn new(entry: &MiniArc<ValueEntry<K, V>>) -> Self {
        Self {
            entry: MiniArc::clone(entry),
            last_accessed: entry.last_accessed(),
            last_modified: entry.last_modified(),
        }
    }
}
