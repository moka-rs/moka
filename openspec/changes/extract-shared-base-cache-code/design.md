## Context

The `sync/base_cache.rs` (3,362 lines) and `future/base_cache.rs` (3,602 lines) modules share approximately 70-80% identical code. The key difference is that sync uses `parking_lot` locks with blocking operations, while future uses `async_lock` with async/await.

However, many functions are purely synchronous and do not involve lock acquisition - these can be safely extracted and shared.

### Constraints

- **Cannot share async vs sync code**: `fn` and `async fn` have fundamentally different semantics in Rust
- **Cannot abstract locks**: `mutex.lock()` vs `mutex.lock().await` cannot be unified
- **No performance regression**: Extracted code must be inlined as effectively as current code

## Goals / Non-Goals

**Goals:**
- Extract ~500 lines of duplicated synchronous code
- Reduce maintenance burden and risk of divergent behavior
- Maintain identical performance characteristics

**Non-Goals:**
- Refactoring async/sync orchestration code (requires locks)
- Creating trait abstractions for lock types
- Changing any public API or behavior

## Decisions

### 1. Module Structure

**Decision:** Create two new modules under `src/common/concurrent/`:
- `expiry.rs` - Expiry checking helper functions
- `admission.rs` - Admission types and logic

**Rationale:** Follows existing pattern in `src/common/concurrent/` which already contains shared types (`deques.rs`, `entry_info.rs`, `arc.rs`, `constants.rs`).

**Alternatives considered:**
- Single `base_cache_shared.rs` - rejected, mixing unrelated concerns
- Trait-based abstraction - rejected, over-engineering for this scope

### 2. Extracted Functions

**Functions to extract to `expiry.rs`:**
```rust
fn is_expired_by_per_entry_ttl<K>(...) -> bool
fn is_expired_entry_ao(...) -> bool
fn is_expired_entry_wo(...) -> bool
fn is_entry_expired_ao_or_invalid(...) -> (bool, bool)
fn is_entry_expired_wo_or_invalid(...) -> (bool, bool)
fn is_invalid_entry(...) -> bool
fn is_expired_by_tti(...) -> bool
fn is_expired_by_ttl(...) -> bool
```
All are pure functions with no side effects, taking only data as input.

**Types to extract to `admission.rs`:**
```rust
struct EvictionCounters { ... }
struct EntrySizeAndFrequency { ... }
enum AdmissionResult<K> { ... }
```

**Methods to extract to `admission.rs`:**
```rust
fn admit<K, V, S>(...) -> AdmissionResult<K>  // ~65 lines
fn handle_admit<K, V>(...)  // ~25 lines
fn update_timer_wheel<K, V>(...)  // ~65 lines
fn handle_remove<K, V>(...)  // ~50 lines
fn handle_remove_with_deques<K, V>(...)  // ~50 lines
fn handle_remove_without_timer_wheel<K, V>(...)  // ~30 lines
```

All methods operate on passed-in mutable references without acquiring locks themselves.

### 3. Visibility

**Decision:** Make all extracted items `pub(crate)` to match existing items in `src/common/concurrent.rs`.

**Rationale:** Consistent with existing shared types like `KeyHash`, `ValueEntry`, `ReadOp`, `WriteOp`.

### 4. Module Re-exports

**Decision:** No re-exports for expiry functions. Callers will use `crate::common::concurrent::expiry::function_name`.

For admission types, re-export only the type names:
```rust
pub(crate) mod expiry;
pub(crate) mod admission;

pub(crate) use admission::{EvictionCounters, EntrySizeAndFrequency, AdmissionResult};
```

**Rationale:**
- Avoids namespace pollution from function-level re-exports
- Makes it clear that functions are related to expiry logic when using `expiry::function_name`
- Prevents potential naming conflicts with other modules
- Type re-exports for admission are acceptable since types are typically used directly and the names are distinctive

## Risks / Trade-offs

| Risk | Mitigation |
|------|------------|
| Inlining regression | Use `#[inline]` on all extracted functions |
| Circular dependencies | Functions only depend on types already in `common/` |
| Merge conflicts during refactor | Do extraction in single commit, minimal changes |

## Migration Plan

1. Create new modules with extracted code
2. Update `src/common/concurrent.rs` to export new modules
3. Update `src/sync/base_cache.rs` to use shared code
4. Update `src/future/base_cache.rs` to use shared code
5. Run tests to verify no regression

**Rollback:** Each file change is independent; can revert individual commits if issues arise.