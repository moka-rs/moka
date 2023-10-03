use async_lock::{RwLock, RwLockWriteGuard};
use async_trait::async_trait;
use futures_util::FutureExt;
use std::{
    any::{Any, TypeId},
    future::Future,
    hash::{BuildHasher, Hash},
    pin::Pin,
    sync::Arc,
};
use triomphe::Arc as TrioArc;

use super::OptionallyNone;

const WAITER_MAP_NUM_SEGMENTS: usize = 64;

#[async_trait]
pub(crate) trait GetOrInsert<K, V> {
    async fn get_without_recording<I>(
        &self,
        key: &Arc<K>,
        hash: u64,
        replace_if: Option<&mut I>,
    ) -> Option<V>
    where
        V: 'static,
        I: for<'i> FnMut(&'i V) -> bool + Send;

    async fn insert(&self, key: Arc<K>, hash: u64, value: V);
}

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

type Waiter<V> = TrioArc<RwLock<WaiterValue<V>>>;
type WaiterMap<K, V, S> = crate::cht::SegmentedHashMap<(Arc<K>, TypeId), Waiter<V>, S>;

struct WaiterGuard<'a, K, V, S>
// NOTE: We usually do not attach trait bounds to here at the struct definition, but
// the Drop trait requires these bounds here.
where
    K: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    w_key: Option<(Arc<K>, TypeId)>,
    w_hash: u64,
    waiters: &'a WaiterMap<K, V, S>,
    write_lock: RwLockWriteGuard<'a, WaiterValue<V>>,
}

impl<'a, K, V, S> WaiterGuard<'a, K, V, S>
where
    K: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    fn new(
        w_key: (Arc<K>, TypeId),
        w_hash: u64,
        waiters: &'a WaiterMap<K, V, S>,
        write_lock: RwLockWriteGuard<'a, WaiterValue<V>>,
    ) -> Self {
        Self {
            w_key: Some(w_key),
            w_hash,
            waiters,
            write_lock,
        }
    }

    fn set_waiter_value(mut self, v: WaiterValue<V>) {
        *self.write_lock = v;
        if let Some(w_key) = self.w_key.take() {
            remove_waiter(self.waiters, w_key, self.w_hash);
        }
    }
}

impl<'a, K, V, S> Drop for WaiterGuard<'a, K, V, S>
where
    K: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    fn drop(&mut self) {
        if let Some(w_key) = self.w_key.take() {
            // Value is not set. This means the future containing `*get_with` method
            // has been aborted. Remove our waiter to prevent the issue described in
            // https://github.com/moka-rs/moka/issues/59
            *self.write_lock = WaiterValue::EnclosingFutureAborted;
            remove_waiter(self.waiters, w_key, self.w_hash);
        }
    }
}

pub(crate) struct ValueInitializer<K, V, S> {
    // TypeId is the type ID of the concrete error type of generic type E in the
    // try_get_with method. We use the type ID as a part of the key to ensure that we
    // can always downcast the trait object ErrorObject (in Waiter<V>) into its
    // concrete type.
    waiters: TrioArc<WaiterMap<K, V, S>>,
}

impl<K, V, S> ValueInitializer<K, V, S>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Send + Sync + 'static,
{
    pub(crate) fn with_hasher(hasher: S) -> Self {
        Self {
            waiters: TrioArc::new(crate::cht::SegmentedHashMap::with_num_segments_and_hasher(
                WAITER_MAP_NUM_SEGMENTS,
                hasher,
            )),
        }
    }

    //
    // NOTES: We use `Pin<&mut impl Future>` instead of `impl Future` here for the
    // `init` argument. This is because we want to avoid the future size inflation
    // caused by calling nested async functions. See the following links for more
    // details:
    //
    // - https://github.com/moka-rs/moka/issues/212
    // - https://swatinem.de/blog/future-size/
    //

