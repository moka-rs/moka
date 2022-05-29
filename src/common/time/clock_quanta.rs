pub(crate) type Clock = quanta::Clock;
pub(crate) type Instant = quanta::Instant;

#[cfg(test)]
// #[cfg(all(test, feature = "sync"))]
pub(crate) type Mock = quanta::Mock;
