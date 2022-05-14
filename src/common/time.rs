use std::time::Duration;

// target_has_atomic is more convenient but yet unstable (Rust 1.55)
// https://github.com/rust-lang/rust/issues/32976
// #[cfg_attr(target_has_atomic = "64", path = "common/time_atomic64.rs")]

#[cfg_attr(
    all(feature = "atomic64", feature = "quanta"),
    path = "time/atomic_time.rs"
)]
#[cfg_attr(
    not(all(feature = "atomic64", feature = "quanta")),
    path = "time/atomic_time_compat.rs"
)]
pub(crate) mod atomic_time;

#[cfg_attr(feature = "quanta", path = "time/clock_quanta.rs")]
#[cfg_attr(not(feature = "quanta"), path = "time/clock_compat.rs")]
mod clock;

pub(crate) use clock::Clock;

#[cfg(test)]
pub(crate) use clock::Mock;

/// a wrapper type over quanta::Instant to force checked additions and prevent
/// unintentional overflow. The type preserve the Copy semantics for the wrapped
#[derive(PartialEq, PartialOrd, Clone, Copy)]
pub(crate) struct Instant(pub clock::Instant);

pub(crate) trait CheckedTimeOps {
    fn checked_add(&self, duration: Duration) -> Option<Self>
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
}

impl CheckedTimeOps for Instant {
    fn checked_add(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_add(duration).map(Instant)
    }
}
