use super::constants::{MAX_SYNC_REPEATS, PERIODICAL_SYNC_INITIAL_DELAY_MILLIS};

use super::{
    atomic_time::AtomicInstant,
    constants::{READ_LOG_FLUSH_POINT, WRITE_LOG_FLUSH_POINT},
};
use crate::common::time::{CheckedTimeOps, Instant};

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub(crate) trait InnerSync {
    fn run_pending_tasks(&self, max_sync_repeats: usize);

    fn now(&self) -> Instant;
}

pub(crate) struct Housekeeper {
    is_sync_running: AtomicBool,
    run_after: AtomicInstant,
    auto_run_enabled: AtomicBool,
}

impl Default for Housekeeper {
    fn default() -> Self {
        Self {
            is_sync_running: Default::default(),
            run_after: AtomicInstant::new(Self::sync_after(Instant::now())),
            auto_run_enabled: AtomicBool::new(true),
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
        self.auto_run_enabled.load(Ordering::Relaxed)
            && (ch_len >= ch_flush_point || now >= self.run_after.instant().unwrap())
    }

    pub(crate) fn run_pending_tasks<T: InnerSync>(&self, cache: &T) {
        self.do_run_pending_tasks(cache);
    }

    pub(crate) fn try_run_pending_tasks<T: InnerSync>(&self, cache: &T) -> bool {
        // Try to flip the value of sync_scheduled from false to true.
        match self.is_sync_running.compare_exchange(
            false,
            true,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.do_run_pending_tasks(cache);
                true
            }
            Err(_) => false,
        }
    }

    fn do_run_pending_tasks<T: InnerSync>(&self, cache: &T) {
        let now = cache.now();
        self.run_after.set_instant(Self::sync_after(now));

        cache.run_pending_tasks(MAX_SYNC_REPEATS);

        self.is_sync_running.store(false, Ordering::Release);
    }

    fn sync_after(now: Instant) -> Instant {
        let dur = Duration::from_millis(PERIODICAL_SYNC_INITIAL_DELAY_MILLIS);
        let ts = now.checked_add(dur);
        // Assuming that `now` is current wall clock time, this should never fail at
        // least next millions of years.
        ts.expect("Timestamp overflow")
    }
}

#[cfg(test)]
impl Housekeeper {
    pub(crate) fn disable_auto_run(&self) {
        self.auto_run_enabled.store(false, Ordering::Relaxed);
    }

    pub(crate) fn reset_run_after(&self, now: Instant) {
        self.run_after.set_instant(Self::sync_after(now));
    }
}
