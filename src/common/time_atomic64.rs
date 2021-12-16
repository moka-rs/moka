use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// a wrapper type over qunta::Instant to force checked additions and prevent
/// unintentioal overflow. The type preserve the Copy semnatics for the wrapped
#[derive(PartialEq, PartialOrd, Clone, Copy)]
pub(crate) struct Instant(quanta::Instant);

pub(crate) type Clock = quanta::Clock;

pub(crate) trait CheckedTimeOps {
    fn checked_add(&self, duration: Duration)-> Option<Self> where Self: Sized;
} 

impl Instant {
    pub(crate) fn new(instant: quanta::Instant) -> Instant {
        Instant(instant)
    }

    pub(crate) fn now()-> Instant {
        Instant(quanta::Instant::now())
    }
}

impl CheckedTimeOps for Instant {
    fn checked_add(&self, duration: Duration) -> Option<Instant> {
        if let Some(checked_add) = self.0.checked_add(duration){
            Some(Instant(checked_add))
        } else {
            None
        }
        
    }
}

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
        self.instant.store(instant.0.as_u64(), Ordering::Release);
    }
}
