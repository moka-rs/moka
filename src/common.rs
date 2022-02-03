use std::convert::TryInto;

pub(crate) mod builder_utils;
pub(crate) mod deque;
pub(crate) mod error;
pub(crate) mod frequency_sketch;
pub(crate) mod thread_pool;
pub(crate) mod unsafe_weak_pointer;

// targe_has_atomic is more convenient but yet unstable (Rust 1.55)
// https://github.com/rust-lang/rust/issues/32976
// #[cfg_attr(target_has_atomic = "64", path = "common/time_atomic64.rs")]

#[cfg_attr(feature = "atomic64", path = "common/atomic_time.rs")]
#[cfg_attr(not(feature = "atomic64"), path = "common/atomic_time_compat.rs")]
pub(crate) mod atomic_time;

pub(crate) mod time;

// Note: `CacheRegion` cannot have more than four enum variants. This is because
// `crate::{sync,unsync}::DeqNodes` uses a `tagptr::TagNonNull<DeqNode<T>, 2>`
// pointer, where the 2-bit tag is `CacheRegion`.
#[derive(Clone, Copy, Debug, Eq)]
pub(crate) enum CacheRegion {
    Window = 0,
    MainProbation = 1,
    MainProtected = 2,
    Other = 3,
}

impl From<usize> for CacheRegion {
    fn from(n: usize) -> Self {
        match n {
            0 => Self::Window,
            1 => Self::MainProbation,
            2 => Self::MainProtected,
            3 => Self::Other,
            _ => panic!("No such CacheRegion variant for {}", n),
        }
    }
}

impl PartialEq<Self> for CacheRegion {
    fn eq(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
}

impl PartialEq<usize> for CacheRegion {
    fn eq(&self, other: &usize) -> bool {
        *self as usize == *other
    }
}

// Ensures the value fits in a range of `128u32..=u32::MAX`.
pub(crate) fn sketch_capacity(max_capacity: u64) -> u32 {
    max_capacity.try_into().unwrap_or(u32::MAX).max(128)
}
