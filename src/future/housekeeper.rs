use crate::common::{
    concurrent::{
        atomic_time::AtomicInstant,
        constants::{
            MAX_SYNC_REPEATS, PERIODICAL_SYNC_INITIAL_DELAY_MILLIS, READ_LOG_FLUSH_POINT,
            WRITE_LOG_FLUSH_POINT,
        },
    },
    time::{CheckedTimeOps, Instant},
};

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use async_trait::async_trait;

#[async_trait]
pub(crate) trait InnerSync {
    async fn flush(&self, max_sync_repeats: usize);
    fn now(&self) -> Instant;
}

pub(crate) struct Housekeeper {
    is_sync_running: AtomicBool,
    sync_after: AtomicInstant,
}

impl Default for Housekeeper {
    fn default() -> Self {
        Self {
            is_sync_running: Default::default(),
            sync_after: AtomicInstant::new(Self::sync_after(Instant::now())),
        }
    }
}

impl Housekeeper {
    pub(crate) fn should_apply_reads(&self, ch_len: usize, now: Instant) -> bool {
        self.should_apply(ch_len, READ_LOG_FLUSH_POINT / 8, now)
    }

    pub(crate) fn should_apply_writes(&self, ch_len: usize, now: Instant) -> bool {
        self.should_apply(ch_len, WRITE_LOG_FLUSH_POINT / 8, now)
    }

    #[inline]
    fn should_apply(&self, ch_len: usize, ch_flush_point: usize, now: Instant) -> bool {
        ch_len >= ch_flush_point || self.sync_after.instant().unwrap() >= now
    }

    pub(crate) async fn try_sync<T: InnerSync>(&self, cache: &T) -> bool {
        // Try to flip the value of sync_scheduled from false to true.
        match self.is_sync_running.compare_exchange(
            false,
            true,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                let now = cache.now();
                self.sync_after.set_instant(Self::sync_after(now));

                cache.flush(MAX_SYNC_REPEATS).await;

                self.is_sync_running.store(false, Ordering::Release);
                true
            }
            Err(_) => false,
        }
    }

    fn sync_after(now: Instant) -> Instant {
        let dur = Duration::from_millis(PERIODICAL_SYNC_INITIAL_DELAY_MILLIS);
        let ts = now.checked_add(dur);
        // Assuming that `now` is current wall clock time, this should never fail at
        // least next millions of years.
        ts.expect("Timestamp overflow")
    }
}