    /// # Panics
    /// Panics if the `init` future has been panicked.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn try_init_or_read<'a, C, I, O, E>(
        &'a self,
        c_key: &Arc<K>,
        c_hash: u64,
        type_id: TypeId,
        cache: &C,
        mut ignore_if: Option<I>,
        // Future to initialize a new value.
        init: Pin<&mut impl Future<Output = O>>,
        // Function to convert a value O, returned from the init future, into
        // Result<V, E>.
        post_init: fn(O) -> Result<V, E>,
    ) -> InitResult<V, E>
    where
        C: GetOrInsert<K, V> + Send + 'a,
        I: FnMut(&V) -> bool + Send,
        E: Send + Sync + 'static,
    {
        use std::panic::{resume_unwind, AssertUnwindSafe};
        use InitResult::*;

        const MAX_RETRIES: usize = 200;
        let mut retries = 0;

        let (w_key, w_hash) = waiter_key_hash(&self.waiters, c_key, type_id);

        let waiter = TrioArc::new(RwLock::new(WaiterValue::Computing));
        // NOTE: We have to acquire a write lock before `try_insert_waiter`,
        // so that any concurrent attempt will get our lock and wait on it.
        let lock = waiter.write().await;

        loop {
            let Some(existing_waiter) =
                try_insert_waiter(&self.waiters, w_key.clone(), w_hash, &waiter)
            else {
                break;
            };

            // Somebody else's waiter already exists, so wait for its result to become available.
            let waiter_result = existing_waiter.read().await;
            match &*waiter_result {
                WaiterValue::Ready(Ok(value)) => return ReadExisting(value.clone()),
                WaiterValue::Ready(Err(e)) => return InitErr(Arc::clone(e).downcast().unwrap()),
                // Somebody else's init future has been panicked.
                WaiterValue::InitFuturePanicked => {
                    retries += 1;
                    panic_if_retry_exhausted_for_panicking(retries, MAX_RETRIES);
                    // Retry from the beginning.
                    continue;
                }
                // Somebody else (a future containing `get_with`/`try_get_with`)
                // has been aborted.
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

        // Our waiter was inserted.

        // Create a guard. This will ensure to remove our waiter when the
        // enclosing future has been aborted:
        // https://github.com/moka-rs/moka/issues/59
        let waiter_guard = WaiterGuard::new(w_key, w_hash, &self.waiters, lock);

        // Check if the value has already been inserted by other thread.
        if let Some(value) = cache
            .get_without_recording(c_key, c_hash, ignore_if.as_mut())
            .await
        {
            // Yes. Set the waiter value, remove our waiter, and return
            // the existing value.
            waiter_guard.set_waiter_value(WaiterValue::Ready(Ok(value.clone())));
            return ReadExisting(value);
        }

        // The value still does note exist. Let's resolve the init
        // future. Catching panic is safe here as we do not try to
        // resolve the future again.
        match AssertUnwindSafe(init).catch_unwind().await {
            // Resolved.
            Ok(value) => match post_init(value) {
                Ok(value) => {
                    cache.insert(Arc::clone(c_key), c_hash, value.clone()).await;
                    waiter_guard.set_waiter_value(WaiterValue::Ready(Ok(value.clone())));
                    Initialized(value)
                }
                Err(e) => {
                    let err: ErrorObject = Arc::new(e);
                    waiter_guard.set_waiter_value(WaiterValue::Ready(Err(Arc::clone(&err))));
                    InitErr(err.downcast().unwrap())
                }
            },
            // Panicked.
            Err(payload) => {
                waiter_guard.set_waiter_value(WaiterValue::InitFuturePanicked);
                resume_unwind(payload);
            }
        }
        // The lock will be unlocked here.
    }

    /// The `post_init` function for the `get_with` method of cache.
    pub(crate) fn post_init_for_get_with(value: V) -> Result<V, ()> {
        Ok(value)
    }

    /// The `post_init` function for the `optionally_get_with` method of cache.
    pub(crate) fn post_init_for_optionally_get_with(
        value: Option<V>,
    ) -> Result<V, Arc<OptionallyNone>> {
        // `value` can be either `Some` or `None`. For `None` case, without change
        // the existing API too much, we will need to convert `None` to Arc<E> here.
        // `Infallible` could not be instantiated. So it might be good to use an
        // empty struct to indicate the error type.
        value.ok_or(Arc::new(OptionallyNone))
    }

    /// The `post_init` function for `try_get_with` method of cache.
    pub(crate) fn post_init_for_try_get_with<E>(result: Result<V, E>) -> Result<V, E> {
        result
    }

    /// Returns the `type_id` for `get_with` method of cache.
    pub(crate) fn type_id_for_get_with() -> TypeId {
        // NOTE: We use a regular function here instead of a const fn because TypeId
        // is not stable as a const fn. (as of our MSRV)
        TypeId::of::<()>()
    }

    /// Returns the `type_id` for `optionally_get_with` method of cache.
    pub(crate) fn type_id_for_optionally_get_with() -> TypeId {
        TypeId::of::<OptionallyNone>()
    }

    /// Returns the `type_id` for `try_get_with` method of cache.
    pub(crate) fn type_id_for_try_get_with<E: 'static>() -> TypeId {
        TypeId::of::<E>()
    }
}

#[inline]
fn remove_waiter<K, V, S>(waiter_map: &WaiterMap<K, V, S>, w_key: (Arc<K>, TypeId), w_hash: u64)
where
    (Arc<K>, TypeId): Eq + Hash,
    S: BuildHasher,
{
    waiter_map.remove(w_hash, |k| k == &w_key);
}

#[inline]
fn try_insert_waiter<K, V, S>(
    waiter_map: &WaiterMap<K, V, S>,
    w_key: (Arc<K>, TypeId),
    w_hash: u64,
    waiter: &Waiter<V>,
) -> Option<Waiter<V>>
where
    (Arc<K>, TypeId): Eq + Hash,
    S: BuildHasher,
{
    let waiter = TrioArc::clone(waiter);
    waiter_map.insert_if_not_present(w_key, w_hash, waiter)
}

#[inline]
fn waiter_key_hash<K, V, S>(
    waiter_map: &WaiterMap<K, V, S>,
    c_key: &Arc<K>,
    type_id: TypeId,
) -> ((Arc<K>, TypeId), u64)
where
    (Arc<K>, TypeId): Eq + Hash,
    S: BuildHasher,
{
    let w_key = (Arc::clone(c_key), type_id);
    let w_hash = waiter_map.hash(&w_key);
    (w_key, w_hash)
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
    but failed {} times. Maybe the future containing `get_with`/`try_get_with` \
    kept being aborted?",
            retries
        );
    }
}
