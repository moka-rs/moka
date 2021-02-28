#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

//! Moka is a fast, concurrent cache library for Rust. Moka is inspired by
//! [Caffeine][caffeine-git] (Java) and [Ristretto][ristretto-git] (Go).
//!
//! Moka provides in-memory concurrent cache implementations that support full
//! concurrency of retrievals and a high expected concurrency for updates. <!-- , and multiple ways to bound the cache. -->
//! They utilize a lock-free concurrent hash table `cht::SegmentedHashMap` from the
//! [cht][cht-crate] crate for the central key-value storage.
//!
//! Moka also provides an in-memory, not thread-safe cache implementation for single
//! threaded applications.
//!
//! All cache implementations perform a best-effort bounding of the map using an entry
//! replacement algorithm to determine which entries to evict when the capacity is
//! exceeded.
//!
//! [caffeine-git]: https://github.com/ben-manes/caffeine
//! [ristretto-git]: https://github.com/dgraph-io/ristretto
//! [cht-crate]: https://crates.io/crates/cht
//!
//! # Features
//!
//! - Thread-safe, highly concurrent in-memory cache implementations:
//!     - Blocking caches that can be shared across OS threads.
//!     - An asynchronous (futures aware) cache that can be accessed inside and
//!       outside of asynchronous contexts.
//! - A not thread-safe, in-memory cache implementation for single threaded applications.
//! - Caches are bounded by the maximum number of entries.
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
//! - Thread-safe, blocking caches:
//!     - [`sync::Cache`][sync-cache-struct] and
//!       [`sync::SegmentedCache`][sync-seg-cache-struct].
//! - An asynchronous (futures aware) cache:
//!     - [`future::Cache`][future-cache-struct].
//! - A not thread-safe, blocking cache for single threaded applications:
//!     - [`unsync::Cache`][unsync-cache-struct].
//!
//! [future-cache-struct]: ./future/struct.Cache.html
//! [sync-cache-struct]: ./sync/struct.Cache.html
//! [sync-seg-cache-struct]: ./sync/struct.SegmentedCache.html
//! [unsync-cache-struct]: ./unsync/struct.Cache.html
//!
//! # Minimum Supported Rust Version
//!
//! This crate's minimum supported Rust version (MSRV) is 1.45.2.
//!
//! If no crate feature is enabled, MSRV will be updated conservatively. When using
//! features like `future`, MSRV might be updated more frequently, up to the latest
//! stable. In both cases, increasing MSRV is _not_ considered a semver-breaking
//! change.
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
//! - The variable expiration
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

#[cfg(feature = "future")]
pub mod future;

pub mod sync;
pub mod unsync;

pub(crate) mod common;
