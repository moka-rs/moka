use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    common::concurrent::{
        constants::WRITE_RETRY_INTERVAL_MICROS,
        thread_pool::{PoolName, ThreadPool, ThreadPoolRegistry},
    },
    notification::{self, DeliveryMode, EvictionListener, EvictionListenerRef, RemovalCause},
};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;

const CHANNEL_CAPACITY: usize = 1_024;
const SUBMIT_TASK_THRESHOLD: usize = 100;
const MAX_NOTIFICATIONS_PER_TASK: u16 = 5_000;

pub(crate) enum RemovalNotifier<K, V> {
    Blocking(BlockingRemovalNotifier<K, V>),
    ThreadPool(ThreadPoolRemovalNotifier<K, V>),
}

impl<K, V> RemovalNotifier<K, V> {
    pub(crate) fn new(
        listener: EvictionListener<K, V>,
        conf: notification::Configuration,
        cache_name: Option<String>,
    ) -> Self {
        match conf.delivery_mode() {
            DeliveryMode::Immediate => {
                Self::Blocking(BlockingRemovalNotifier::new(listener, cache_name))
            }
            DeliveryMode::Queued => {
                Self::ThreadPool(ThreadPoolRemovalNotifier::new(listener, cache_name))
            }
        }
    }

    pub(crate) fn is_blocking(&self) -> bool {
        matches!(self, RemovalNotifier::Blocking(_))
    }

    pub(crate) fn is_batching_supported(&self) -> bool {
        matches!(self, RemovalNotifier::ThreadPool(_))
    }

    pub(crate) fn notify(&self, key: Arc<K>, value: V, cause: RemovalCause)
    where
        K: Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        match self {
            RemovalNotifier::Blocking(notifier) => notifier.notify(key, value, cause),
            RemovalNotifier::ThreadPool(notifier) => {
                notifier.add_single_notification(key, value, cause)
            }
        }
    }

    pub(crate) fn batch_notify(&self, entries: Vec<RemovedEntry<K, V>>)
    where
        K: Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        match self {
            RemovalNotifier::Blocking(_) => unreachable!(),
            RemovalNotifier::ThreadPool(notifier) => notifier.add_multiple_notifications(entries),
        }
    }

    pub(crate) fn sync(&self)
    where
        K: Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        match self {
            RemovalNotifier::Blocking(_) => unreachable!(),
            RemovalNotifier::ThreadPool(notifier) => notifier.submit_task(),
        }
    }
}

pub(crate) struct BlockingRemovalNotifier<K, V> {
    listener: EvictionListener<K, V>,
    is_enabled: AtomicBool,
    #[cfg(feature = "logging")]
    cache_name: Option<String>,
}

impl<K, V> BlockingRemovalNotifier<K, V> {
    fn new(listener: EvictionListener<K, V>, _cache_name: Option<String>) -> Self {
        Self {
            listener,
            is_enabled: AtomicBool::new(true),
            #[cfg(feature = "logging")]
            cache_name: _cache_name,
        }
    }

    fn notify(&self, key: Arc<K>, value: V, cause: RemovalCause) {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        if !self.is_enabled.load(Ordering::Acquire) {
            return;
        }

        let listener_clo = || (self.listener)(key, value, cause);

        // Safety: It is safe to assert unwind safety here because we will not
        // call the listener again if it has been panicked.
        let result = catch_unwind(AssertUnwindSafe(listener_clo));
        if let Err(_payload) = result {
            self.is_enabled.store(false, Ordering::Release);
            #[cfg(feature = "logging")]
            log_panic(&*_payload, self.cache_name.as_deref());
        }
    }
}

pub(crate) struct ThreadPoolRemovalNotifier<K, V> {
    snd: Sender<RemovedEntries<K, V>>,
    state: Arc<NotifierState<K, V>>,
    thread_pool: Arc<ThreadPool>,
}

