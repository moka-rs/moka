use crate::common::{deque::DeqNode, time::Instant};

use parking_lot::Mutex;
use std::{ptr::NonNull, sync::Arc};
use tagptr::TagNonNull;
use triomphe::Arc as TrioArc;

pub(crate) mod constants;
pub(crate) mod deques;
pub(crate) mod entry_info;
pub(crate) mod housekeeper;
pub(crate) mod thread_pool;
pub(crate) mod unsafe_weak_pointer;

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

pub(crate) type Weigher<K, V> = Arc<dyn Fn(&K, &V) -> u32 + Send + Sync + 'static>;

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn set_last_accessed(&self, timestamp: Instant);
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_modified(&self, timestamp: Instant);
}

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

pub(crate) struct KeyDate<K> {
    key: Arc<K>,
    entry_info: TrioArc<EntryInfo>,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Arc<K>, entry_info: &TrioArc<EntryInfo>) -> Self {
        Self {
            key,
            entry_info: TrioArc::clone(entry_info),
        }
    }

    pub(crate) fn key(&self) -> &Arc<K> {
        &self.key
    }

    // #[cfg(any(feature = "sync", feature = "future"))]
    pub(crate) fn last_modified(&self) -> Option<Instant> {
        self.entry_info.last_modified()
    }

    // #[cfg(any(feature = "sync", feature = "future"))]
    pub(crate) fn is_dirty(&self) -> bool {
        self.entry_info.is_dirty()
    }
}

pub(crate) struct KeyHashDate<K> {
    key: Arc<K>,
    hash: u64,
    entry_info: TrioArc<EntryInfo>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(kh: KeyHash<K>, entry_info: &TrioArc<EntryInfo>) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            entry_info: TrioArc::clone(entry_info),
        }
    }

    pub(crate) fn key(&self) -> &Arc<K> {
        &self.key
    }

    pub(crate) fn hash(&self) -> u64 {
        self.hash
    }

    pub(crate) fn entry_info(&self) -> &EntryInfo {
        &self.entry_info
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

impl<K> AccessTime for DeqNode<KeyDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_accessed(&self, _timestamp: Instant) {
        unreachable!();
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
        None
    }

    #[inline]
    fn set_last_modified(&self, _timestamp: Instant) {
        unreachable!();
    }
}

// DeqNode for an access order queue.
type KeyDeqNodeAo<K> = TagNonNull<DeqNode<KeyHashDate<K>>, 2>;

// DeqNode for the write order queue.
type KeyDeqNodeWo<K> = NonNull<DeqNode<KeyDate<K>>>;

pub(crate) struct DeqNodes<K> {
    access_order_q_node: Option<KeyDeqNodeAo<K>>,
    write_order_q_node: Option<KeyDeqNodeWo<K>>,
}

// We need this `unsafe impl` as DeqNodes have NonNull pointers.
unsafe impl<K> Send for DeqNodes<K> {}

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: V,
    info: TrioArc<EntryInfo>,
    nodes: Mutex<DeqNodes<K>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(value: V, entry_info: TrioArc<EntryInfo>) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        self::debug_counters::InternalGlobalDebugCounters::value_entry_created();

        Self {
            value,
            info: entry_info,
            nodes: Mutex::new(DeqNodes {
                access_order_q_node: None,
                write_order_q_node: None,
            }),
        }
    }

    pub(crate) fn new_from(value: V, entry_info: TrioArc<EntryInfo>, other: &Self) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        self::debug_counters::InternalGlobalDebugCounters::value_entry_created();

        let nodes = {
            let other_nodes = other.nodes.lock();
            DeqNodes {
                access_order_q_node: other_nodes.access_order_q_node,
                write_order_q_node: other_nodes.write_order_q_node,
            }
        };
        Self {
            value,
            info: entry_info,
            nodes: Mutex::new(nodes),
        }
    }

    pub(crate) fn entry_info(&self) -> &TrioArc<EntryInfo> {
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

    pub(crate) fn set_dirty(&self, value: bool) {
        self.info.set_dirty(value);
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        self.info.policy_weight()
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
    // u64 is the hash of the key.
    Hit(u64, TrioArc<ValueEntry<K, V>>, Instant),
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Upsert {
        key_hash: KeyHash<K>,
        value_entry: TrioArc<ValueEntry<K, V>>,
        old_weight: u32,
        new_weight: u32,
    },
    Remove(KvEntry<K, V>),
}
