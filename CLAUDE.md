# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Moka is a concurrent cache library for Rust, inspired by Java Caffeine. It provides thread-safe in-memory cache implementations with near-optimal hit ratios using TinyLFU (LFU admission + LRU eviction) policy.

## Build and Test Commands

### Building

**Note**: At least one of `sync` or `future` features must be enabled. Building without features will fail with a compile error.

```bash
# Build with sync cache (synchronous, thread-safe cache)
cargo build -F sync

# Build with future cache (async/await aware cache)
cargo build -F future

# Build with all features
cargo build --all-features
```

### Running Tests
```bash
# Run all tests including future feature and doc tests
RUSTFLAGS='--cfg trybuild' cargo test --all-features

# Run tests without default features
RUSTFLAGS='--cfg trybuild' cargo test --no-default-features -F future,sync

# Run a specific test
cargo test -F sync test_name -- --nocapture

# Run tests with a specific feature
cargo test -F sync
cargo test -F future
```

### Linting
```bash
cargo clippy --lib --tests --all-features --all-targets -- -D warnings
cargo fmt --all -- --check
```

### Generating Documentation
```bash
cargo +nightly -Z unstable-options --config 'build.rustdocflags="--cfg docsrs"' \
    doc --no-deps -F future,sync
```

## Architecture

### Key Source Files

- `src/lib.rs` - Crate entry point, feature gates
- `src/sync/cache.rs`, `src/sync/segment.rs` - `sync::Cache`, `sync::SegmentedCache`
- `src/sync/base_cache.rs` - Shared sync cache implementation
- `src/future/cache.rs` - `future::Cache`
- `src/future/base_cache.rs` - Shared async cache implementation
- `src/cht/` - Lock-free concurrent hash table (from cht crate)
- `src/common/concurrent/` - Entry types, deques, housekeeper
- `src/common/frequency_sketch.rs` - TinyLFU admission policy (Apache 2.0, from Caffeine)
- `src/common/timer_wheel.rs` - Hierarchical timer wheel for expiration (Apache 2.0, from Caffeine)
- `src/notification/` - Eviction listener support
- `src/ops/` - Compute operations
- `src/policy.rs` - Policy types (Expiry, Policy)

### Key Architectural Components

1. **Lock-free Hash Table (`cht/`)**: The central key-value storage. Uses open addressing with atomic CAS operations. Supports incremental resizing without blocking readers.

2. **Cache Policy Data Structures**: Eventually consistent with the hash table. Guarded by locks, operations applied in batches:
   - **FrequencySketch**: Count-Min sketch for TinyLFU admission decisions
   - **Deque-based LRU queues**: Access order and write order queues
   - **TimerWheel**: Hierarchical timer wheel for per-entry variable expiration

3. **Bounded Channels**: Read and write operations are recorded to channels and drained in batches:
   - Read channel: Non-blocking, discards recordings when full
   - Write channel: Blocking when full

4. **Housekeeper**: Maintenance tasks (admission decisions, eviction, expiration) run on user threads when triggered by cache operations or `run_pending_tasks()`.

### Cache Types

- `sync::Cache`: Standard thread-safe cache
- `sync::SegmentedCache`: Sharded cache for reduced contention
- `future::Cache`: Async-aware cache for use with tokio/async-std

### Concurrency Model

- **Hash table**: Strong consistency, lock-free, immediate updates
- **Policy structures**: Eventual consistency, guarded by locks, batched operations

The `get` method returns `Option<V>` (cloned value) rather than `Option<&V>` because entries can be evicted by other threads at any time.

## Design Documentation

- `docs/adr/` — Architecture Decision Records: the "why" behind non-obvious
  design decisions. See `docs/adr/README.md` for the index, the template, and
  the criteria for when to write one.

Rules:

1. If your change makes a non-obvious design decision (public API shape or
   semantics, concurrency protocol or memory ordering, dependency choice,
   MSRV/edition/licensing policy), add a new ADR in the same PR.
2. Never edit an accepted ADR. To revise a decision, write a new ADR that
   supersedes the old one and change the old ADR's Status line to
   `Superseded by ADR-NNNN`. Do not renumber ADRs.

## Important Licensing Notes

`src/common/frequency_sketch.rs` and `src/common/timer_wheel.rs` are ported from Caffeine and are **Apache 2.0 only** (not dual-licensed like the rest of the crate).

## MSRV

Rust 1.71.1 for both `sync` and `future` features. Note that Rust 1.71.1 does not support Rust 2024 edition, so this crate uses the 2021 edition.