impl<K, V> Drop for ThreadPoolRemovalNotifier<K, V> {
    fn drop(&mut self) {
        let state = &self.state;
        // Disallow to create and run a notification task by now.
        state.shutdown();

        // Wait for the notification task to finish. (busy loop)
        while state.is_running() {
            std::thread::sleep(Duration::from_millis(1));
        }

        ThreadPoolRegistry::release_pool(&self.thread_pool);
    }
}

impl<K, V> ThreadPoolRemovalNotifier<K, V> {
    fn new(listener: EvictionListener<K, V>, _cache_name: Option<String>) -> Self {
        let (snd, rcv) = crossbeam_channel::bounded(CHANNEL_CAPACITY);
        let thread_pool = ThreadPoolRegistry::acquire_pool(PoolName::RemovalNotifier);
        let state = NotifierState {
            task_lock: Default::default(),
            rcv,
            listener,
            #[cfg(feature = "logging")]
            cache_name: _cache_name,
            is_enabled: AtomicBool::new(true),
            is_running: Default::default(),
            is_shutting_down: Default::default(),
        };

        #[cfg_attr(beta_clippy, allow(clippy::arc_with_non_send_sync))]
        let state = Arc::new(state);

        Self {
            snd,
            state,
            thread_pool,
        }
    }
}

impl<K, V> ThreadPoolRemovalNotifier<K, V>
where
    K: Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    fn add_single_notification(&self, key: Arc<K>, value: V, cause: RemovalCause) {
        let entry = RemovedEntries::new_single(key, value, cause);
        self.send_entries(entry)
            .expect("Failed to send notification");
    }

    fn add_multiple_notifications(&self, entries: Vec<RemovedEntry<K, V>>) {
        let entries = RemovedEntries::new_multi(entries);
        self.send_entries(entries)
            .expect("Failed to send notification");
    }

    fn send_entries(
        &self,
        entries: RemovedEntries<K, V>,
    ) -> Result<(), TrySendError<RemovedEntries<K, V>>> {
        let mut entries = entries;
        loop {
            self.submit_task_if_necessary();
            match self.snd.try_send(entries) {
                Ok(()) => break,
                Err(TrySendError::Full(entries1)) => {
                    entries = entries1;
                    std::thread::sleep(Duration::from_millis(WRITE_RETRY_INTERVAL_MICROS));
                }
                Err(e @ TrySendError::Disconnected(_)) => return Err(e),
            }
        }
        Ok(())
    }

    fn submit_task(&self) {
        // TODO: Use compare and exchange to ensure it was false.

        let state = &self.state;

        if state.is_running() || !state.is_enabled() || state.is_shutting_down() {
            return;
        }
        state.set_running(true);

        let task = NotificationTask::new(state);
        self.thread_pool.pool.execute(move || {
            task.execute();
        });
    }

    fn submit_task_if_necessary(&self) {
        if self.snd.len() >= SUBMIT_TASK_THRESHOLD && !self.state.is_running() {
            self.submit_task(); // TODO: Error handling?
        }
    }
}

struct NotificationTask<K, V> {
    state: Arc<NotifierState<K, V>>,
}

impl<K, V> NotificationTask<K, V> {
    fn new(state: &Arc<NotifierState<K, V>>) -> Self {
        Self {
            state: Arc::clone(state),
        }
    }

    fn execute(&self) {
        // Only one task can be executed at a time for a cache segment.
        let task_lock = self.state.task_lock.lock();
        let mut count = 0u16;
        let mut is_enabled = self.state.is_enabled();

        if !is_enabled {
            return;
        }

        while let Ok(entries) = self.state.rcv.try_recv() {
            match entries {
                RemovedEntries::Single(entry) => {
                    let result = self.notify(&self.state.listener, entry);
                    if result.is_err() {
                        is_enabled = false;
                        break;
                    }
                    count += 1;
                }
                RemovedEntries::Multi(entries) => {
                    for entry in entries {
                        let result = self.notify(&self.state.listener, entry);
                        if result.is_err() {
                            is_enabled = false;
                            break;
                        }
                        if self.state.is_shutting_down() {
                            break;
                        }
                        count += 1;
                    }
                }
            }

            if count > MAX_NOTIFICATIONS_PER_TASK || self.state.is_shutting_down() {
                break;
            }
        }

        if !is_enabled {
            self.state.set_enabled(false);
        }

        std::mem::drop(task_lock);
        self.state.set_running(false);
    }

