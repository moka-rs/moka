//! Panic containment and dispatch for the synchronous post-removal observer.
//!
//! The observer deliberately stays separate from `RemovalNotifier`: it does not
//! return a future and must not opt the cache into listener-only key locking.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::notification::{PostRemovalObserver as ObserverFn, RemovalCause};

/// Owns an observer callback and permanently disables it after its first panic.
pub(crate) struct PostRemovalObserver<K, V> {
    observer: ObserverFn<K, V>,
    is_enabled: AtomicBool,
    #[cfg(feature = "logging")]
    cache_name: Option<String>,
}

impl<K, V> PostRemovalObserver<K, V> {
    /// Wraps a configured callback and records the cache name for panic logging.
    pub(crate) fn new(observer: ObserverFn<K, V>, _cache_name: Option<String>) -> Self {
        Self {
            observer,
            is_enabled: AtomicBool::new(true),
            #[cfg(feature = "logging")]
            cache_name: _cache_name,
        }
    }

    /// Returns whether new observer calls are still accepted.
    #[inline]
    pub(crate) fn is_enabled(&self) -> bool {
        self.is_enabled.load(Ordering::Acquire)
    }

    /// Invokes the callback inline and disables later calls if it unwinds.
    ///
    /// Calls that already passed the enabled check may overlap. A panic in one of
    /// them does not cancel another call that is already running.
    pub(crate) fn observe(&self, key: Arc<K>, value: V, cause: RemovalCause) {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        if !self.is_enabled() {
            return;
        }

        let observer = || (self.observer)(key, value, cause);
        if let Err(_payload) = catch_unwind(AssertUnwindSafe(observer)) {
            self.is_enabled.store(false, Ordering::Release);
            #[cfg(feature = "logging")]
            log_panic(&*_payload, self.cache_name.as_deref());
        }
    }
}

/// Logs the payload that caused an observer to be permanently disabled.
#[cfg(feature = "logging")]
fn log_panic(payload: &(dyn std::any::Any + Send + 'static), cache_name: Option<&str>) {
    let message: Option<std::borrow::Cow<'_, str>> =
        (payload.downcast_ref::<&str>().map(|s| (*s).into()))
            .or_else(|| payload.downcast_ref::<String>().map(Into::into));
    let prefix = cache_name
        .map(|name| format!("[{name}] "))
        .unwrap_or_default();

    if let Some(message) = message {
        log::error!(
            "{prefix}Disabled the post-removal observer because it panicked at '{message}'"
        );
    } else {
        log::error!("{prefix}Disabled the post-removal observer because it panicked");
    }
}
