//! Provides thread-safe, synchronous (blocking) cache implementations.

use crate::common::{deque::DeqNode, u64_to_instant, AccessTime};

use parking_lot::Mutex;
use quanta::Instant;
use std::{
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

pub(crate) mod base_cache;
mod builder;
pub(crate) mod cache;
pub(crate) mod deques;
pub(crate) mod housekeeper;
mod segment;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use segment::SegmentedCache;

/// Provides extra methods that will be useful for testing.
pub trait ConcurrentCacheExt<K, V> {
    /// Performs any pending maintenance operations needed by the cache.
    fn sync(&self);
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

pub(crate) struct KeyDate<K> {
    pub(crate) key: Arc<K>,
    pub(crate) timestamp: Arc<AtomicU64>,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Arc<K>, timestamp: Arc<AtomicU64>) -> Self {
        Self { key, timestamp }
    }
}

pub(crate) struct KeyHashDate<K> {
    pub(crate) key: Arc<K>,
    pub(crate) hash: u64,
    pub(crate) timestamp: Arc<AtomicU64>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(kh: KeyHash<K>, timestamp: Arc<AtomicU64>) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            timestamp,
        }
    }
}

// DeqNode for an access order queue.
type KeyDeqNodeAo<K> = NonNull<DeqNode<KeyHashDate<K>>>;

// DeqNode for the write order queue.
type KeyDeqNodeWo<K> = NonNull<DeqNode<KeyDate<K>>>;

struct DeqNodes<K> {
    access_order_q_node: Option<KeyDeqNodeAo<K>>,
    write_order_q_node: Option<KeyDeqNodeWo<K>>,
}

#[cfg(feature = "future")]
// Multi-threaded async runtimes require ValueEntry to be Send, but it will
// not be without this `unsafe impl`. This is because DeqNodes have NonNull
// pointers.
unsafe impl<K> Send for DeqNodes<K> {}

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: V,
    last_accessed: Arc<AtomicU64>,
    last_modified: Arc<AtomicU64>,
    nodes: Mutex<DeqNodes<K>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(value: V) -> Self {
        Self {
            value,
            last_accessed: Arc::new(AtomicU64::new(std::u64::MAX)),
            last_modified: Arc::new(AtomicU64::new(std::u64::MAX)),
            nodes: Mutex::new(DeqNodes {
                access_order_q_node: None,
                write_order_q_node: None,
            }),
        }
    }

    pub(crate) fn new_with(value: V, other: &Self) -> Self {
        let nodes = {
            let other_nodes = other.nodes.lock();
            DeqNodes {
                access_order_q_node: other_nodes.access_order_q_node,
                write_order_q_node: other_nodes.write_order_q_node,
            }
        };
        Self {
            value,
            last_accessed: other.last_accessed.clone(),
            last_modified: other.last_modified.clone(),
            nodes: Mutex::new(nodes),
        }
    }

    pub(crate) fn raw_last_accessed(&self) -> Arc<AtomicU64> {
        self.last_accessed.clone()
    }

    pub(crate) fn raw_last_modified(&self) -> Arc<AtomicU64> {
        self.last_modified.clone()
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

impl<K, V> AccessTime for Arc<ValueEntry<K, V>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        u64_to_instant(self.last_accessed.load(Ordering::Relaxed))
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.last_accessed
            .store(timestamp.as_u64(), Ordering::Relaxed);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        u64_to_instant(self.last_modified.load(Ordering::Relaxed))
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        self.last_modified
            .store(timestamp.as_u64(), Ordering::Relaxed);
    }
}

impl<K> AccessTime for DeqNode<KeyDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_accessed(&mut self, _timestamp: Instant) {
        unreachable!();
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        u64_to_instant(self.element.timestamp.load(Ordering::Relaxed))
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        self.element
            .timestamp
            .store(timestamp.as_u64(), Ordering::Relaxed);
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        u64_to_instant(self.element.timestamp.load(Ordering::Relaxed))
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.element
            .timestamp
            .store(timestamp.as_u64(), Ordering::Relaxed);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_modified(&mut self, _timestamp: Instant) {
        unreachable!();
    }
}

pub(crate) enum ReadOp<K, V> {
    Hit(u64, Arc<ValueEntry<K, V>>, Instant),
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Insert(KeyHash<K>, Arc<ValueEntry<K, V>>),
    Update(Arc<ValueEntry<K, V>>),
    Remove(Arc<ValueEntry<K, V>>),
}
