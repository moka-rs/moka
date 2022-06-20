use std::sync::Arc;

pub(crate) type EvictionListener<K, V> =
    Arc<dyn Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static>;

pub(crate) type EvictionListenerRef<'a, K, V> =
    &'a Arc<dyn Fn(Arc<K>, V, RemovalCause) + Send + Sync + 'static>;

// NOTE: Currently, dropping the cache will drop all entries without sending
// notifications. Calling `invalidate_all` method of the cache will trigger
// the notifications, but currently there is no way to know when all entries
// have been invalidated and their notifications have been sent.

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

#[derive(Default)]
pub struct ConfigurationBuilder {
    mode: DeliveryMode,
}

impl ConfigurationBuilder {
    pub fn build(self) -> Configuration {
        Configuration { mode: self.mode }
    }

    pub fn delivery_mode(self, mode: DeliveryMode) -> Self {
        // Self { mode, ..self }
        Self { mode }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DeliveryMode {
    Immediate,
    Queued,
}

impl Default for DeliveryMode {
    fn default() -> Self {
        Self::Immediate
    }
}

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
