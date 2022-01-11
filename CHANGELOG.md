# Moka &mdash; Change Log

## Version 0.6.4

- **Important Fix**: A memory leak issue (#66 below) was found in all previous
  versions (since v0.1.0) and v0.7.0. It was fixed in this version and v0.7.1. All
  users are encouraged to upgrade to this version or v0.7.1.

### Fixed

- Fix a memory leak that will happen when evicting/expiring an entry or manually
  invalidating an entry. ([#66][gh-pull-0066])


## Version 0.6.3

### Fixed

- Fix a bug in `get_or_insert_with` and `get_or_try_insert_with` methods of
  `future::Cache`, which caused a panic if previously inserting task aborted.
  ([#59][gh-issue-0059])


## Version 0.6.2

### Removed

- Remove `Send` and `'static` bounds from `get_or_insert_with` and
  `get_or_try_insert_with` methods of `future::Cache`. ([#53][gh-pull-0053])

### Fixed

- Protect overflow when computing expiration. ([#56][gh-pull-0056])


## Version 0.6.1

### Changed

- Replace futures with futures-util. ([#47][gh-pull-0047])


## Version 0.6.0

### Fixed

- Fix a bug in `get_or_insert_with` and `get_or_try_insert_with` methods of
  `future::Cache` and `sync::Cache`; a panic in the `init` future/closure
  causes subsequent calls on the same key to get "unreachable code" panics.
  ([#43][gh-issue-0043])

### Changed

- Change `get_or_try_insert_with` to return a concrete error type rather
  than a trait object. ([#23][gh-pull-0023], [#37][gh-pull-0037])


## Version 0.5.4

### Changed

-  Restore quanta dependency on some 32-bit platforms such as
   `armv5te-unknown-linux-musleabi` or `mips-unknown-linux-musl`.
   ([#42][gh-pull-0042])


## Version 0.5.3

### Added

- Add support for some 32-bit platforms where `std::sync::atomic::AtomicU64` is not
  provided. (e.g. `armv5te-unknown-linux-musleabi` or `mips-unknown-linux-musl`)
  ([#38][gh-issue-0038])
    - On these platforms, you will need to disable the default features of Moka.
      See [the relevant section][resolving-error-on-32bit] of the README.


## Version 0.5.2

### Fixed

- Fix a bug in `get_or_insert_with` and `get_or_try_insert_with` methods of
  `future::Cache` by adding missing bounds `Send` and `'static` to the `init`
  future. Without this fix, these methods will accept non-`Send` or
  non-`'static` future and may cause undefined behavior.
  ([#31][gh-issue-0031])
- Fix `usize` overflow on big cache capacity. ([#28][gh-pull-0028])

### Added

- Add examples for `get_or_insert_with` and `get_or_try_insert_with`
  methods to the docs. ([#30][gh-pull-0030])

### Changed

- Downgrade crossbeam-epoch used in moka-cht from v0.9.x to v0.8.x as a possible
  workaround for segmentation faults on many-core CPU machines.
  ([#33][gh-pull-0033])


## Version 0.5.1

### Changed

- Replace a dependency cht v0.4 with moka-cht v0.5. ([#22][gh-pull-0022])


## Version 0.5.0

### Added

- Add `get_or_insert_with` and `get_or_try_insert_with` methods to `sync` and
  `future` caches. ([#20][gh-pull-0020])


## Version 0.4.0

### Fixed

- **Breaking change**: Now `sync::{Cache, SegmentedCache}` and `future::Cache`
  require `Send`, `Sync` and `'static` for the generic parameters `K` (key),
  `V` (value) and `S` (hasher state). This is necessary to prevent potential
  undefined behaviors in applications using single-threaded async runtime such as
  Actix-rt. ([#19][gh-pull-0019])

### Added

- Add `invalidate_entries_if` method to `sync`, `future` and `unsync` caches.
  ([#12][gh-pull-0012])


## Version 0.3.1

### Changed

- Stop skeptic from having to be compiled by all downstream users. ([#16][gh-pull-0016])


## Version 0.3.0

### Added

- Add an unsync cache (`moka::unsync::Cache`) and its builder for single-thread
  applications. ([#9][gh-pull-0009])
- Add `invalidate_all` method to `sync`, `future` and `unsync` caches.
  ([#11][gh-pull-0011])

### Fixed

- Fix problems including segfault caused by race conditions between the sync/eviction
  thread and client writes. (Addressed as a part of [#11][gh-pull-0011]).


## Version 0.2.0

### Added

- Add an asynchronous, futures aware cache (`moka::future::Cache`) and its builder.
  ([#7][gh-pull-0007])


## Version 0.1.0

### Added

- Add thread-safe, highly concurrent in-memory cache implementations
  (`moka::sync::{Cache, SegmentedCache}`) with the following features:
    - Bounded by the maximum number of elements.
    - Maintains good hit rate by using entry replacement algorithms inspired by
      [Caffeine][caffeine-git]:
        - Admission to a cache is controlled by the Least Frequently Used (LFU) policy.
        - Eviction from a cache is controlled by the Least Recently Used (LRU) policy.
    - Expiration policies:
        - Time to live
        - Time to idle


<!-- Links -->

[caffeine-git]: https://github.com/ben-manes/caffeine

[resolving-error-on-32bit]: https://github.com/moka-rs/moka#resolving-compile-errors-on-some-32-bit-platforms

[gh-issue-0059]: https://github.com/moka-rs/moka/issues/59/
[gh-issue-0043]: https://github.com/moka-rs/moka/issues/43/
[gh-issue-0038]: https://github.com/moka-rs/moka/issues/38/
[gh-issue-0031]: https://github.com/moka-rs/moka/issues/31/

[gh-pull-0066]: https://github.com/moka-rs/moka/pull/66/
[gh-pull-0056]: https://github.com/moka-rs/moka/pull/56/
[gh-pull-0053]: https://github.com/moka-rs/moka/pull/53/
[gh-pull-0047]: https://github.com/moka-rs/moka/pull/47/
[gh-pull-0042]: https://github.com/moka-rs/moka/pull/42/
[gh-pull-0037]: https://github.com/moka-rs/moka/pull/37/
[gh-pull-0033]: https://github.com/moka-rs/moka/pull/33/
[gh-pull-0030]: https://github.com/moka-rs/moka/pull/30/
[gh-pull-0028]: https://github.com/moka-rs/moka/pull/28/
[gh-pull-0023]: https://github.com/moka-rs/moka/pull/23/
[gh-pull-0022]: https://github.com/moka-rs/moka/pull/22/
[gh-pull-0020]: https://github.com/moka-rs/moka/pull/20/
[gh-pull-0019]: https://github.com/moka-rs/moka/pull/19/
[gh-pull-0016]: https://github.com/moka-rs/moka/pull/16/
[gh-pull-0012]: https://github.com/moka-rs/moka/pull/12/
[gh-pull-0011]: https://github.com/moka-rs/moka/pull/11/
[gh-pull-0009]: https://github.com/moka-rs/moka/pull/9/
[gh-pull-0007]: https://github.com/moka-rs/moka/pull/7/
