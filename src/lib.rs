#![warn(clippy::all)]
#![warn(rust_2018_idioms)]
// Temporary disable this lint as the MSRV (1.51) require an older lint name:
// #![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! Moka is a fast, concurrent cache library for Rust. Moka is inspired by the
//! [Caffeine][caffeine-git] library for Java.
//!
//! Moka provides in-memory concurrent cache implementations on top of hash maps.
//! They support full concurrency of retrievals and a high expected concurrency for
//! updates. They utilize a lock-free concurrent hash table as the central key-value
//! storage.
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
//!     - An asynchronous (futures aware) cache.
//! - A cache can be bounded by one of the followings:
//!     - The maximum number of entries.
//!     - The total weighted size of entries. (Size aware eviction)
//! - Maintains near optimal hit ratio by using an entry replacement algorithms
//!   inspired by Caffeine:
//!     - Admission to a cache is controlled by the Least Frequently Used (LFU)
//!       policy.
//!     - Eviction from a cache is controlled by the Least Recently Used (LRU)
//!       policy.
//!     - [More details and some benchmark results are available here][tiny-lfu].
//! - Supports expiration policies:
//!     - Time to live.
//!     - Time to idle.
//!     - Per-entry variable expiration.
//! - Supports eviction listener, a callback function that will be called when an
//!   entry is removed from the cache.
//!
//! [tiny-lfu]: https://github.com/moka-rs/moka/wiki#admission-and-eviction-policies
//!
//! # Examples
//!
//! See the following document:
//!
//! - Thread-safe, synchronous caches:
//!     - [`sync::Cache`][sync-cache-struct]
//!     - [`sync::SegmentedCache`][sync-seg-cache-struct]
//! - An asynchronous (futures aware) cache:
//!     - [`future::Cache`][future-cache-struct] (Requires "future" feature)
//!
//! [future-cache-struct]: ./future/struct.Cache.html
//! [sync-cache-struct]: ./sync/struct.Cache.html
//! [sync-seg-cache-struct]: ./sync/struct.SegmentedCache.html
//!
//! **NOTE:** The following caches have been moved to a separate crate called
//! "[mini-moka][mini-moka-crate]".
//!
//! - Non concurrent cache for single threaded applications:
//!     - `moka::unsync::Cache` → [`mini_moka::unsync::Cache`][unsync-cache-struct]
//! - A simple, thread-safe, synchronous cache:
//!     - `moka::dash::Cache` → [`mini_moka::sync::Cache`][dash-cache-struct]
//!
//! [mini-moka-crate]: https://crates.io/crates/mini-moka
//! [unsync-cache-struct]:
//!     https://docs.rs/mini-moka/latest/mini_moka/unsync/struct.Cache.html
//! [dash-cache-struct]:
//!     https://docs.rs/mini-moka/latest/mini_moka/sync/struct.Cache.html
//!
//! # Minimum Supported Rust Versions
//!
//! This crate's minimum supported Rust versions (MSRV) are the followings:
//!
//! | Feature          | MSRV                      |
//! |:-----------------|:-------------------------:|
//! | default features | Rust 1.65.0 (Nov 3, 2022) |
//! | `future`         | Rust 1.65.0 (Nov 3, 2022) |
//!
//! It will keep a rolling MSRV policy of at least 6 months. If only the default
//! features are enabled, MSRV will be updated conservatively. When using other
//! features, like `future`, MSRV might be updated more frequently, up to the latest
//! stable. In both cases, increasing MSRV is _not_ considered a semver-breaking
//! change.

#[cfg(not(any(feature = "sync", feature = "future")))]
compile_error!(
    "At least one of the crate features `sync` or `future` must be enabled for \
    `moka` crate. Please update your dependencies in Cargo.toml"
);

#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
pub mod future;

#[cfg(feature = "sync")]
#[cfg_attr(docsrs, doc(cfg(feature = "sync")))]
pub mod sync;

#[cfg(any(feature = "sync", feature = "future"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "sync", feature = "future"))))]
pub mod notification;

#[cfg(any(feature = "sync", feature = "future"))]
pub(crate) mod cht;

#[cfg(any(feature = "sync", feature = "future"))]
pub(crate) mod common;

#[cfg(any(feature = "sync", feature = "future"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "sync", feature = "future"))))]
pub mod ops;

#[cfg(any(feature = "sync", feature = "future"))]
pub mod policy;

#[cfg(any(feature = "sync", feature = "future"))]
pub(crate) mod sync_base;

#[cfg(any(feature = "sync", feature = "future"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "sync", feature = "future"))))]
pub use common::error::PredicateError;

#[cfg(any(feature = "sync", feature = "future"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "sync", feature = "future"))))]
pub use common::entry::Entry;

#[cfg(any(feature = "sync", feature = "future"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "sync", feature = "future"))))]
pub use policy::{Expiry, Policy};

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

    #[cfg(all(trybuild, feature = "future"))]
    #[test]
    fn trybuild_future() {
        let t = trybuild::TestCases::new();
        t.compile_fail("tests/compile_tests/future/clone/*.rs");
    }
}
