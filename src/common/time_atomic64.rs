use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) type Instant = quanta::Instant;
pub(crate) type Clock = quanta::Clock;

#[cfg(test)]
pub(crate) type Mock = quanta::Mock;

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

impl AtomicInstant {
    pub(crate) fn reset(&self) {
        self.instant.store(std::u64::MAX, Ordering::Release);
    }

    pub(crate) fn is_set(&self) -> bool {
        self.instant.load(Ordering::Acquire) != u64::MAX
    }

    pub(crate) fn instant(&self) -> Option<Instant> {
        let ts = self.instant.load(Ordering::Acquire);
        if ts == u64::MAX {
            None
        } else {
            Some(unsafe { std::mem::transmute(ts) })
        }
    }

    pub(crate) fn set_instant(&self, instant: Instant) {
        self.instant.store(instant.as_u64(), Ordering::Release);
    }
}
