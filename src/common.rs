use std::convert::TryInto;

#[cfg(any(feature = "sync", feature = "future", feature = "dash"))]
pub(crate) mod concurrent;

#[cfg(any(feature = "sync", feature = "future"))]
pub(crate) mod error;

pub(crate) mod builder_utils;
pub(crate) mod deque;
pub(crate) mod frequency_sketch;
pub(crate) mod time;

#[cfg(all(test, feature = "flush", any(feature = "sync", feature = "future")))]
pub(crate) mod test_utils;

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