    /// Returns `Ok(())` when calling the listener succeeded. Returns
    /// `Err(panic_payload)` when the listener panicked.
    fn notify(
        &self,
        listener: EvictionListenerRef<'_, K, V>,
        entry: RemovedEntry<K, V>,
    ) -> Result<(), Box<dyn std::any::Any + Send>> {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let RemovedEntry { key, value, cause } = entry;
        let listener_clo = || (listener)(key, value, cause);

        // Safety: It is safe to assert unwind safety here because we will not
        // call the listener again if it has been panicked.
        //
        #[allow(clippy::let_and_return)]
        // https://rust-lang.github.io/rust-clippy/master/index.html#let_and_return
        let result = catch_unwind(AssertUnwindSafe(listener_clo));
        #[cfg(feature = "logging")]
        {
            if let Err(payload) = &result {
                log_panic(&**payload, self.state.cache_name.as_deref());
            }
        }
        result
    }
}

struct NotifierState<K, V> {
    task_lock: Mutex<()>,
    rcv: Receiver<RemovedEntries<K, V>>,
    listener: EvictionListener<K, V>,
    #[cfg(feature = "logging")]
    cache_name: Option<String>,
    is_enabled: AtomicBool,
    is_running: AtomicBool,
    is_shutting_down: AtomicBool,
}

impl<K, V> NotifierState<K, V> {
    fn is_enabled(&self) -> bool {
        self.is_enabled.load(Ordering::Acquire)
    }

    fn set_enabled(&self, value: bool) {
        self.is_enabled.store(value, Ordering::Release);
    }

    fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Acquire)
    }

    fn set_running(&self, value: bool) {
        self.is_running.store(value, Ordering::Release);
    }

    fn is_shutting_down(&self) -> bool {
        self.is_shutting_down.load(Ordering::Acquire)
    }

    fn shutdown(&self) {
        self.is_shutting_down.store(true, Ordering::Release);
    }
}

pub(crate) struct RemovedEntry<K, V> {
    key: Arc<K>,
    value: V,
    cause: RemovalCause,
}

impl<K, V> RemovedEntry<K, V> {
    pub(crate) fn new(key: Arc<K>, value: V, cause: RemovalCause) -> Self {
        Self { key, value, cause }
    }
}

enum RemovedEntries<K, V> {
    Single(RemovedEntry<K, V>),
    Multi(Vec<RemovedEntry<K, V>>),
}

impl<K, V> RemovedEntries<K, V> {
    fn new_single(key: Arc<K>, value: V, cause: RemovalCause) -> Self {
        Self::Single(RemovedEntry::new(key, value, cause))
    }

    fn new_multi(entries: Vec<RemovedEntry<K, V>>) -> Self {
        Self::Multi(entries)
    }
}

#[cfg(feature = "logging")]
fn log_panic(payload: &(dyn std::any::Any + Send + 'static), cache_name: Option<&str>) {
    // Try to downcast the payload into &str or String.
    //
    // NOTE: Clippy will complain if we use `if let Some(_)` here.
    // https://rust-lang.github.io/rust-clippy/master/index.html#manual_map
    let message: Option<std::borrow::Cow<'_, str>> =
        (payload.downcast_ref::<&str>().map(|s| (*s).into()))
            .or_else(|| payload.downcast_ref::<String>().map(Into::into));

    let cn = cache_name
        .map(|name| format!("[{}] ", name))
        .unwrap_or_default();

    if let Some(m) = message {
        log::error!(
            "{}Disabled the eviction listener because it panicked at '{}'",
            cn,
            m
        );
    } else {
        log::error!("{}Disabled the eviction listener because it panicked", cn);
    }
}
