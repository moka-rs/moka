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
    async fn run_pending_tasks(&self, max_sync_repeats: usize);
    fn now(&self) -> Instant;
}

pub(crate) struct Housekeeper {
    /// A shared `Future` of the maintenance task that is currently being resolved.
    current_task: Mutex<Option<Shared<BoxFuture<'static, ()>>>>,
    run_after: AtomicInstant,
    auto_run_enabled: AtomicBool,
    #[cfg(test)]
    pub(crate) start_count: AtomicUsize,
    #[cfg(test)]
    pub(crate) complete_count: AtomicUsize,
}

impl Default for Housekeeper {
    fn default() -> Self {
        Self {
            current_task: Mutex::default(),
            run_after: AtomicInstant::new(Self::sync_after(Instant::now())),
            auto_run_enabled: AtomicBool::new(true),
            #[cfg(test)]
            start_count: Default::default(),
            #[cfg(test)]
            complete_count: Default::default(),
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

    pub(crate) async fn run_pending_tasks<T>(&self, cache: Arc<T>)
    where
        T: InnerSync + Send + Sync + 'static,
    {
        let mut current_task = self.current_task.lock().await;
        self.do_run_pending_tasks(cache, &mut current_task).await;
    }

    pub(crate) async fn try_run_pending_tasks<T>(&self, cache: Arc<T>) -> bool
    where
        T: InnerSync + Send + Sync + 'static,
    {
        if let Some(mut current_task) = self.current_task.try_lock() {
            self.do_run_pending_tasks(cache, &mut current_task).await;
            true
        } else {
            false
        }
    }

    async fn do_run_pending_tasks<T>(
        &self,
        cache: Arc<T>,
        current_task: &mut Option<Shared<BoxFuture<'static, ()>>>,
    ) where
        T: InnerSync + Send + Sync + 'static,
    {
        use futures_util::FutureExt;

        let now = cache.now();

        // Async Cancellation Safety: Our maintenance task is cancellable as we save
        // it in the lock. If it is canceled, we will resume it in the next run.

        if let Some(task) = &*current_task {
            // This task was cancelled in the previous run due to the enclosing
            // Future was dropped. Resume the task now by awaiting.
            task.clone().await;
        } else {
            // Create a new maintenance task and await it.
            let task = async move { cache.run_pending_tasks(MAX_SYNC_REPEATS).await }
                .boxed()
                .shared();
            *current_task = Some(task.clone());

            #[cfg(test)]
            self.start_count.fetch_add(1, Ordering::AcqRel);

            task.await;
        }

        // If we are here, it means that the maintenance task has been completed.
        // We can remove it from the lock.
        *current_task = None;
        self.run_after.set_instant(Self::sync_after(now));

        #[cfg(test)]
        self.complete_count.fetch_add(1, Ordering::AcqRel);
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
