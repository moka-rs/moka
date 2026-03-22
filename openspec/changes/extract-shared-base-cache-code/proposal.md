## Why

The `sync/base_cache.rs` and `future/base_cache.rs` modules contain approximately 500 lines of duplicated code that is functionally identical. This redundancy increases maintenance burden and risk of divergent behavior when making changes.

## What Changes

- Extract shared helper functions for expiry checking to `src/common/concurrent/expiry.rs`
- Extract shared data structures (`EvictionCounters`, `EntrySizeAndFrequency`, `AdmissionResult`) to `src/common/concurrent/admission.rs`
- Extract shared admission logic methods (`admit`, `handle_admit`, `update_timer_wheel`, `handle_remove*`) to shared module
- Both `sync/base_cache.rs` and `future/base_cache.rs` will import from the new shared modules

## Capabilities

### New Capabilities

- `shared-expiry-helpers`: Pure functions for checking entry expiration (no async/locks)
- `shared-admission-types`: Data structures for cache admission policy
- `shared-admission-logic`: Synchronous admission decision and deque manipulation methods

### Modified Capabilities

None - this is a pure refactoring with no behavior changes.

## Impact

**Affected Files:**
- `src/sync/base_cache.rs` - will import from new shared modules
- `src/future/base_cache.rs` - will import from new shared modules

**New Files:**
- `src/common/concurrent/expiry.rs` - expiry helper functions
- `src/common/concurrent/admission.rs` - admission types and logic

**No API Changes** - internal refactoring only