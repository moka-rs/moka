//! Common data types for notifications.

use std::sync::Arc;

pub(crate) type EvictionListener<K, V> =
    Arc<dyn Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static>;

pub(crate) type EvictionListenerRef<'a, K, V> =
    &'a Arc<dyn Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static>;

// NOTE: Currently, dropping the cache will drop all entries without sending
// notifications. Calling `invalidate_all` method of the cache will trigger
// the notifications, but currently there is no way to know when all entries
// have been invalidated and their notifications have been sent.

/// Configuration for an eviction listener of a cache.
///
/// Currently only setting the [`DeliveryMode`][delivery-mode] is supported.
///
/// [delivery-mode]: ./enum.DeliveryMode.html
#[derive(Clone, Debug, Default)]
pub struct Configuration {
    mode: DeliveryMode,
}

impl Configuration {
    pub fn builder() -> ConfigurationBuilder {
        ConfigurationBuilder::default()
    }

    pub fn delivery_mode(&self) -> DeliveryMode {
        self.mode
    }
}

/// Builds a [`Configuration`][conf] with some configuration knobs.
///
/// Currently only setting the [`DeliveryMode`][delivery-mode] is supported.
///
/// [conf]: ./struct.Configuration.html
/// [delivery-mode]: ./enum.DeliveryMode.html
#[derive(Default)]
pub struct ConfigurationBuilder {
    mode: DeliveryMode,
}

impl ConfigurationBuilder {
    pub fn build(self) -> Configuration {
        Configuration { mode: self.mode }
    }

    pub fn delivery_mode(self, mode: DeliveryMode) -> Self {
        Self { mode }
    }
}

/// Specifies how and when an eviction notifications should be delivered to an
/// eviction listener.
///
/// For more details, see [the document][delivery-mode-doc] of `sync::Cache`.
///
/// [delivery-mode-doc]: ../sync/struct.Cache.html#delivery-modes-for-eviction-listener
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DeliveryMode {
    /// With mode, a notification should be delivered to the listener immediately
    /// after an entry was evicted. It also guarantees that eviction notifications
    /// and cache write operations such and `insert`, `get_with` and `invalidate` for
    /// a given cache key are ordered by the time when they occurred.
    ///
    /// To guarantee the order, cache maintains key-level lock, which will reduce
    /// concurrent write performance.
    ///
    /// Use this mode when the order is more import than the write performance.
    Immediate,
    /// With this mode, a notification will be delivered to the listener some time
    /// after an entry was evicted. Therefore, it does not preserve the order of
    /// eviction notifications and write operations.
    ///
    /// On the other hand, cache does not maintain key-level lock, so there will be
    /// no overhead on write performance.
    ///
    /// Use this mode when write performance is more important than preserving the
    /// order of eviction notifications and write operations.
    Queued,
}

impl Default for DeliveryMode {
    fn default() -> Self {
        Self::Immediate
    }
}

/// Indicates the reason why a cached entry was removed.
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

#[cfg(all(test, feature = "sync"))]
pub(crate) mod macros {

    macro_rules! assert_with_mode {
        ($cond:expr, $delivery_mode:ident) => {
            assert!(
                $cond,
                "assertion failed. (delivery mode: {:?})",
                $delivery_mode
            )
        };
    }

    macro_rules! assert_eq_with_mode {
        ($left:expr, $right:expr, $delivery_mode:ident) => {
            assert_eq!($left, $right, "(delivery mode: {:?})", $delivery_mode)
        };
    }

    pub(crate) use {assert_eq_with_mode, assert_with_mode};
}
