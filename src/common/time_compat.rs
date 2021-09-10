use std::{
    cmp::Ordering,
    ops::Add,
    sync::Arc,
    time::{Duration, Instant as StdInstant},
};

use parking_lot::RwLock;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Instant(StdInstant);

impl Instant {
    pub(crate) fn now() -> Self {
        Self(StdInstant::now())
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, other: Duration) -> Self::Output {
        let instant = self
            .0
            .checked_add(other)
            .expect("overflow when adding duration to instant");
        Self(instant)
    }
}

impl PartialOrd for Instant {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Instant {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

pub(crate) struct AtomicInstant {
    instant: RwLock<Option<Instant>>,
}

impl Default for AtomicInstant {
    fn default() -> Self {
        Self {
            instant: RwLock::new(None),
        }
    }
}

impl AtomicInstant {
    pub(crate) fn reset(&self) {
        *self.instant.write() = None;
    }

    pub(crate) fn is_set(&self) -> bool {
        self.instant.read().is_some()
    }

    pub(crate) fn instant(&self) -> Option<Instant> {
        *self.instant.read()
    }

    pub(crate) fn set_instant(&self, instant: Instant) {
        *self.instant.write() = Some(instant);
    }
}

pub(crate) struct Clock(Arc<Mock>);

impl Clock {
    #[cfg(test)]
    pub(crate) fn mock() -> (Clock, Arc<Mock>) {
        let mock = Arc::new(Mock::default());
        let clock = Clock(Arc::clone(&mock));
        (clock, mock)
    }

    pub(crate) fn now(&self) -> Instant {
        Instant(*self.0.now.read())
    }
}

pub(crate) struct Mock {
    now: RwLock<StdInstant>,
}

impl Default for Mock {
    fn default() -> Self {
        Self {
            now: RwLock::new(StdInstant::now()),
        }
    }
}

#[cfg(test)]
impl Mock {
    pub(crate) fn increment(&self, amount: Duration) {
        *self.now.write() += amount;
    }
}
