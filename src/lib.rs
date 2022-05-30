#![warn(clippy::all)]
#![warn(rust_2018_idioms)]
// Temporary disable this lint as the MSRV (1.51) require an older lint name:
// #![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! Moka is a fast, concurrent cache library for Rust. Moka is inspired by
//! the [Caffeine][caffeine-git] library for Java.
//!
//! Moka provides in-memory concurrent cache implementations on top of hash maps.
//! They support full concurrency of retrievals and a high expected concurrency for
//! updates. They utilize a lock-free concurrent hash table as the central key-value
//! storage.
//!
//! Moka also provides an in-memory, non-thread-safe cache implementation for single
//! thread applications.
//!
//! All cache implementations perform a best-effort bounding of the map using an
//! entry replacement algorithm to determine which entries to evict when the capacity
//! is exceeded.
//!
//! [caffeine-git]: https://github.com/ben-manes/caffeine
//!
//! # Features
//!
//! - Thread-safe, highly concurrent in-memory cache implementations:
//!     - Synchronous caches that can be shared across OS threads.
//!     - An asynchronous (futures aware) cache that can be accessed inside and
//!       outside of asynchronous contexts.
//! - A cache can be bounded by one of the followings:
//!     - The maximum number of entries.
//!     - The total weighted size of entries. (Size aware eviction)
//! - Maintains good hit rate by using entry replacement algorithms inspired by
//!   [Caffeine][caffeine-git]:
//!     - Admission to a cache is controlled by the Least Frequently Used (LFU) policy.
//!     - Eviction from a cache is controlled by the Least Recently Used (LRU) policy.
//! - Supports expiration policies:
//!     - Time to live
//!     - Time to idle
//!
//! # Examples
//!
//! See the following document:
//!
//! - Thread-safe, synchronous caches:
//!     - [`sync::Cache`][sync-cache-struct]
//!     - [`sync::SegmentedCache`][sync-seg-cache-struct]
//!     - [`dash::Cache`][dash-cache-struct] (Experimental, requires "dash" feature)
//! - An asynchronous (futures aware) cache:
//!     - [`future::Cache`][future-cache-struct] (Requires "future" feature)
//! - A not thread-safe, blocking cache for single threaded applications:
//!     - [`unsync::Cache`][unsync-cache-struct]
//!
//! [dash-cache-struct]: ./dash/struct.Cache.html
//! [future-cache-struct]: ./future/struct.Cache.html
//! [sync-cache-struct]: ./sync/struct.Cache.html
//! [sync-seg-cache-struct]: ./sync/struct.SegmentedCache.html
//! [unsync-cache-struct]: ./unsync/struct.Cache.html
//! [dashmap]: https://docs.rs/dashmap/*/dashmap/struct.DashMap.html
//!
//! # Minimum Supported Rust Versions
//!
//! This crate's minimum supported Rust versions (MSRV) are the followings:
//!
//! | Feature    | Enabled by default? | MSRV        |
//! |:-----------|:-------------------:|:-----------:|
//! | no feature |                     | Rust 1.51.0 |
//! | `atomic64` |       yes           | Rust 1.51.0 |
//! | `quanta`   |       yes           | Rust 1.51.0 |
//! | `future`   |                     | Rust 1.51.0 |
//! | `dash`     |                     | Rust 1.51.0 |
//!
//! If only the default features are enabled, MSRV will be updated conservatively.
//! When using other features, like `future`, MSRV might be updated more frequently,
//! up to the latest stable. In both cases, increasing MSRV is _not_ considered a
//! semver-breaking change.
//!
//! # Implementation Details
//!
//! ## Concurrency
//!
//! In a concurrent cache (`sync` or `future` cache), the entry replacement
//! algorithms are kept eventually consistent with the map. While updates to the
//! cache are immediately applied to the map, recording of reads and writes may not
//! be immediately reflected on the cache policy's data structures.
//!
//! These structures are guarded by a lock and operations are applied in batches to
//! avoid lock contention. There are bounded inter-thread channels to hold these
//! operations. These channels are drained at the first opportunity when:
//!
//! - The numbers of read/write recordings reach to the configured amounts.
//! - Or, the certain time past from the last draining.
//!
//! In a `Cache`, this draining and batch application is handled by a single worker
//! thread. So under heavy concurrent operations from clients, draining may not be
//! able to catch up and the bounded channels can become full.
//!
//! When read or write channel becomes full, one of the followings will occur:
//!
//! - For the read channel, recordings of new reads will be discarded, so that
//!   retrievals will never be blocked. This behavior may have some impact to the hit
//!   rate of the cache.
//! - For the write channel, updates from clients to the cache will be blocked until
//!   the draining task catches up.
//!
//! `Cache` does its best to avoid blocking updates by adjusting the interval of
//! draining. But since it has only one worker
//! thread, it cannot always avoid blocking. If this happens very often in your cache
//! (in the future, you can check the statistics of the cache), you may want to
//! switch to `SegmentedCache`. It has multiple internal cache segments and each
//! segment has dedicated draining thread.
//!
//! ## Admission and Eviction
//!
//! Every time a client tries to retrieve an item from the cache, that activity is
//! retained in a historic popularity estimator. This estimator has a tiny memory
//! footprint as it uses hashing to probabilistically estimate an item's frequency.
//!
//! All caches employ [TinyLFU] (Least Frequently Used) as the admission policy. When
//! a new entry is inserted to the cache, it is temporary admitted to the cache, and
//! a recording of this insertion is added to the write queue. When the write queue
//! is drained and the main space of the cache is already full, then the historic
//! popularity estimator determines to evict one of the following entries:
//!
//! - The temporary admitted entry.
//! - Or, an entry that is selected from the main cache space by LRU (Least Recently
//!   Used) eviction policy.
//!
//! In a future release of this crate, TinyLFU admission policy will be replaced by
//! Window TinyLFU (W-TinyLFU) policy. W-TinyLFU has an admission window in front of
//! the main space. A new entry starts in the admission window and remains there as
//! long as it has high temporal locality (recency). Eventually an entry will slip
//! off from the window, then TinyLFU comes in play to determine whether or not to
//! admit the entry to the main space based on its popularity (frequency).
//!
//! [TinyLFU]: https://dl.acm.org/citation.cfm?id=3149371
//!
//! ## Expiration
//!
//! Current release supports the following cache expiration policies:
//!
//! - The time-to-live policy
//! - The time-to-idle policy
//!
//! A future release will support the following:
//!
//! - The variable expiration (which allows to set different expiration on each
//!   cached entry)
//!
//! These policies are provided with _O(1)_ time complexity:
//!
//! - The time-to-live policy uses a write-order queue.
//! - The time-to-idle policy uses an access-order queue.
//! - The variable expiration will use a [hierarchical timer wheel][timer-wheel] (*1).
//!
//! *1: If you get 404 page not found when you click on the link to the hierarchical
//! timer wheel paper, try to change the URL from `https:` to `http:`.
//!
//! [timer-wheel]: http://www.cs.columbia.edu/~nahum/w6998/papers/ton97-timing-wheels.pdf

