use super::constants::{
    MAX_SYNC_REPEATS, PERIODICAL_SYNC_FAST_PACE_NANOS, PERIODICAL_SYNC_INITIAL_DELAY_MILLIS,
    PERIODICAL_SYNC_NORMAL_PACE_MILLIS,
};
use super::{
    thread_pool::{ThreadPool, ThreadPoolRegistry},
    unsafe_weak_pointer::UnsafeWeakPointer,
};

use super::{
    atomic_time::AtomicInstant,
    constants::{READ_LOG_FLUSH_POINT, WRITE_LOG_FLUSH_POINT},
};
use crate::common::time::{CheckedTimeOps, Instant};
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

pub(crate) trait InnerSync {
    fn sync(&self, max_sync_repeats: usize) -> Option<SyncPace>;

    fn now(&self) -> Instant;
}

#[derive(Clone, Debug)]
pub(crate) struct Configuration {
    is_blocking: bool,
    periodical_sync_enabled: bool,
}

impl Configuration {
    pub(crate) fn new_blocking() -> Self {
        Self {
            is_blocking: true,
            periodical_sync_enabled: false,
        }
    }

    pub(crate) fn new_thread_pool(periodical_sync_enabled: bool) -> Self {
        Self {
            is_blocking: false,
            periodical_sync_enabled,
        }
    }
}

pub(crate) enum Housekeeper<T> {
    Blocking(BlockingHousekeeper),
    ThreadPool(ThreadPoolHousekeeper<T>),
}

impl<T> Housekeeper<T>
where
    T: InnerSync + 'static,
{
    pub(crate) fn new(inner: Weak<T>, config: Configuration) -> Self {
        if config.is_blocking {
            Housekeeper::Blocking(BlockingHousekeeper::default())
        } else {
            Housekeeper::ThreadPool(ThreadPoolHousekeeper::new(
                inner,
                config.periodical_sync_enabled,
            ))
        }
    }

    pub(crate) fn should_apply_reads(&self, ch_len: usize, now: Instant) -> bool {
        match self {
            Housekeeper::Blocking(h) => h.should_apply_reads(ch_len, now),
            Housekeeper::ThreadPool(h) => h.should_apply_reads(ch_len, now),
        }
    }

    pub(crate) fn should_apply_writes(&self, ch_len: usize, now: Instant) -> bool {
        match self {
            Housekeeper::Blocking(h) => h.should_apply_writes(ch_len, now),
            Housekeeper::ThreadPool(h) => h.should_apply_writes(ch_len, now),
        }
    }

    pub(crate) fn try_sync(&self, cache: &impl InnerSync) -> bool {
        match self {
            Housekeeper::Blocking(h) => h.try_sync(cache),
            Housekeeper::ThreadPool(h) => h.try_schedule_sync(),
        }
    }

    #[cfg(test)]
    pub(crate) fn stop_periodical_sync_job(&self) {
        match self {
            Housekeeper::Blocking(_) => (),
            Housekeeper::ThreadPool(h) => h.stop_periodical_sync_job(),
        }
    }
}

pub(crate) struct BlockingHousekeeper {
    is_sync_running: AtomicBool,
    sync_after: AtomicInstant,
}

impl Default for BlockingHousekeeper {
    fn default() -> Self {
        Self {
            is_sync_running: Default::default(),
            sync_after: AtomicInstant::new(Self::sync_after(Instant::now())),
        }
    }
}

impl BlockingHousekeeper {
    fn should_apply_reads(&self, ch_len: usize, now: Instant) -> bool {
        self.should_apply(ch_len, READ_LOG_FLUSH_POINT / 8, now)
    }

    fn should_apply_writes(&self, ch_len: usize, now: Instant) -> bool {
        self.should_apply(ch_len, WRITE_LOG_FLUSH_POINT / 8, now)
    }

    #[inline]
    fn should_apply(&self, ch_len: usize, ch_flush_point: usize, now: Instant) -> bool {
        ch_len >= ch_flush_point || self.sync_after.instant().unwrap() >= now
    }

