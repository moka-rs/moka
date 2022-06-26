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
    // NonBlocking(NonBlockingRemovalNotifier<K, V>),
    ThreadPool(ThreadPoolRemovalNotifier<K, V>),
}

impl<K, V> RemovalNotifier<K, V> {
    pub(crate) fn new(listener: EvictionListener<K, V>, conf: notification::Configuration) -> Self {
        match conf.delivery_mode() {
            DeliveryMode::Immediate => Self::Blocking(BlockingRemovalNotifier::new(listener)),
            DeliveryMode::Queued => Self::ThreadPool(ThreadPoolRemovalNotifier::new(listener)),
        }
    }

    pub(crate) fn is_blocking(&self) -> bool {
        matches!(self, RemovalNotifier::Blocking(_))
    }

    pub(crate) fn is_batching_supported(&self) -> bool {
        matches!(
            self,
            // RemovalNotifier::NonBlocking(_) | RemovalNotifier::ThreadPool(_)
            RemovalNotifier::ThreadPool(_)
        )
    }

    pub(crate) fn notify(&self, key: Arc<K>, value: V, cause: RemovalCause)
    where
        K: Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        match self {
            RemovalNotifier::Blocking(notifier) => notifier.notify(key, value, cause),
            // RemovalNotifier::NonBlocking(_) => todo!(),
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
            // RemovalNotifier::NonBlocking(_) => todo!(),
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
            // RemovalNotifier::NonBlocking(_) => todo!(),
            RemovalNotifier::ThreadPool(notifier) => notifier.submit_task(),
        }
    }
}

pub(crate) struct BlockingRemovalNotifier<K, V> {
    listener: EvictionListener<K, V>,
}

impl<K, V> BlockingRemovalNotifier<K, V> {
    fn new(listener: EvictionListener<K, V>) -> Self {
        Self { listener }
    }

    fn notify(&self, key: Arc<K>, value: V, cause: RemovalCause) {
        // use std::panic::{catch_unwind, AssertUnwindSafe};

        (self.listener)(key, value, cause);

        // let listener_clo = || listener(key, value, cause);
        // match catch_unwind(AssertUnwindSafe(listener_clo)) {
        //     Ok(_) => todo!(),
        //     Err(_) => todo!(),
        // }
    }
}

// pub(crate) struct NonBlockingRemovalNotifier<K, V> {
//     _phantom: std::marker::PhantomData<(K, V)>,
// }

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
    fn new(listener: EvictionListener<K, V>) -> Self {
        let (snd, rcv) = crossbeam_channel::bounded(CHANNEL_CAPACITY);
        let thread_pool = ThreadPoolRegistry::acquire_pool(PoolName::RemovalNotifier);
        let state = NotifierState {
            task_lock: Default::default(),
            rcv,
            listener,
            is_running: Default::default(),
            is_shutting_down: Default::default(),
        };
        Self {
            snd,
            state: Arc::new(state),
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

        if self.state.is_running() {
            return;
        }
        self.state.set_running(true);

        let task = NotificationTask::new(&self.state);
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
        let task_lock = self.state.task_lock.lock();
        let mut count = 0u16;

        while let Ok(entries) = self.state.rcv.try_recv() {
            match entries {
                RemovedEntries::Single(entry) => {
                    self.notify(&self.state.listener, entry);
                    count += 1;
                }
                RemovedEntries::Multi(entries) => {
                    for entry in entries {
                        self.notify(&self.state.listener, entry);
                        count += 1;

                        if self.state.is_shutting_down() {
                            break;
                        }
                    }
                }
            }

            if count > MAX_NOTIFICATIONS_PER_TASK || self.state.is_shutting_down() {
                break;
            }
        }

        std::mem::drop(task_lock);
        self.state.set_running(false);
    }

    fn notify(&self, listener: EvictionListenerRef<'_, K, V>, entry: RemovedEntry<K, V>) {
        // use std::panic::{catch_unwind, AssertUnwindSafe};

        let RemovedEntry { key, value, cause } = entry;
        listener(key, value, cause);

        // let listener_clo = || listener(key, value, cause);
        // match catch_unwind(AssertUnwindSafe(listener_clo)) {
        //     Ok(_) => todo!(),
        //     Err(_) => todo!(),
        // }
    }
}

struct NotifierState<K, V> {
    task_lock: Mutex<()>,
    rcv: Receiver<RemovedEntries<K, V>>,
    listener: EvictionListener<K, V>,
    is_running: AtomicBool,
    is_shutting_down: AtomicBool,
}

impl<K, V> NotifierState<K, V> {
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
