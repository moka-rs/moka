use async_lock::RwLock;
use std::{
    any::{Any, TypeId},
    future::Future,
    hash::{BuildHasher, Hash},
    sync::Arc,
};

type ErrorObject = Arc<dyn Any + Send + Sync + 'static>;

pub(crate) enum InitResult<V, E> {
    Initialized(V),
    ReadExisting(V),
    InitErr(Arc<E>),
}

enum WaiterValue<V> {
    Computing,
    Ready(Result<V, ErrorObject>),
    // https://github.com/moka-rs/moka/issues/43
    InitFuturePanicked,
    // https://github.com/moka-rs/moka/issues/59
    EnclosingFutureAborted,
}

type Waiter<V> = Arc<RwLock<WaiterValue<V>>>;

struct WaiterGuard<'a, K, V, S>
// NOTE: We usually do not attach trait bounds to here at the struct definition, but
// the Drop trait requires these bounds here.
where
    Arc<K>: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    is_waiter_value_set: bool,
    key: &'a Arc<K>,
    type_id: TypeId,
    value_initializer: &'a ValueInitializer<K, V, S>,
    write_lock: &'a mut WaiterValue<V>,
}

impl<'a, K, V, S> WaiterGuard<'a, K, V, S>
where
    Arc<K>: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    fn new(
        key: &'a Arc<K>,
        type_id: TypeId,
        value_initializer: &'a ValueInitializer<K, V, S>,
        write_lock: &'a mut WaiterValue<V>,
    ) -> Self {
        Self {
            is_waiter_value_set: false,
            key,
            type_id,
            value_initializer,
            write_lock,
        }
    }

    fn set_waiter_value(&mut self, v: WaiterValue<V>) {
        *self.write_lock = v;
        self.is_waiter_value_set = true;
    }
}

impl<'a, K, V, S> Drop for WaiterGuard<'a, K, V, S>
where
    Arc<K>: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    fn drop(&mut self) {
        if !self.is_waiter_value_set {
            // Value is not set. This means the future containing
            // `get_or_*_insert_with` has been aborted. Remove our waiter to prevent
            // the issue described in https://github.com/moka-rs/moka/issues/59
            *self.write_lock = WaiterValue::EnclosingFutureAborted;
            self.value_initializer.remove_waiter(self.key, self.type_id);
            self.is_waiter_value_set = true;
        }
    }
}

pub(crate) struct ValueInitializer<K, V, S> {
    // TypeId is the type ID of the concrete error type of generic type E in
    // try_init_or_read(). We use the type ID as a part of the key to ensure that
    // we can always downcast the trait object ErrorObject (in Waiter<V>) into
    // its concrete type.
    waiters: crate::cht::SegmentedHashMap<(Arc<K>, TypeId), Waiter<V>, S>,
}