    fn try_sync<T: InnerSync>(&self, cache: &T) -> bool {
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

                cache.sync(MAX_SYNC_REPEATS);

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

pub(crate) struct ThreadPoolHousekeeper<T> {
    inner: Arc<Mutex<UnsafeWeakPointer<T>>>,
    thread_pool: Arc<ThreadPool>,
    is_shutting_down: Arc<AtomicBool>,
    periodical_sync_job: Mutex<Option<JobHandle>>,
    periodical_sync_running: Arc<Mutex<()>>,
    on_demand_sync_scheduled: Arc<AtomicBool>,
    _marker: PhantomData<T>,
}

impl<T> Drop for ThreadPoolHousekeeper<T> {
    fn drop(&mut self) {
        // Disallow to create and/or run sync jobs by now.
        self.is_shutting_down.store(true, Ordering::Release);

        // Cancel the periodical sync job. (This will not abort the job if it is
        // already running)
        if let Some(j) = self.periodical_sync_job.lock().take() {
            j.cancel()
        }

        // Wait for the periodical sync job to finish.
        //
        // NOTE: As suggested by Clippy 1.59, drop the lock explicitly rather
        // than doing non-binding let to `_`.
        // https://rust-lang.github.io/rust-clippy/master/index.html#let_underscore_lock
        std::mem::drop(self.periodical_sync_running.lock());

        // Wait for the on-demand sync job to finish. (busy loop)
        while self.on_demand_sync_scheduled.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }

        // All sync jobs should have been finished by now. Clean other stuff up.
        ThreadPoolRegistry::release_pool(&self.thread_pool);
        std::mem::drop(unsafe { self.inner.lock().as_weak_arc() });
    }
}

// functions/methods used by Cache
impl<T> ThreadPoolHousekeeper<T>
where
    T: InnerSync + 'static,
{
    fn new(inner: Weak<T>, periodical_sync_enable: bool) -> Self {
        use super::thread_pool::PoolName;

        let thread_pool = ThreadPoolRegistry::acquire_pool(PoolName::Housekeeper);
        let inner_ptr = Arc::new(Mutex::new(UnsafeWeakPointer::from_weak_arc(inner)));
        let is_shutting_down = Arc::new(AtomicBool::new(false));
        let periodical_sync_running = Arc::new(Mutex::new(()));

        let maybe_sync_job = if periodical_sync_enable {
            Some(Self::start_periodical_sync_job(
                &thread_pool,
                Arc::clone(&inner_ptr),
                Arc::clone(&is_shutting_down),
                Arc::clone(&periodical_sync_running),
            ))
        } else {
            None
        };

        Self {
            inner: inner_ptr,
            thread_pool,
            is_shutting_down,
            periodical_sync_job: Mutex::new(maybe_sync_job),
            periodical_sync_running,
            on_demand_sync_scheduled: Arc::new(AtomicBool::new(false)),
            _marker: PhantomData,
        }
    }

    fn start_periodical_sync_job(
        thread_pool: &Arc<ThreadPool>,
        unsafe_weak_ptr: Arc<Mutex<UnsafeWeakPointer<T>>>,
        is_shutting_down: Arc<AtomicBool>,
        periodical_sync_running: Arc<Mutex<()>>,
    ) -> JobHandle {
        let mut sync_pace = SyncPace::Normal;

        let housekeeper_closure = {
            move || {
                if !is_shutting_down.load(Ordering::Acquire) {
                    let _lock = periodical_sync_running.lock();
                    if let Some(new_pace) = Self::call_sync(&unsafe_weak_ptr) {
                        if sync_pace != new_pace {
                            sync_pace = new_pace
                        }
                    }
                }

                Some(sync_pace.make_duration())
            }
        };

        let initial_delay = Duration::from_millis(PERIODICAL_SYNC_INITIAL_DELAY_MILLIS);

        // Execute a task in a worker thread.
        thread_pool
            .pool
            .execute_with_dynamic_delay(initial_delay, housekeeper_closure)
    }

    fn should_apply_reads(&self, ch_len: usize, _now: Instant) -> bool {
        ch_len >= READ_LOG_FLUSH_POINT
    }

    fn should_apply_writes(&self, ch_len: usize, _now: Instant) -> bool {
        ch_len >= WRITE_LOG_FLUSH_POINT
    }

    fn try_schedule_sync(&self) -> bool {
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
    pub(crate) fn stop_periodical_sync_job(&self) {
        if let Some(j) = self.periodical_sync_job.lock().take() {
            j.cancel();
        }
    }
}

impl<T: InnerSync> ThreadPoolHousekeeper<T> {
    fn call_sync(unsafe_weak_ptr: &Arc<Mutex<UnsafeWeakPointer<T>>>) -> Option<SyncPace> {
        let lock = unsafe_weak_ptr.lock();
        // Restore the Weak pointer to Inner<K, V, S>.
        let weak = unsafe { lock.as_weak_arc() };
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
