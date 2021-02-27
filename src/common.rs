use quanta::Instant;

pub(crate) mod deque;
pub(crate) mod frequency_sketch;
pub(crate) mod thread_pool;
pub(crate) mod unsafe_weak_pointer;

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn set_last_accessed(&mut self, timestamp: Instant);
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_modified(&mut self, timestamp: Instant);
}
