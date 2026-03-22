## ADDED Requirements

### Requirement: Admission decision logic SHALL be synchronous

The `admit` function SHALL perform size-aware admission without:
- Acquiring any locks
- Performing any async operations
- Calling any async notification handlers

The function SHALL take:
- `candidate`: The entry's size and frequency metrics
- `cache`: Reference to the hash map (read-only)
- `deqs`: Mutable reference to deques (already borrowed)
- `freq`: Reference to frequency sketch

#### Scenario: Candidate admitted with victims
- **WHEN** candidate's frequency exceeds aggregated victims' frequency
- **AND** victims' total weight covers candidate's weight
- **THEN** returns `AdmissionResult::Admitted` with victim keys

#### Scenario: Candidate rejected
- **WHEN** candidate's frequency does not exceed victims' frequency
- **OR** insufficient victims are available
- **THEN** returns `AdmissionResult::Rejected`

### Requirement: handle_admit SHALL update deques synchronously

`handle_admit` SHALL update the deques and counters without acquiring locks:
- Increment entry and weight counters
- Push entry to access-order deque
- Push entry to write-order deque (if enabled)
- Set entry's admitted flag

#### Scenario: Entry admitted to deques
- **WHEN** `handle_admit` is called with an entry
- **THEN** the entry is added to probation deque and write-order deque

### Requirement: update_timer_wheel SHALL manage timer entries

`update_timer_wheel` SHALL update timer wheel registration based on expiration time:
- Schedule new entries with expiration time
- Reschedule entries with updated expiration time
- Deschedule entries without expiration time

#### Scenario: Entry scheduled for expiration
- **WHEN** entry has expiration time and is not in timer wheel
- **THEN** entry is scheduled in timer wheel

### Requirement: handle_remove functions SHALL clean up deques

The `handle_remove*` family of functions SHALL remove entries from deques without acquiring locks:
- Remove from access-order deque
- Remove from write-order deque
- Update timer wheel
- Update counters

#### Scenario: Entry removed from all structures
- **WHEN** `handle_remove` is called with an entry
- **THEN** entry is unlinked from deques, descheduled from timer, and counters updated

### Requirement: Admission logic SHALL be shared between sync and future

All admission logic functions SHALL be exported from `src/common/concurrent/admission.rs` and callable from both sync and future base_cache modules.

#### Scenario: Sync cache uses shared admission logic
- **WHEN** sync cache calls `admit`, `handle_admit`, etc.
- **THEN** behavior is identical to original inline implementation

#### Scenario: Future cache uses shared admission logic
- **WHEN** future cache calls `admit`, `handle_admit`, etc.
- **THEN** behavior is identical to original inline implementation