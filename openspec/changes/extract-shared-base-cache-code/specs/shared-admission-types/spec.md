## ADDED Requirements

### Requirement: EvictionCounters SHALL track cache statistics

`EvictionCounters` SHALL be a plain data structure that tracks:
- `entry_count`: Number of entries in cache
- `weighted_size`: Total weight of entries
- `eviction_count`: Number of evictions performed

#### Scenario: Counters update on entry operations
- **WHEN** `saturating_add` or `saturating_sub` is called with entry count and weight
- **THEN** the counters are updated with saturation to prevent overflow

### Requirement: EntrySizeAndFrequency SHALL aggregate admission metrics

`EntrySizeAndFrequency` SHALL aggregate metrics for admission decisions:
- `policy_weight`: Total weight of the entry
- `freq`: Aggregated frequency from the sketch

#### Scenario: Frequency is accumulated from sketch
- **WHEN** `add_frequency` is called with a frequency sketch and hash
- **THEN** the frequency value is added to the accumulated `freq`

### Requirement: AdmissionResult SHALL represent admission decision

`AdmissionResult<K>` SHALL be an enum with two variants:
- `Admitted { victim_keys }`: Entry admitted with potential victims to evict
- `Rejected`: Entry rejected from cache

#### Scenario: Admitted result contains victim keys
- **WHEN** an entry is admitted
- **THEN** `AdmissionResult::Admitted` contains a `SmallVec` of `(KeyHash<K>, Option<Instant>)` tuples

### Requirement: Types SHALL be shared between sync and future modules

All admission types SHALL be exported from `src/common/concurrent/admission.rs` and usable by both sync and future base_cache modules.

#### Scenario: Types are identical in both modules
- **WHEN** sync and future modules use the shared types
- **THEN** the types have identical structure and behavior