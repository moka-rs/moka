mod atomic_time;
mod clock;
mod instant;

pub(crate) use atomic_time::AtomicInstant;
pub(crate) use clock::Clock;
pub use clock::ExternalClock;
pub(crate) use instant::Instant;

#[cfg(test)]
pub(crate) use clock::Mock;
