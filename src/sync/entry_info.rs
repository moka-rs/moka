use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};

use super::AccessTime;
use crate::common::{atomic_time::AtomicInstant, time::Instant};

pub(crate) trait EntryInfo: AccessTime {
    fn is_admitted(&self) -> bool;
    fn set_is_admitted(&self, value: bool);
    fn reset_timestamps(&self);
    fn policy_weight(&self) -> u32;
    fn set_policy_weight(&self, size: u32);
}

pub(crate) type ArcEntryInfo = Arc<dyn EntryInfo + Send + Sync + 'static>;

pub(crate) struct EntryInfoFull {
    is_admitted: AtomicBool,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
    policy_weight: AtomicU32,
}

impl EntryInfoFull {
    pub(crate) fn new(policy_weight: u32) -> Self {
        Self {
            is_admitted: Default::default(),
            last_accessed: Default::default(),
            last_modified: Default::default(),
            policy_weight: AtomicU32::new(policy_weight),
        }
    }
}

impl EntryInfo for EntryInfoFull {
    #[inline]
    fn is_admitted(&self) -> bool {
        self.is_admitted.load(Ordering::Acquire)
    }

    #[inline]
    fn set_is_admitted(&self, value: bool) {
        self.is_admitted.store(value, Ordering::Release);
    }

    #[inline]
    fn reset_timestamps(&self) {
        self.last_accessed.reset();
        self.last_modified.reset();
    }

    #[inline]
    fn policy_weight(&self) -> u32 {
        self.policy_weight.load(Ordering::Acquire)
    }

    #[inline]
    fn set_policy_weight(&self, size: u32) {
        self.policy_weight.store(size, Ordering::Release);
    }
}

impl AccessTime for EntryInfoFull {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.last_accessed.instant()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.last_accessed.set_instant(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.last_modified.instant()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.last_modified.set_instant(timestamp);
    }
}

pub(crate) struct EntryInfoWo {
    is_admitted: AtomicBool,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
}

impl EntryInfoWo {
    pub(crate) fn new(_policy_weight: u32) -> Self {
        Self {
            is_admitted: Default::default(),
            last_accessed: Default::default(),
            last_modified: Default::default(),
        }
    }
}

impl EntryInfo for EntryInfoWo {
    #[inline]
    fn is_admitted(&self) -> bool {
        self.is_admitted.load(Ordering::Acquire)
    }

    #[inline]
    fn set_is_admitted(&self, value: bool) {
        self.is_admitted.store(value, Ordering::Release);
    }

    #[inline]
    fn reset_timestamps(&self) {
        self.last_accessed.reset();
        self.last_modified.reset();
    }

    #[inline]
    fn policy_weight(&self) -> u32 {
        1
    }

    #[inline]
    fn set_policy_weight(&self, _size: u32) {}
}

impl AccessTime for EntryInfoWo {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.last_accessed.instant()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.last_accessed.set_instant(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.last_modified.instant()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.last_modified.set_instant(timestamp);
    }
}
