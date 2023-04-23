use crate::common::time::{clock::Instant as ClockInstant, Instant};

use std::{
    any::TypeId,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug)]
pub(crate) struct AtomicInstant {
    instant: AtomicU64,
}

impl Default for AtomicInstant {
    fn default() -> Self {
        Self {
            instant: AtomicU64::new(std::u64::MAX),
        }
    }
}

// TODO: Need a safe way to convert between `quanta::Instant` and `u64`.
// quanta v0.10.0 no longer provides `quanta::Instant::as_u64` method.

impl AtomicInstant {
    pub(crate) fn new(timestamp: Instant) -> Self {
        let ai = Self::default();
        ai.set_instant(timestamp);
        ai
    }

    pub(crate) fn clear(&self) {
        self.instant.store(u64::MAX, Ordering::Release);
    }

    pub(crate) fn is_set(&self) -> bool {
        self.instant.load(Ordering::Acquire) != u64::MAX
    }

    pub(crate) fn instant(&self) -> Option<Instant> {
        let ts = self.instant.load(Ordering::Acquire);
        if ts == u64::MAX {
            None
        } else {
            debug_assert_eq!(
                TypeId::of::<ClockInstant>(),
                TypeId::of::<quanta::Instant>()
            );
            Some(Instant::new(unsafe { std::mem::transmute(ts) }))
        }
    }

    pub(crate) fn set_instant(&self, instant: Instant) {
        debug_assert_eq!(
            TypeId::of::<ClockInstant>(),
            TypeId::of::<quanta::Instant>()
        );
        let ts = unsafe { std::mem::transmute(instant.inner_clock()) };
        self.instant.store(ts, Ordering::Release);
    }
}
