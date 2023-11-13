use std::time::Duration;

#[cfg_attr(feature = "quanta", path = "time/clock_quanta.rs")]
#[cfg_attr(not(feature = "quanta"), path = "time/clock_compat.rs")]
pub(crate) mod clock;

pub(crate) use clock::Clock;

#[cfg(test)]
pub(crate) use clock::Mock;

/// a wrapper type over Instant to force checked additions and prevent
/// unintentional overflow. The type preserve the Copy semantics for the wrapped
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub(crate) struct Instant(clock::Instant);

pub(crate) trait CheckedTimeOps {
    fn checked_add(&self, duration: Duration) -> Option<Self>
    where
        Self: Sized;

    fn checked_duration_since(&self, earlier: Self) -> Option<Duration>
    where
        Self: Sized;
}

impl Instant {
    pub(crate) fn new(instant: clock::Instant) -> Instant {
        Instant(instant)
    }

    pub(crate) fn now() -> Instant {
        Instant(clock::Instant::now())
    }

    #[cfg(feature = "quanta")]
    pub(crate) fn inner_clock(self) -> clock::Instant {
        self.0
    }
}

impl CheckedTimeOps for Instant {
    fn checked_add(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_add(duration).map(Instant)
    }

    fn checked_duration_since(&self, earlier: Self) -> Option<Duration>
    where
        Self: Sized,
    {
        self.0.checked_duration_since(earlier.0)
    }
}
