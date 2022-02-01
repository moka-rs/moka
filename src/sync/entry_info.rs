use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::AccessTime;
use crate::common::{atomic_time::AtomicInstant, time::Instant};

pub(crate) struct EntryInfo {
    is_admitted: AtomicBool,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
    policy_weight: AtomicU32,
}

impl EntryInfo {
    #[inline]
    pub(crate) fn new(policy_weight: u32) -> Self {
        Self {
            is_admitted: Default::default(),
            last_accessed: Default::default(),
            last_modified: Default::default(),
            policy_weight: AtomicU32::new(policy_weight),
        }
    }

    #[inline]
    pub(crate) fn is_admitted(&self) -> bool {
        self.is_admitted.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn set_is_admitted(&self, value: bool) {
        self.is_admitted.store(value, Ordering::Release);
    }

    #[inline]
    pub(crate) fn reset_timestamps(&self) {
        self.last_accessed.reset();
        self.last_modified.reset();
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        self.policy_weight.load(Ordering::Acquire)
    }

    pub(crate) fn set_policy_weight(&self, size: u32) {
        self.policy_weight.store(size, Ordering::Release);
    }
}

impl AccessTime for EntryInfo {
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
