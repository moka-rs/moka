use std::time::Duration;

#[derive(Clone, Debug)]
/// The policy of a cache.
pub struct Policy {
    max_capacity: Option<u64>,
    num_segments: usize,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
}

impl Policy {
    pub(crate) fn new(
        max_capacity: Option<u64>,
        num_segments: usize,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        Self {
            max_capacity,
            num_segments,
            time_to_live,
            time_to_idle,
        }
    }

    /// Returns the `max_capacity` of the cache.
    pub fn max_capacity(&self) -> Option<u64> {
        self.max_capacity
    }

    // #[cfg(feature = "sync")]
    pub(crate) fn set_max_capacity(&mut self, capacity: Option<u64>) {
        self.max_capacity = capacity;
    }

    /// Returns the number of internal segments of the cache.
    pub fn num_segments(&self) -> usize {
        self.num_segments
    }

    // #[cfg(feature = "sync")]
    pub(crate) fn set_num_segments(&mut self, num: usize) {
        self.num_segments = num;
    }

    /// Returns the `time_to_live` of the cache.
    pub fn time_to_live(&self) -> Option<Duration> {
        self.time_to_live
    }

    /// Returns the `time_to_idle` of the cache.
    pub fn time_to_idle(&self) -> Option<Duration> {
        self.time_to_idle
    }
}
