use parking_lot::RwLock;
use std::{
    any::{Any, TypeId},
    hash::{BuildHasher, Hash},
    sync::Arc,
};
use triomphe::Arc as TrioArc;

use super::OptionallyNone;

const WAITER_MAP_NUM_SEGMENTS: usize = 64;

type ErrorObject = Arc<dyn Any + Send + Sync + 'static>;
type WaiterValue<V> = Option<Result<V, ErrorObject>>;
type Waiter<V> = TrioArc<RwLock<WaiterValue<V>>>;

pub(crate) enum InitResult<V, E> {
    Initialized(V),
    ReadExisting(V),
    InitErr(Arc<E>),
}

pub(crate) struct ValueInitializer<K, V, S> {
    // TypeId is the type ID of the concrete error type of generic type E in the
    // try_get_with method. We use the type ID as a part of the key to ensure that
    // we can always downcast the trait object ErrorObject (in Waiter<V>) into
    // its concrete type.
    waiters: crate::cht::SegmentedHashMap<(Arc<K>, TypeId), Waiter<V>, S>,
}

impl<K, V, S> ValueInitializer<K, V, S>
where
    K: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    pub(crate) fn with_hasher(hasher: S) -> Self {
        Self {
            waiters: crate::cht::SegmentedHashMap::with_num_segments_and_hasher(
                WAITER_MAP_NUM_SEGMENTS,
                hasher,
            ),
        }
    }

    /// # Panics
    /// Panics if the `init` closure has been panicked.
    pub(crate) fn init_or_read(
        &self,
        key: Arc<K>,
        // Closure to get an existing value from cache.
        get: impl FnMut() -> Option<V>,
        init: impl FnOnce() -> V,
        // Closure to insert a new value into cache.
        mut insert: impl FnMut(V),
    ) -> InitResult<V, ()> {
        // This closure will be called before the init closure is called, in order to
        // check if the value has already been inserted by other thread.
        let pre_init = make_pre_init(get);

        // This closure will be called after the init closure has returned a value.
        // It will insert the returned value (from init) to the cache, and convert
        // the value into a pair of a WaiterValue and an InitResult.
        let post_init = |value: V| {
            insert(value.clone());
            (Some(Ok(value.clone())), InitResult::Initialized(value))
        };

        let type_id = TypeId::of::<()>();
        self.do_try_init(&key, type_id, pre_init, init, post_init)
    }

    /// # Panics
    /// Panics if the `init` closure has been panicked.
    pub(crate) fn try_init_or_read<E>(
        &self,
        key: Arc<K>,
        get: impl FnMut() -> Option<V>,
        init: impl FnOnce() -> Result<V, E>,
        mut insert: impl FnMut(V),
    ) -> InitResult<V, E>
    where
        E: Send + Sync + 'static,
    {
        let type_id = TypeId::of::<E>();

        // This closure will be called before the init closure is called, in order to
        // check if the value has already been inserted by other thread.
        let pre_init = make_pre_init(get);

        // This closure will be called after the init closure has returned a value.
        // It will insert the returned value (from init) to the cache, and convert
        // the value into a pair of a WaiterValue and an InitResult.
        let post_init = |value: Result<V, E>| match value {
            Ok(value) => {
                insert(value.clone());
                (Some(Ok(value.clone())), InitResult::Initialized(value))
            }
            Err(e) => {
                let err: ErrorObject = Arc::new(e);
                (
                    Some(Err(Arc::clone(&err))),
                    InitResult::InitErr(err.downcast().unwrap()),
                )
            }
        };

        self.do_try_init(&key, type_id, pre_init, init, post_init)
    }

    /// # Panics
    /// Panics if the `init` closure has been panicked.
    pub(super) fn optionally_init_or_read(
        &self,
        key: Arc<K>,
        get: impl FnMut() -> Option<V>,
        init: impl FnOnce() -> Option<V>,
        mut insert: impl FnMut(V),
    ) -> InitResult<V, OptionallyNone> {
        let type_id = TypeId::of::<OptionallyNone>();

        // This closure will be called before the init closure is called, in order to
        // check if the value has already been inserted by other thread.
        let pre_init = make_pre_init(get);

        // This closure will be called after the init closure has returned a value.
        // It will insert the returned value (from init) to the cache, and convert
        // the value into a pair of a WaiterValue and an InitResult.
        let post_init = |value: Option<V>| match value {
            Some(value) => {
                insert(value.clone());
                (Some(Ok(value.clone())), InitResult::Initialized(value))
            }
            None => {
                // `value` can be either `Some` or `None`. For `None` case, without
                // change the existing API too much, we will need to convert `None`
                // to Arc<E> here. `Infallible` could not be instantiated. So it
                // might be good to use an empty struct to indicate the error type.
                let err: ErrorObject = Arc::new(OptionallyNone);
                (
                    Some(Err(Arc::clone(&err))),
                    InitResult::InitErr(err.downcast().unwrap()),
                )
            }
        };

        self.do_try_init(&key, type_id, pre_init, init, post_init)
    }

    /// # Panics
    /// Panics if the `init` closure has been panicked.
    fn do_try_init<O, E>(
        &self,
        key: &Arc<K>,
        type_id: TypeId,
        mut pre_init: impl FnMut() -> Option<(WaiterValue<V>, InitResult<V, E>)>,
        init: impl FnOnce() -> O,
        mut post_init: impl FnMut(O) -> (WaiterValue<V>, InitResult<V, E>),
    ) -> InitResult<V, E>
    where
        E: Send + Sync + 'static,
    {
        use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
        use InitResult::*;

        const MAX_RETRIES: usize = 200;
        let mut retries = 0;

        let (cht_key, hash) = self.cht_key_hash(key, type_id);

        loop {
            let waiter = TrioArc::new(RwLock::new(None));
            let mut lock = waiter.write();

            match self.try_insert_waiter(cht_key.clone(), hash, &waiter) {
                None => {
                    // Our waiter was inserted.
                    // Check if the value has already been inserted by other thread.
                    if let Some((waiter_val, init_res)) = pre_init() {
                        // Yes. Set the waiter value, remove our waiter, and return
                        // the existing value.
                        *lock = waiter_val;
                        self.remove_waiter(cht_key, hash);
                        return init_res;
                    }

                    // The value still does note exist. Let's evaluate the init
                    // closure. Catching panic is safe here as we do not try to
                    // evaluate the closure again.
                    match catch_unwind(AssertUnwindSafe(init)) {
                        // Evaluated.
                        Ok(value) => {
                            let (waiter_val, init_res) = post_init(value);
                            *lock = waiter_val;
                            self.remove_waiter(cht_key, hash);
                            return init_res;
                        }
                        // Panicked.
                        Err(payload) => {
                            *lock = None;
                            // Remove the waiter so that others can retry.
                            self.remove_waiter(cht_key, hash);
                            resume_unwind(payload);
                        }
                    } // The write lock will be unlocked here.
                }
                Some(res) => {
                    // Somebody else's waiter already exists. Drop our write lock and
                    // wait for the read lock to become available.
                    std::mem::drop(lock);
                    match &*res.read() {
                        Some(Ok(value)) => return ReadExisting(value.clone()),
                        Some(Err(e)) => return InitErr(Arc::clone(e).downcast().unwrap()),
                        // None means somebody else's init closure has been panicked.
                        None => {
                            retries += 1;
                            if retries < MAX_RETRIES {
                                // Retry from the beginning.
                                continue;
                            } else {
                                panic!(
                                    "Too many retries. Tried to read the return value from the `init` \
                                closure but failed {} times. Maybe the `init` kept panicking?",
                                    retries
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[inline]
    fn remove_waiter(&self, cht_key: (Arc<K>, TypeId), hash: u64) {
        self.waiters.remove(hash, |k| k == &cht_key);
    }

    #[inline]
    fn try_insert_waiter(
        &self,
        cht_key: (Arc<K>, TypeId),
        hash: u64,
        waiter: &Waiter<V>,
    ) -> Option<Waiter<V>> {
        let waiter = TrioArc::clone(waiter);
        self.waiters.insert_if_not_present(cht_key, hash, waiter)
    }

    #[inline]
    fn cht_key_hash(&self, key: &Arc<K>, type_id: TypeId) -> ((Arc<K>, TypeId), u64) {
        let cht_key = (Arc::clone(key), type_id);
        let hash = self.waiters.hash(&cht_key);
        (cht_key, hash)
    }
}

#[inline]
fn make_pre_init<V, E>(
    mut get: impl FnMut() -> Option<V>,
) -> impl FnMut() -> Option<(WaiterValue<V>, InitResult<V, E>)>
where
    V: Clone,
{
    move || get().map(|value| (Some(Ok(value.clone())), InitResult::ReadExisting(value)))
}
