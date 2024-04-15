use crate::common::{
    concurrent::{
        atomic_time::AtomicInstant,
        constants::{LOG_SYNC_INTERVAL_MILLIS, READ_LOG_FLUSH_POINT, WRITE_LOG_FLUSH_POINT},
    },
    time::{CheckedTimeOps, Instant},
    HousekeeperConfig,
};

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use async_lock::Mutex;
use async_trait::async_trait;
use futures_util::future::{BoxFuture, Shared};

#[async_trait]
pub(crate) trait InnerSync {
    /// Runs the pending tasks. Returns `true` if there are more entries to evict in
    /// next run.
    async fn run_pending_tasks(
        &self,
        timeout: Option<Duration>,
        max_log_sync_repeats: u32,
        eviction_batch_size: u32,
    ) -> bool;

    /// Notifies all the async tasks waiting in `BaseCache::schedule_write_op` method
    /// for the write op channel to have enough room.
    fn notify_write_op_ch_is_ready(&self);

    fn now(&self) -> Instant;
}

pub(crate) struct Housekeeper {
    /// A shared `Future` of the maintenance task that is currently being resolved.
    current_task: Mutex<Option<Shared<BoxFuture<'static, bool>>>>,
    run_after: AtomicInstant,
    /// A flag to indicate if the last call on `run_pending_tasks` method left some
    /// entries to evict.
    ///
    /// Used only when the eviction listener closure is set for this cache instance
    /// because, if not, `run_pending_tasks` will never leave entries to evict.
    more_entries_to_evict: Option<AtomicBool>,
    /// The timeout duration for the `run_pending_tasks` method. This is a safe-guard
    /// to prevent cache read/write operations (that may call `run_pending_tasks`
    /// internally) from being blocked for a long time when the user wrote a slow
    /// eviction listener closure.
    ///
    /// Used only when the eviction listener closure is set for this cache instance.
    maintenance_task_timeout: Option<Duration>,
    /// The maximum repeat count for receiving operation logs from the read and write
    /// log channels. Default: `MAX_LOG_SYNC_REPEATS`.
    max_log_sync_repeats: u32,
    /// The batch size of entries to be processed by each internal eviction method.
    /// Default: `EVICTION_BATCH_SIZE`.
    eviction_batch_size: u32,
    auto_run_enabled: AtomicBool,
    #[cfg(test)]
    pub(crate) start_count: AtomicUsize,
    #[cfg(test)]
    pub(crate) complete_count: AtomicUsize,
}

impl Housekeeper {
    pub(crate) fn new(is_eviction_listener_enabled: bool, config: HousekeeperConfig) -> Self {
        let (more_entries_to_evict, maintenance_task_timeout) = if is_eviction_listener_enabled {
            (
                Some(AtomicBool::new(false)),
                Some(config.maintenance_task_timeout),
            )
        } else {
            (None, None)
        };

        Self {
            current_task: Mutex::default(),
            run_after: AtomicInstant::new(Self::sync_after(Instant::now())),
            more_entries_to_evict,
            maintenance_task_timeout,
            max_log_sync_repeats: config.max_log_sync_repeats,
            eviction_batch_size: config.eviction_batch_size,
            auto_run_enabled: AtomicBool::new(true),
            #[cfg(test)]
            start_count: Default::default(),
            #[cfg(test)]
            complete_count: Default::default(),
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

    pub(crate) async fn run_pending_tasks<T>(&self, cache: Arc<T>)
    where
        T: InnerSync + Send + Sync + 'static,
    {
        let mut current_task = self.current_task.lock().await;
        self.do_run_pending_tasks(Arc::clone(&cache), &mut current_task)
            .await;

        drop(current_task);

        // If there are any async tasks waiting in `BaseCache::schedule_write_op`
        // method for the write op channel, notify them.
        cache.notify_write_op_ch_is_ready();
    }

    /// Tries to run the pending tasks if the lock is free. Returns `true` if there
    /// are more entries to evict in next run.
    pub(crate) async fn try_run_pending_tasks<T>(&self, cache: &Arc<T>) -> bool
    where
        T: InnerSync + Send + Sync + 'static,
    {
        if let Some(mut current_task) = self.current_task.try_lock() {
            self.do_run_pending_tasks(Arc::clone(cache), &mut current_task)
                .await;
        } else {
            return false;
        }

        // The `current_task` lock should be free now.

        // If there are any async tasks waiting in `BaseCache::schedule_write_op`
        // method for the write op channel, notify them.
        cache.notify_write_op_ch_is_ready();
        true
    }

    async fn do_run_pending_tasks<T>(
        &self,
        cache: Arc<T>,
        current_task: &mut Option<Shared<BoxFuture<'static, bool>>>,
    ) where
        T: InnerSync + Send + Sync + 'static,
    {
        use futures_util::FutureExt;

        let now = cache.now();
        let more_to_evict;
        // Async Cancellation Safety: Our maintenance task is cancellable as we save
        // it in the lock. If it is canceled, we will resume it in the next run.

        if let Some(task) = &*current_task {
            // This task was cancelled in the previous run due to the enclosing
            // Future was dropped. Resume the task now by awaiting.
            more_to_evict = task.clone().await;
        } else {
            let timeout = self.maintenance_task_timeout;
            let repeats = self.max_log_sync_repeats;
            let batch_size = self.eviction_batch_size;
            // Create a new maintenance task and await it.
            let task = async move { cache.run_pending_tasks(timeout, repeats, batch_size).await }
                .boxed()
                .shared();
            *current_task = Some(task.clone());

            #[cfg(test)]
            self.start_count.fetch_add(1, Ordering::AcqRel);

            more_to_evict = task.await;
        }

        // If we are here, it means that the maintenance task has been completed.
        // We can remove it from the lock.
        *current_task = None;
        self.run_after.set_instant(Self::sync_after(now));
        self.set_more_entries_to_evict(more_to_evict);

        #[cfg(test)]
        self.complete_count.fetch_add(1, Ordering::AcqRel);
    }

    fn sync_after(now: Instant) -> Instant {
        let dur = Duration::from_millis(LOG_SYNC_INTERVAL_MILLIS);
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
