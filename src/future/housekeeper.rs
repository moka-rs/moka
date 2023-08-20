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

use std::{sync::Arc, time::Duration};

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
}

impl Default for Housekeeper {
    fn default() -> Self {
        Self {
            current_task: Default::default(),
            run_after: AtomicInstant::new(Self::sync_after(Instant::now())),
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
        ch_len >= ch_flush_point || self.run_after.instant().unwrap() >= now
    }

    // TODO: Change the name to something that shows some kind of relationship with
    // `run_pending_tasks`.
    pub(crate) async fn try_sync<T>(&self, cache: Arc<T>) -> bool
    where
        T: InnerSync + Send + Sync + 'static,
    {
        use futures_util::FutureExt;

        // TODO: This will skip to run pending tasks if lock cannot be acquired.
        // Change this so that when `try_sync` is explicitly called, it will be
        // blocked here until the lock is acquired.
        if let Some(mut lock) = self.current_task.try_lock() {
            let now = cache.now();

            if let Some(task) = &*lock {
                // This task was being resolved, but did not complete. This means
                // that the enclosing Future was canceled. Try to resolve it.
                task.clone().await;
            } else {
                // Create a new maintenance task and try to resolve it.
                let task = async move { cache.run_pending_tasks(MAX_SYNC_REPEATS).await }
                    .boxed()
                    .shared();
                *lock = Some(task.clone());
                task.await;
            }

            // If we are here, it means that the maintenance task has been completed,
            // so we can remove it from the lock.
            *lock = None;
            self.run_after.set_instant(Self::sync_after(now));

            true
        } else {
            false
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
