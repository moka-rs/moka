use super::constants::{
    DEFAULT_RUN_PENDING_TASKS_TIMEOUT_MILLIS, MAX_SYNC_REPEATS,
    PERIODICAL_SYNC_INITIAL_DELAY_MILLIS,
};

use super::{
    atomic_time::AtomicInstant,
    constants::{READ_LOG_FLUSH_POINT, WRITE_LOG_FLUSH_POINT},
};
use crate::common::time::{CheckedTimeOps, Instant};

use parking_lot::{Mutex, MutexGuard};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub(crate) trait InnerSync {
    /// Runs the pending tasks. Returns `true` if there are more entries to evict.
    fn run_pending_tasks(&self, max_sync_repeats: usize, timeout: Option<Duration>) -> bool;

    fn now(&self) -> Instant;
}

pub(crate) struct Housekeeper {
    run_lock: Mutex<()>,
    run_after: AtomicInstant,
    /// A flag to indicate if the last `run_pending_tasks` call left more entries to
    /// evict.
    ///
    /// Used only when the eviction listener closure is set for this cache instance
    /// because, if not, `run_pending_tasks` will never leave more entries to evict.
    more_entries_to_evict: Option<AtomicBool>,
    /// The timeout duration for the `run_pending_tasks` method. This is a safe-guard
    /// to prevent cache read/write operations (that may call `run_pending_tasks`
    /// internally) from being blocked for a long time when the user wrote a slow
    /// eviction listener closure.
    ///
    /// Used only when the eviction listener closure is set for this cache instance.
    maintenance_task_timeout: Option<Duration>,
    auto_run_enabled: AtomicBool,
}

impl Housekeeper {
    pub(crate) fn new(is_eviction_listener_enabled: bool) -> Self {
        let (more_entries_to_evict, maintenance_task_timeout) = if is_eviction_listener_enabled {
            (
                Some(AtomicBool::new(false)),
                Some(Duration::from_millis(
                    DEFAULT_RUN_PENDING_TASKS_TIMEOUT_MILLIS,
                )),
            )
        } else {
            (None, None)
        };

        Self {
            run_lock: Mutex::default(),
            run_after: AtomicInstant::new(Self::sync_after(Instant::now())),
            more_entries_to_evict,
            maintenance_task_timeout,
            auto_run_enabled: AtomicBool::new(true),
        }
    }

    pub(crate) fn should_apply_reads(&self, ch_len: usize, now: Instant) -> bool {
        self.more_entries_to_evict() || self.should_apply(ch_len, READ_LOG_FLUSH_POINT, now)
    }

    pub(crate) fn should_apply_writes(&self, ch_len: usize, now: Instant) -> bool {
        self.more_entries_to_evict() || self.should_apply(ch_len, WRITE_LOG_FLUSH_POINT, now)
    }

    #[inline]
    fn more_entries_to_evict(&self) -> bool {
        self.more_entries_to_evict
            .as_ref()
            .map(|v| v.load(Ordering::Acquire))
            .unwrap_or(false)
    }

    fn set_more_entries_to_evict(&self, v: bool) {
        if let Some(flag) = &self.more_entries_to_evict {
            flag.store(v, Ordering::Release);
        }
    }

    #[inline]
    fn should_apply(&self, ch_len: usize, ch_flush_point: usize, now: Instant) -> bool {
        self.auto_run_enabled.load(Ordering::Relaxed)
            && (ch_len >= ch_flush_point || now >= self.run_after.instant().unwrap())
    }

    pub(crate) fn run_pending_tasks<T: InnerSync>(&self, cache: &T) {
        let lock = self.run_lock.lock();
        self.do_run_pending_tasks(cache, lock);
    }

    pub(crate) fn try_run_pending_tasks<T: InnerSync>(&self, cache: &T) -> bool {
        if let Some(lock) = self.run_lock.try_lock() {
            self.do_run_pending_tasks(cache, lock);
            true
        } else {
            false
        }
    }

    fn do_run_pending_tasks<T: InnerSync>(&self, cache: &T, _lock: MutexGuard<'_, ()>) {
        let now = cache.now();
        self.run_after.set_instant(Self::sync_after(now));
        let timeout = self.maintenance_task_timeout;
        let more_to_evict = cache.run_pending_tasks(MAX_SYNC_REPEATS, timeout);
        self.set_more_entries_to_evict(more_to_evict);
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
