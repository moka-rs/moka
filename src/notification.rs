use std::sync::Arc;

// use parking_lot::Mutex;

// TODO: Perhaps `Arc<dyn Fn(...)>` is enough for the most use cases because
// Sync would require captured values to be interior mutable?
// pub(crate) type EvictionListener<K, V> =
//     Arc<Mutex<dyn FnMut(Arc<K>, V, RemovalCause) + Send + Sync + 'static>>;
pub(crate) type EvictionListener<K, V> =
    Arc<dyn Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static>;

// pub(crate) type EvictionListenerRef<'a, K, V> = &'a mut dyn FnMut(Arc<K>, V, RemovalCause);
pub(crate) type EvictionListenerRef<'a, K, V> =
    &'a Arc<dyn Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static>;

// NOTE: Currently, dropping the cache will drop all entries without sending
// notifications. Calling `invalidate_all` method of the cache will trigger
// the notifications, but currently there is no way to know when all entries
// have been invalidated and their notifications have been sent.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovalCause {
    /// The entry's expiration timestamp has passed.
    Expired,
    /// The entry was manually removed by the user.
    Explicit,
    /// The entry itself was not actually removed, but its value was replaced by
    /// the user.
    Replaced,
    /// The entry was evicted due to size constraints.
    Size,
}

impl RemovalCause {
    pub fn was_evicted(&self) -> bool {
        matches!(self, Self::Expired | Self::Size)
    }
}
