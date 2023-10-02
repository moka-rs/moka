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
    pub(crate) fn try_init_or_read<O, E>(
        &self,
        key: &Arc<K>,
        type_id: TypeId,
        // Closure to get an existing value from cache.
        mut get: impl FnMut() -> Option<V>,
        // Closure to initialize a new value.
        init: impl FnOnce() -> O,
        // Closure to insert a new value into cache.
        mut insert: impl FnMut(V),
        // Function to convert a value O, returned from the init future, into
        // Result<V, E>.
        post_init: fn(O) -> Result<V, E>,
    ) -> InitResult<V, E>
    where
        E: Send + Sync + 'static,
    {
        use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
        use InitResult::*;

        const MAX_RETRIES: usize = 200;
        let mut retries = 0;

        let (w_key, w_hash) = self.waiter_key_hash(key, type_id);

        let waiter = TrioArc::new(RwLock::new(None));
        let mut lock = waiter.write();

        loop {
            let Some(existing_waiter) = self.try_insert_waiter(w_key.clone(), w_hash, &waiter)
            else {
                break;
            };

            // Somebody else's waiter already exists, so wait for its result to become available.
            let waiter_result = existing_waiter.read();
            match &*waiter_result {
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

        // Our waiter was inserted.

        // Check if the value has already been inserted by other thread.
        if let Some(value) = get() {
            // Yes. Set the waiter value, remove our waiter, and return
            // the existing value.
            *lock = Some(Ok(value.clone()));
            self.remove_waiter(w_key, w_hash);
            return InitResult::ReadExisting(value);
        }

        // The value still does note exist. Let's evaluate the init
        // closure. Catching panic is safe here as we do not try to
        // evaluate the closure again.
        match catch_unwind(AssertUnwindSafe(init)) {
            // Evaluated.
            Ok(value) => {
                let (waiter_val, init_res) = match post_init(value) {
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
                *lock = waiter_val;
                self.remove_waiter(w_key, w_hash);
                init_res
            }
            // Panicked.
            Err(payload) => {
                *lock = None;
                // Remove the waiter so that others can retry.
                self.remove_waiter(w_key, w_hash);
                resume_unwind(payload);
            }
        }
        // The write lock will be unlocked here.
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

    #[inline]
    fn remove_waiter(&self, w_key: (Arc<K>, TypeId), w_hash: u64) {
        self.waiters.remove(w_hash, |k| k == &w_key);
    }

    #[inline]
    fn try_insert_waiter(
        &self,
        w_key: (Arc<K>, TypeId),
        w_hash: u64,
        waiter: &Waiter<V>,
    ) -> Option<Waiter<V>> {
        let waiter = TrioArc::clone(waiter);
        self.waiters.insert_if_not_present(w_key, w_hash, waiter)
    }

    #[inline]
    fn waiter_key_hash(&self, c_key: &Arc<K>, type_id: TypeId) -> ((Arc<K>, TypeId), u64) {
        let w_key = (Arc::clone(c_key), type_id);
        let w_hash = self.waiters.hash(&w_key);
        (w_key, w_hash)
    }
}
