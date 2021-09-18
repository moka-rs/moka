pub(crate) mod deque;
pub(crate) mod error;
pub(crate) mod frequency_sketch;
pub(crate) mod thread_pool;
pub(crate) mod unsafe_weak_pointer;

// targe_has_atomic is more convenient but yet unstable (Rust 1.55)
// https://github.com/rust-lang/rust/issues/32976
// #[cfg_attr(target_has_atomic = "64", path = "common/time_atomic64.rs")]

#[cfg_attr(feature = "atomic64", path = "common/time_atomic64.rs")]
#[cfg_attr(not(feature = "atomic64"), path = "common/time_compat.rs")]
pub(crate) mod time;

use time::Instant;

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn set_last_accessed(&mut self, timestamp: Instant);
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_modified(&mut self, timestamp: Instant);
}
