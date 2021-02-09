use super::{
    thread_pool::{ThreadPool, ThreadPoolRegistry},
    unsafe_weak_pointer::UnsafeWeakPointer,
};
use crate::common::base_cache::{
    MAX_SYNC_REPEATS, PERIODICAL_SYNC_FAST_PACE_NANOS, PERIODICAL_SYNC_INITIAL_DELAY_MILLIS,
    PERIODICAL_SYNC_NORMAL_PACE_MILLIS,
};

use parking_lot::Mutex;
use scheduled_thread_pool::JobHandle;
use std::{
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::Duration,
};

#[derive(PartialEq, Eq)]
pub(crate) enum SyncPace {
    Normal,
    Fast,
}

impl SyncPace {
    fn make_duration(&self) -> Duration {
        use SyncPace::*;
        match self {
            Normal => Duration::from_millis(PERIODICAL_SYNC_NORMAL_PACE_MILLIS),
            Fast => Duration::from_nanos(PERIODICAL_SYNC_FAST_PACE_NANOS),
        }
    }
}

pub(crate) trait InnerSync {
    fn sync(&self, max_sync_repeats: usize) -> Option<SyncPace>;
}

pub(crate) struct Housekeeper<T> {
    inner: Arc<Mutex<UnsafeWeakPointer>>,
    thread_pool: Arc<ThreadPool>,
    is_shutting_down: Arc<AtomicBool>,
    periodical_sync_job: Mutex<Option<JobHandle>>,
    periodical_sync_running: Arc<Mutex<()>>,
    on_demand_sync_scheduled: Arc<AtomicBool>,
    _marker: PhantomData<T>,
}

impl<T> Drop for Housekeeper<T> {
    fn drop(&mut self) {
        // Disallow to create and/or run sync jobs by now.
        self.is_shutting_down.store(true, Ordering::Release);

        // Cancel the periodical sync job. (This will not abort the job if it is
        // already running)
        if let Some(j) = self.periodical_sync_job.lock().take() {
            j.cancel()
        }

        // Wait for the periodical sync job to finish.
        let _ = self.periodical_sync_running.lock();

        // Wait for the on-demand sync job to finish. (busy loop)
        while self.on_demand_sync_scheduled.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }

        // All sync jobs should have been finished by now. Clean other stuff up.
        ThreadPoolRegistry::release_pool(&self.thread_pool);
        std::mem::drop(unsafe { self.inner.lock().as_weak_arc::<T>() });
    }
}

// functions/methods used by Cache
impl<T: InnerSync> Housekeeper<T> {
    pub(crate) fn new(inner: Weak<T>) -> Self {
        let thread_pool = ThreadPoolRegistry::acquire_default_pool();

        let inner_ptr = Arc::new(Mutex::new(UnsafeWeakPointer::from_weak_arc(inner)));
        let is_shutting_down = Arc::new(AtomicBool::new(false));
        let periodical_sync_running = Arc::new(Mutex::new(()));
        let periodical_sync_pace = Arc::new(Mutex::new(SyncPace::Normal));

        let housekeeper_closure = {
            // The following Arc clones will be moved into the housekeeper closure.
            let unsafe_weak_ptr = Arc::clone(&inner_ptr);
            let shutting_down = Arc::clone(&is_shutting_down);
            let sync_running = Arc::clone(&periodical_sync_running);
            let sync_pace = Arc::clone(&periodical_sync_pace);

            move || {
                // To avoid dead-locking with other thread, acquire the lock at very beginning.
                let mut sync_pace = sync_pace.lock();
                if !shutting_down.load(Ordering::Acquire) {
                    let _lock = sync_running.lock();
                    if let Some(new_pace) = Self::call_sync(&unsafe_weak_ptr) {
                        if *sync_pace != new_pace {
                            *sync_pace = new_pace
                        }
                    }
                }
                // TODO: Check what would happen if the closure returns None.
                Some(sync_pace.make_duration())
            }
        };

        let initial_delay = Duration::from_millis(PERIODICAL_SYNC_INITIAL_DELAY_MILLIS);

        // Execute a task in a worker thread.
        let job = thread_pool
            .pool
            .execute_with_dynamic_delay(initial_delay, housekeeper_closure);

        Self {
            inner: inner_ptr,
            thread_pool,
            is_shutting_down,
            periodical_sync_job: Mutex::new(Some(job)),
            periodical_sync_running,
            on_demand_sync_scheduled: Arc::new(AtomicBool::new(false)),
            _marker: PhantomData::default(),
        }
    }

    pub(crate) fn try_schedule_sync(&self) -> bool {
        // TODO: Check if these `Orderings` are correct.

        // If shutting down, do not schedule the task.
        if self.is_shutting_down.load(Ordering::Acquire) {
            return false;
        }

        // Try to flip the value of sync_scheduled from false to true.
        match self.on_demand_sync_scheduled.compare_exchange(
            false,
            true,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                let unsafe_weak_ptr = Arc::clone(&self.inner);
                let sync_scheduled = Arc::clone(&self.on_demand_sync_scheduled);
                // Execute a task in a worker thread.
                self.thread_pool.pool.execute(move || {
                    Self::call_sync(&unsafe_weak_ptr);
                    sync_scheduled.store(false, Ordering::Release);
                });
                true
            }
            Err(_) => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn periodical_sync_job(&self) -> &Mutex<Option<JobHandle>> {
        &self.periodical_sync_job
    }
}

// private functions/methods
impl<T: InnerSync> Housekeeper<T> {
    fn call_sync(unsafe_weak_ptr: &Arc<Mutex<UnsafeWeakPointer>>) -> Option<SyncPace> {
        let lock = unsafe_weak_ptr.lock();
        // Restore the Weak pointer to Inner<K, V, S>.
        let weak = unsafe { lock.as_weak_arc::<T>() };
        if let Some(inner) = weak.upgrade() {
            // TODO: Protect this call with catch_unwind().
            let sync_pace = inner.sync(MAX_SYNC_REPEATS);
            // Avoid to drop the Arc<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_arc(inner);
            sync_pace
        } else {
            // Avoid to drop the Weak<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_weak_arc(weak);
            None
        }
    }
}
