use quanta::Instant;
use std::{cell::UnsafeCell, ptr::NonNull, sync::Arc};

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
    // Optional. Instant(0u64) for None.
    pub(crate) timestamp: Instant,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Arc<K>, timestamp: Option<Instant>) -> Self {
        Self {
            key,
            timestamp: timestamp.unwrap_or_else(|| unsafe { std::mem::transmute(0u64) }),
        }
    }
}

pub(crate) struct KeyHashDate<K> {
    pub(crate) key: Arc<K>,
    pub(crate) hash: u64,
    // Optional. Instant(0u64) for None.
    pub(crate) timestamp: Instant,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(kh: KeyHash<K>, timestamp: Option<Instant>) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            timestamp: timestamp.unwrap_or_else(|| unsafe { std::mem::transmute(0u64) }),
        }
    }
}

pub(crate) type KeyDeqNodeAO<K> = Option<NonNull<DeqNode<KeyHashDate<K>>>>;
pub(crate) type KeyDeqNodeWO<K> = Option<NonNull<DeqNode<KeyDate<K>>>>;

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: Arc<V>,
    pub(crate) access_order_q_node: UnsafeCell<KeyDeqNodeAO<K>>,
    pub(crate) write_order_q_node: UnsafeCell<KeyDeqNodeWO<K>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(
        value: Arc<V>,
        access_order_q_node: KeyDeqNodeAO<K>,
        write_order_q_node: KeyDeqNodeWO<K>,
    ) -> Self {
        Self {
            value,
            access_order_q_node: UnsafeCell::new(access_order_q_node),
            write_order_q_node: UnsafeCell::new(write_order_q_node),
        }
    }
}

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_accessed(&mut self, timestamp: Instant);
}

impl<K, V> AccessTime for Arc<ValueEntry<K, V>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        unsafe { (*self.access_order_q_node.get()).map(|node| node.as_ref().element.timestamp) }
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        unsafe { (*self.write_order_q_node.get()).map(|node| node.as_ref().element.timestamp) }
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        unsafe {
            if let Some(mut node) = *self.access_order_q_node.get() {
                node.as_mut().element.timestamp = timestamp;
            }
        }
    }
}

impl<K> AccessTime for DeqNode<KeyDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        Some(self.element.timestamp)
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.element.timestamp = timestamp;
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        Some(self.element.timestamp)
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.element.timestamp = timestamp;
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
