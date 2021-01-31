use parking_lot::Mutex;
use quanta::Instant;
use std::{
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

pub(crate) mod deque;
pub(crate) mod deques;
pub(crate) mod frequency_sketch;
pub(crate) mod housekeeper;
pub(crate) mod thread_pool;
pub(crate) mod unsafe_weak_pointer;

use self::deque::DeqNode;

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
    pub(crate) timestamp: Option<Arc<AtomicU64>>,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Arc<K>, timestamp: Option<Arc<AtomicU64>>) -> Self {
        Self { key, timestamp }
    }
}

pub(crate) struct KeyHashDate<K> {
    pub(crate) key: Arc<K>,
    pub(crate) hash: u64,
    pub(crate) timestamp: Option<Arc<AtomicU64>>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(kh: KeyHash<K>, timestamp: Option<Arc<AtomicU64>>) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            timestamp,
        }
    }
}

pub(crate) type KeyDeqNodeAO<K> = NonNull<DeqNode<KeyHashDate<K>>>;
pub(crate) type KeyDeqNodeWO<K> = NonNull<DeqNode<KeyDate<K>>>;

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: Arc<V>,
    last_accessed: Option<Arc<AtomicU64>>,
    last_modified: Option<Arc<AtomicU64>>,
    access_order_q_node: Mutex<Option<KeyDeqNodeAO<K>>>,
    write_order_q_node: Mutex<Option<KeyDeqNodeWO<K>>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(
        value: Arc<V>,
        last_accessed: Option<Instant>,
        last_modified: Option<Instant>,
        access_order_q_node: Option<KeyDeqNodeAO<K>>,
        write_order_q_node: Option<KeyDeqNodeWO<K>>,
    ) -> Self {
        Self {
            value,
            last_accessed: last_accessed.map(|ts| Arc::new(AtomicU64::new(ts.as_u64()))),
            last_modified: last_modified.map(|ts| Arc::new(AtomicU64::new(ts.as_u64()))),
            access_order_q_node: Mutex::new(access_order_q_node),
            write_order_q_node: Mutex::new(write_order_q_node),
        }
    }

    pub(crate) fn new_with(value: Arc<V>, other: &Self) -> Self {
        Self {
            value,
            last_accessed: other.last_accessed.clone(),
            last_modified: other.last_modified.clone(),
            access_order_q_node: Mutex::new(*other.access_order_q_node().lock()),
            write_order_q_node: Mutex::new(*other.write_order_q_node().lock()),
        }
    }

    pub(crate) fn raw_last_accessed(&self) -> Option<Arc<AtomicU64>> {
        self.last_accessed.clone()
    }

    pub(crate) fn raw_last_modified(&self) -> Option<Arc<AtomicU64>> {
        self.last_modified.clone()
    }

    pub(crate) fn access_order_q_node(&self) -> &Mutex<Option<KeyDeqNodeAO<K>>> {
        &self.access_order_q_node
    }
    pub(crate) fn set_access_order_q_node(&self, node: Option<KeyDeqNodeAO<K>>) {
        *self.access_order_q_node.lock() = node;
    }

    pub(crate) fn write_order_q_node(&self) -> &Mutex<Option<KeyDeqNodeWO<K>>> {
        &self.write_order_q_node
    }

    pub(crate) fn set_write_order_q_node(&self, node: Option<KeyDeqNodeWO<K>>) {
        *self.write_order_q_node.lock() = node;
    }
}

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn set_last_accessed(&mut self, timestamp: Instant);
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_modified(&mut self, timestamp: Instant);
}

impl<K, V> AccessTime for Arc<ValueEntry<K, V>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.last_accessed
            .as_ref()
            .map(|ts| ts.load(Ordering::Relaxed))
            .and_then(|ts| {
                if ts == u64::MAX {
                    None
                } else {
                    Some(unsafe { std::mem::transmute(ts) })
                }
            })
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        if let Some(ts) = &self.last_accessed {
            ts.store(timestamp.as_u64(), Ordering::Relaxed);
        }
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.last_modified
            .as_ref()
            .map(|ts| ts.load(Ordering::Relaxed))
            .and_then(|ts| {
                if ts == u64::MAX {
                    None
                } else {
                    Some(unsafe { std::mem::transmute(ts) })
                }
            })
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        if let Some(ts) = &self.last_modified {
            ts.store(timestamp.as_u64(), Ordering::Relaxed);
        }
    }
}

impl<K> AccessTime for DeqNode<KeyDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_accessed(&mut self, _timestamp: Instant) {
        // do nothing
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.element
            .timestamp
            .as_ref()
            .map(|ts| ts.load(Ordering::Relaxed))
            .and_then(|ts| {
                if ts == u64::MAX {
                    None
                } else {
                    Some(unsafe { std::mem::transmute(ts) })
                }
            })
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        if let Some(ts) = self.element.timestamp.as_ref() {
            ts.store(timestamp.as_u64(), Ordering::Relaxed);
        }
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.element
            .timestamp
            .as_ref()
            .map(|ts| ts.load(Ordering::Relaxed))
            .and_then(|ts| {
                if ts == u64::MAX {
                    None
                } else {
                    Some(unsafe { std::mem::transmute(ts) })
                }
            })
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        if let Some(ts) = self.element.timestamp.as_ref() {
            ts.store(timestamp.as_u64(), Ordering::Relaxed);
        }
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_modified(&mut self, _timestamp: Instant) {
        // do nothing
    }
}

pub(crate) enum ReadOp<K, V> {
    Hit(u64, Arc<ValueEntry<K, V>>, Option<Instant>),
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Insert(KeyHash<K>, Arc<ValueEntry<K, V>>),
    Update(Arc<ValueEntry<K, V>>),
    Remove(Arc<ValueEntry<K, V>>),
}
