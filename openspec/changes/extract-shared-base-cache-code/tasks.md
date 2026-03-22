## 1. Create expiry.rs module

- [x] 1.1 Create `src/common/concurrent/expiry.rs` file
- [x] 1.2 Extract `is_expired_by_per_entry_ttl` function
- [x] 1.3 Extract `is_expired_entry_ao` function
- [x] 1.4 Extract `is_expired_entry_wo` function
- [x] 1.5 Extract `is_entry_expired_ao_or_invalid` function
- [x] 1.6 Extract `is_entry_expired_wo_or_invalid` function
- [x] 1.7 Extract `is_invalid_entry` function
- [x] 1.8 Extract `is_expired_by_tti` function
- [x] 1.9 Extract `is_expired_by_ttl` function
- [x] 1.10 Add `#[inline]` attribute to all functions

## 2. Create admission.rs module (types)

- [x] 2.1 Create `src/common/concurrent/admission.rs` file
- [x] 2.2 Extract `EvictionCounters` struct with `new`, `saturating_add`, `saturating_sub`, `incr_eviction_count` methods
- [x] 2.3 Extract `EntrySizeAndFrequency` struct with `new`, `add_policy_weight`, `add_frequency` methods
- [x] 2.4 Extract `AdmissionResult<K>` enum with `Admitted` and `Rejected` variants

## 3. Create admission.rs module (logic)

- [x] 3.1 Extract `admit` function with all size-aware admission logic
- [x] 3.2 Extract `handle_admit` function
- [x] 3.3 Extract `update_timer_wheel` function
- [x] 3.4 Extract `handle_remove` function
- [x] 3.5 Extract `handle_remove_with_deques` function
- [x] 3.6 Extract `handle_remove_without_timer_wheel` function
- [x] 3.7 Add `#[inline]` attributes where appropriate

## 4. Update module exports

- [x] 4.1 Add `pub(crate) mod expiry;` to `src/common/concurrent.rs`
- [x] 4.2 Add `pub(crate) mod admission;` to `src/common/concurrent.rs`
- [x] 4.3 ~~Add re-export for expiry functions~~ (Removed: callers use `expiry::function_name` directly)
- [x] 4.4 Add `pub(crate) use admission::{...};` to re-export types

## 5. Update sync::base_cache

- [x] 5.1 Remove duplicated expiry functions from `src/sync/base_cache.rs`
- [x] 5.2 Remove duplicated types from `src/sync/base_cache.rs`
- [x] 5.3 Remove duplicated admission logic functions from `src/sync/base_cache.rs`
- [x] 5.4 Update imports to use shared module
- [x] 5.5 Verify compilation

## 6. Update future::base_cache

- [x] 6.1 Remove duplicated expiry functions from `src/future/base_cache.rs`
- [x] 6.2 Remove duplicated types from `src/future/base_cache.rs`
- [x] 6.3 Remove duplicated admission logic functions from `src/future/base_cache.rs`
- [x] 6.4 Update imports to use shared module
- [x] 6.5 Verify compilation

## 7. Verification

- [x] 7.1 Run `cargo build --all-features`
- [x] 7.2 Run `cargo clippy --all-features -- -D warnings`
- [x] 7.3 Run `cargo fmt --all -- --check`
- [x] 7.4 Run `cargo test --all-features` (with `RUSTFLAGS='--cfg trybuild'`) — Note: 153 lib tests pass; trybuild compile test has Rust version-related output diff (unrelated to this change)
- [x] 7.5 Verify no performance regression (optional: run benchmarks) — N/A (no benchmarks; pure refactoring)