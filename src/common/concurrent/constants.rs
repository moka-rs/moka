pub(crate) const MAX_SYNC_REPEATS: usize = 4;
pub(crate) const PERIODICAL_SYNC_INITIAL_DELAY_MILLIS: u64 = 300;

pub(crate) const READ_LOG_FLUSH_POINT: usize = 64;
pub(crate) const READ_LOG_SIZE: usize = READ_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2); // 384

pub(crate) const WRITE_LOG_FLUSH_POINT: usize = 64;
pub(crate) const WRITE_LOG_SIZE: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2); // 384

// TODO: Calculate the batch size based on the number of entries in the cache (or an
// estimated number of entries to evict)
pub(crate) const EVICTION_BATCH_SIZE: usize = WRITE_LOG_SIZE;
pub(crate) const INVALIDATION_BATCH_SIZE: usize = WRITE_LOG_SIZE;

/// The default timeout duration for the `run_pending_tasks` method.
pub(crate) const DEFAULT_RUN_PENDING_TASKS_TIMEOUT_MILLIS: u64 = 100;

#[cfg(feature = "sync")]
pub(crate) const WRITE_RETRY_INTERVAL_MICROS: u64 = 50;
