pub(crate) const MAX_SYNC_REPEATS: usize = 4;
pub(crate) const PERIODICAL_SYNC_INITIAL_DELAY_MILLIS: u64 = 300;

pub(crate) const READ_LOG_FLUSH_POINT: usize = 512;
pub(crate) const READ_LOG_SIZE: usize = READ_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

pub(crate) const WRITE_LOG_FLUSH_POINT: usize = 512;
pub(crate) const WRITE_LOG_SIZE: usize = WRITE_LOG_FLUSH_POINT * (MAX_SYNC_REPEATS + 2);

#[cfg(feature = "sync")]
pub(crate) const WRITE_RETRY_INTERVAL_MICROS: u64 = 50;