pub(crate) mod common;
pub(crate) mod policy;
pub mod unsync;

#[cfg(any(feature = "sync", feature = "future"))]
pub(crate) mod cht;

#[cfg(feature = "dash")]
#[cfg_attr(docsrs, doc(cfg(feature = "dash")))]
pub mod dash;

#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
pub mod future;

#[cfg(any(feature = "sync", feature = "future"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "sync", feature = "future"))))]
pub mod notification;

#[cfg(feature = "sync")]
#[cfg_attr(docsrs, doc(cfg(feature = "sync")))]
pub mod sync;

#[cfg(any(feature = "sync", feature = "future"))]
pub(crate) mod sync_base;

#[cfg(any(feature = "sync", feature = "future"))]
pub use common::error::PredicateError;

pub use policy::Policy;

#[cfg(feature = "unstable-debug-counters")]
#[cfg_attr(docsrs, doc(cfg(feature = "unstable-debug-counters")))]
pub use common::concurrent::debug_counters::GlobalDebugCounters;

#[cfg(test)]
mod tests {
    #[cfg(trybuild)]
    #[test]
    fn trybuild_default() {
        let t = trybuild::TestCases::new();
        t.compile_fail("tests/compile_tests/default/clone/*.rs");
    }

    #[cfg(all(trybuild, feature = "dash"))]
    #[test]
    fn trybuild_dash() {
        let t = trybuild::TestCases::new();
        t.compile_fail("tests/compile_tests/dash/clone/*.rs");
    }

    #[cfg(all(trybuild, feature = "future"))]
    #[test]
    fn trybuild_future() {
        let t = trybuild::TestCases::new();
        t.compile_fail("tests/compile_tests/future/clone/*.rs");
    }
}