impl<K, V, S> ValueInitializer<K, V, S>
where
    Arc<K>: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    pub(crate) fn with_hasher(hasher: S) -> Self {
        Self {
            waiters: crate::cht::SegmentedHashMap::with_num_segments_and_hasher(16, hasher),
        }
    }

    /// # Panics
    /// Panics if the `init` future has been panicked.
    pub(crate) async fn init_or_read<F>(&self, key: Arc<K>, init: F) -> InitResult<V, ()>
    where
        F: Future<Output = V>,
    {
        // This closure will be called after the init closure has returned a value.
        // It will convert the returned value (from init) into an InitResult.
        let post_init = |_key, value: V, mut guard: WaiterGuard<'_, K, V, S>| {
            guard.set_waiter_value(WaiterValue::Ready(Ok(value.clone())));
            InitResult::Initialized(value)
        };

        let type_id = TypeId::of::<()>();
        self.do_try_init(&key, type_id, init, post_init).await
    }

    /// # Panics
    /// Panics if the `init` future has been panicked.
    pub(crate) async fn try_init_or_read<F, E>(&self, key: Arc<K>, init: F) -> InitResult<V, E>
    where
        F: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        let type_id = TypeId::of::<E>();

        // This closure will be called after the init closure has returned a value.
        // It will convert the returned value (from init) into an InitResult.
        let post_init = |key, value: Result<V, E>, mut guard: WaiterGuard<'_, K, V, S>| match value
        {
            Ok(value) => {
                guard.set_waiter_value(WaiterValue::Ready(Ok(value.clone())));
                InitResult::Initialized(value)
            }
            Err(e) => {
                let err: ErrorObject = Arc::new(e);
                guard.set_waiter_value(WaiterValue::Ready(Err(Arc::clone(&err))));
                self.remove_waiter(key, type_id);
                InitResult::InitErr(err.downcast().unwrap())
            }
        };

        self.do_try_init(&key, type_id, init, post_init).await
    }

    /// # Panics
    /// Panics if the `init` future has been panicked.
    async fn do_try_init<'a, F, O, C, E>(
        &self,
        key: &'a Arc<K>,
        type_id: TypeId,
        init: F,
        mut post_init: C,
    ) -> InitResult<V, E>
    where
        F: Future<Output = O>,
        C: FnMut(&'a Arc<K>, O, WaiterGuard<'_, K, V, S>) -> InitResult<V, E>,
        E: Send + Sync + 'static,
    {
        use futures_util::FutureExt;
        use std::panic::{resume_unwind, AssertUnwindSafe};
        use InitResult::*;

        const MAX_RETRIES: usize = 200;
        let mut retries = 0;

        loop {
            let waiter = Arc::new(RwLock::new(WaiterValue::Computing));
            let mut lock = waiter.write().await;

            match self.try_insert_waiter(key, type_id, &waiter) {
                None => {
                    // Our waiter was inserted. Let's resolve the init future.

                    // Create a guard. This will ensure to remove our waiter when the
                    // enclosing future has been aborted:
                    // https://github.com/moka-rs/moka/issues/59
                    let mut waiter_guard = WaiterGuard::new(key, type_id, self, &mut lock);

                    // Catching panic is safe here as we do not try to resolve the future again.
                    match AssertUnwindSafe(init).catch_unwind().await {
                        // Resolved.
                        Ok(value) => return post_init(key, value, waiter_guard),
                        // Panicked.
                        Err(payload) => {
                            waiter_guard.set_waiter_value(WaiterValue::InitFuturePanicked);
                            // Remove the waiter so that others can retry.
                            self.remove_waiter(key, type_id);
                            resume_unwind(payload);
                        } // The lock will be unlocked here.
                    }
                }
                Some(res) => {
                    // Somebody else's waiter already exists. Drop our write lock and wait
                    // for a read lock to become available.
                    std::mem::drop(lock);
                    match &*res.read().await {
                        WaiterValue::Ready(Ok(value)) => return ReadExisting(value.clone()),
                        WaiterValue::Ready(Err(e)) => {
                            return InitErr(Arc::clone(e).downcast().unwrap())
                        }
                        // Somebody else's init future has been panicked.
                        WaiterValue::InitFuturePanicked => {
                            retries += 1;
                            panic_if_retry_exhausted_for_panicking(retries, MAX_RETRIES);
                            // Retry from the beginning.
                            continue;
                        }
                        // Somebody else (a future containing `get_or_insert_with`/
                        // `get_or_try_insert_with`) has been aborted.
                        WaiterValue::EnclosingFutureAborted => {
                            retries += 1;
                            panic_if_retry_exhausted_for_aborting(retries, MAX_RETRIES);
                            // Retry from the beginning.
                            continue;
                        }
                        // Unexpected state.
                        WaiterValue::Computing => panic!(
                            "Got unexpected state `Computing` after resolving `init` future. \
                        This might be a bug in Moka"
                        ),
                    }
                }
            }
        }
    }

    #[inline]
    pub(crate) fn remove_waiter(&self, key: &Arc<K>, type_id: TypeId) {
        let key = Arc::clone(key);
        self.waiters.remove(&(key, type_id));
    }

    #[inline]
    fn try_insert_waiter(
        &self,
        key: &Arc<K>,
        type_id: TypeId,
        waiter: &Waiter<V>,
    ) -> Option<Waiter<V>> {
        let key = Arc::clone(key);
        let waiter = Arc::clone(waiter);

        self.waiters
            .insert_with_or_modify((key, type_id), || waiter, |_, w| Arc::clone(w))
    }
}

fn panic_if_retry_exhausted_for_panicking(retries: usize, max: usize) {
    if retries >= max {
        panic!(
            "Too many retries. Tried to read the return value from the `init` future \
    but failed {} times. Maybe the `init` kept panicking?",
            retries
        );
    }
}

fn panic_if_retry_exhausted_for_aborting(retries: usize, max: usize) {
    if retries >= max {
        panic!(
            "Too many retries. Tried to read the return value from the `init` future \
    but failed {} times. Maybe the future containing `get_or_insert_with`/\
    `get_or_try_insert_with` kept being aborted?",
            retries
        );
    }
}
