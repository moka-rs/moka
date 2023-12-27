use crate::common::{deque::DeqNode, time::Instant};

use parking_lot::Mutex;
use std::{fmt, ptr::NonNull, sync::Arc};
use tagptr::TagNonNull;
use triomphe::Arc as TrioArc;

pub(crate) mod constants;
pub(crate) mod deques;
pub(crate) mod entry_info;

#[cfg(feature = "sync")]
pub(crate) mod housekeeper;

// target_has_atomic is more convenient but yet unstable (Rust 1.55)
// https://github.com/rust-lang/rust/issues/32976
// #[cfg_attr(target_has_atomic = "64", path = "common/time_atomic64.rs")]

#[cfg_attr(
    all(feature = "atomic64", feature = "quanta"),
    path = "concurrent/atomic_time/atomic_time.rs"
)]
#[cfg_attr(
    not(all(feature = "atomic64", feature = "quanta")),
    path = "concurrent/atomic_time/atomic_time_compat.rs"
)]
pub(crate) mod atomic_time;

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
    entry_info: TrioArc<EntryInfo<K>>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(entry_info: &TrioArc<EntryInfo<K>>) -> Self {
        Self {
            entry_info: TrioArc::clone(entry_info),
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
    pub(crate) entry: TrioArc<ValueEntry<K, V>>,
}

impl<K, V> KvEntry<K, V> {
    pub(crate) fn new(key: Arc<K>, entry: TrioArc<ValueEntry<K, V>>) -> Self {
        Self { key, entry }
    }
}

impl<K, V> Clone for KvEntry<K, V> {
    fn clone(&self) -> Self {
        Self {
            key: Arc::clone(&self.key),
            entry: TrioArc::clone(&self.entry),
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
}

impl<K> Default for DeqNodes<K> {
    fn default() -> Self {
        Self {
            access_order_q_node: None,
            write_order_q_node: None,
            timer_node: None,
        }
    }
}

// We need this `unsafe impl` as DeqNodes have NonNull pointers.
unsafe impl<K> Send for DeqNodes<K> {}

impl<K> DeqNodes<K> {
    pub(crate) fn set_timer_node(&mut self, timer_node: Option<DeqNodeTimer<K>>) {
        self.timer_node = timer_node;
    }
}

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: V,
    info: TrioArc<EntryInfo<K>>,
    nodes: TrioArc<Mutex<DeqNodes<K>>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(value: V, entry_info: TrioArc<EntryInfo<K>>) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        self::debug_counters::InternalGlobalDebugCounters::value_entry_created();

        Self {
            value,
            info: entry_info,
            nodes: TrioArc::new(Mutex::new(DeqNodes::default())),
        }
    }

    pub(crate) fn new_from(value: V, entry_info: TrioArc<EntryInfo<K>>, other: &Self) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        self::debug_counters::InternalGlobalDebugCounters::value_entry_created();
        Self {
            value,
            info: entry_info,
            nodes: TrioArc::clone(&other.nodes),
        }
    }

    pub(crate) fn entry_info(&self) -> &TrioArc<EntryInfo<K>> {
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

    pub(crate) fn deq_nodes(&self) -> &TrioArc<Mutex<DeqNodes<K>>> {
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

    pub(crate) fn timer_node(&self) -> Option<DeqNodeTimer<K>> {
        self.nodes.lock().timer_node
    }

    pub(crate) fn set_timer_node(&self, node: Option<DeqNodeTimer<K>>) {
        self.nodes.lock().timer_node = node;
    }

    pub(crate) fn take_timer_node(&self) -> Option<DeqNodeTimer<K>> {
        self.nodes.lock().timer_node.take()
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

impl<K, V> AccessTime for TrioArc<ValueEntry<K, V>> {
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
        value_entry: TrioArc<ValueEntry<K, V>>,
        is_expiry_modified: bool,
    },
    // u64 is the hash of the key.
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Upsert {
        key_hash: KeyHash<K>,
        value_entry: TrioArc<ValueEntry<K, V>>,
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

/// Cloning a `WriteOp` is safe and cheap because it uses `Arc` and `TrioArc` pointers to
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
                value_entry: TrioArc::clone(value_entry),
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
        value_entry: &TrioArc<ValueEntry<K, V>>,
        entry_generation: u16,
        old_weight: u32,
        new_weight: u32,
    ) -> Self {
        let key_hash = KeyHash::new(Arc::clone(key), hash);
        let value_entry = TrioArc::clone(value_entry);
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
    pub(crate) entry: TrioArc<ValueEntry<K, V>>,
    pub(crate) last_accessed: Option<Instant>,
    pub(crate) last_modified: Option<Instant>,
}

impl<K, V> OldEntryInfo<K, V> {
    pub(crate) fn new(entry: &TrioArc<ValueEntry<K, V>>) -> Self {
        Self {
            entry: TrioArc::clone(entry),
            last_accessed: entry.last_accessed(),
            last_modified: entry.last_modified(),
        }
    }
}
