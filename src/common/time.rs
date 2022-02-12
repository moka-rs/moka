use std::time::Duration;

pub(crate) type Clock = quanta::Clock;
#[cfg(test)]
pub(crate) type Mock = quanta::Mock;

/// a wrapper type over qunta::Instant to force checked additions and prevent
/// unintentional overflow. The type preserve the Copy semantics for the wrapped
#[derive(PartialEq, PartialOrd, Clone, Copy)]
pub(crate) struct Instant(pub quanta::Instant);

pub(crate) trait CheckedTimeOps {
    fn checked_add(&self, duration: Duration) -> Option<Self>
    where
        Self: Sized;
}

impl Instant {
    pub(crate) fn new(instant: quanta::Instant) -> Instant {
        Instant(instant)
    }

    pub(crate) fn now() -> Instant {
        Instant(quanta::Instant::now())
    }

    pub(crate) fn elapsed_nanos(&self) -> u64 {
        use std::convert::TryInto;
        quanta::Instant::now()
            .duration_since(self.0)
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

impl CheckedTimeOps for Instant {
    fn checked_add(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_add(duration).map(Instant)
    }
}
