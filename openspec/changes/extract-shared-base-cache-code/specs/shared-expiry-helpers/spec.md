## ADDED Requirements

### Requirement: Expiry helper functions SHALL be pure and synchronous

The expiry helper functions SHALL be pure functions that:
- Take only data as input (no `self` parameter)
- Return only computed results (no side effects)
- Not acquire any locks or perform async operations

#### Scenario: Check per-entry TTL expiration
- **WHEN** `is_expired_by_per_entry_ttl` is called with an entry's info and current time
- **THEN** it returns `true` if the entry's expiration time is set and has passed, `false` otherwise

#### Scenario: Check access-order expiration
- **WHEN** `is_expired_entry_ao` is called with time-to-idle, valid-after, entry, and current time
- **THEN** it returns `true` if the entry is expired by TTI or invalidated by valid-after

#### Scenario: Check write-order expiration
- **WHEN** `is_expired_entry_wo` is called with time-to-live, valid-after, entry, and current time
- **THEN** it returns `true` if the entry is expired by TTL or invalidated by valid-after

### Requirement: Expiry helpers SHALL be available to both sync and future modules

The expiry helper functions SHALL be exported from `src/common/concurrent/expiry.rs` and usable by both `sync::base_cache` and `future::base_cache` without modification.

#### Scenario: Sync cache uses shared expiry helpers
- **WHEN** sync cache code calls expiry helper functions
- **THEN** the functions work identically to the original inline code

#### Scenario: Future cache uses shared expiry helpers
- **WHEN** future cache code calls expiry helper functions
- **THEN** the functions work identically to the original inline code