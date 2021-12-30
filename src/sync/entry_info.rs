use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};

use super::{AccessTime, CacheFeatures};
use crate::common::{atomic_time::AtomicInstant, time::Instant};

pub(crate) enum EntryInfo {
    Plain(Arc<Plain>),
    Weighted(Arc<Weighted>),
}

#[derive(Default)]
pub(crate) struct Plain {
    is_admitted: AtomicBool,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
}

pub(crate) struct Weighted {
    is_admitted: AtomicBool,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
    policy_weight: AtomicU32,
}

impl Weighted {
    pub(crate) fn new(policy_weight: u32) -> Self {
        Self {
            is_admitted: Default::default(),
            last_accessed: Default::default(),
            last_modified: Default::default(),
            policy_weight: AtomicU32::new(policy_weight),
        }
    }
}

impl Clone for EntryInfo {
    fn clone(&self) -> Self {
        match self {
            Self::Plain(ei) => Self::Plain(Arc::clone(ei)),
            Self::Weighted(ei) => Self::Weighted(Arc::clone(ei)),
        }
    }
}

impl EntryInfo {
    #[inline]
    pub(crate) fn new(features: CacheFeatures, policy_weight: u32) -> Self {
        match features {
            CacheFeatures::Plain => Self::Plain(Arc::new(Plain::default())),
            CacheFeatures::Weighted => Self::Weighted(Arc::new(Weighted::new(policy_weight))),
        }
    }

    #[inline]
    pub(crate) fn is_admitted(&self) -> bool {
        let v = match self {
            Self::Plain(ei) => &ei.is_admitted,
            Self::Weighted(ei) => &ei.is_admitted,
        };
        v.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn set_is_admitted(&self, value: bool) {
        let v = match self {
            Self::Plain(ei) => &ei.is_admitted,
            Self::Weighted(ei) => &ei.is_admitted,
        };
        v.store(value, Ordering::Release);
    }

    #[inline]
    pub(crate) fn reset_timestamps(&self) {
        match self {
            Self::Plain(ei) => {
                ei.last_accessed.reset();
                ei.last_accessed.reset();
            }
            Self::Weighted(ei) => {
                ei.last_accessed.reset();
                ei.last_modified.reset();
            }
        }
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        match self {
            Self::Plain(_) => 1,
            Self::Weighted(ei) => ei.policy_weight.load(Ordering::Acquire),
        }
    }

    pub(crate) fn set_policy_weight(&self, size: u32) {
        match self {
            Self::Plain(_) => (),
            Self::Weighted(ei) => ei.policy_weight.store(size, Ordering::Release),
        }
    }
}

impl AccessTime for EntryInfo {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        let v = match self {
            Self::Plain(ei) => &ei.last_accessed,
            Self::Weighted(ei) => &ei.last_accessed,
        };
        v.instant()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        let v = match self {
            Self::Plain(ei) => &ei.last_accessed,
            Self::Weighted(ei) => &ei.last_accessed,
        };
        v.set_instant(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        let v = match self {
            Self::Plain(ei) => &ei.last_modified,
            Self::Weighted(ei) => &ei.last_modified,
        };
        v.instant()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        let v = match self {
            Self::Plain(ei) => &ei.last_modified,
            Self::Weighted(ei) => &ei.last_modified,
        };
        v.set_instant(timestamp);
    }
}
