//! Provides a thread-safe, concurrent asynchronous (futures aware) cache
//! implementation.
//!
//! To use this module, enable a crate feature called "future".

use async_lock::Mutex;
use crossbeam_channel::Sender;
use futures_util::future::{BoxFuture, Shared};
use once_cell::sync::Lazy;
use std::{future::Future, hash::Hash, sync::Arc};

use crate::common::{concurrent::WriteOp, time::Instant};

mod base_cache;
mod builder;
mod cache;
mod entry_selector;
mod housekeeper;
mod invalidator;
mod key_lock;
mod notifier;
mod value_initializer;

pub use {
    builder::CacheBuilder,
    cache::Cache,
    entry_selector::{OwnedKeyEntrySelector, RefKeyEntrySelector},
};

/// The type of the unique ID to identify a predicate used by
/// [`Cache::invalidate_entries_if`][invalidate-if] method.
///
/// A `PredicateId` is a `String` of UUID (version 4).
///
/// [invalidate-if]: ./struct.Cache.html#method.invalidate_entries_if
pub type PredicateId = String;

pub(crate) type PredicateIdStr<'a> = &'a str;

// Empty struct to be used in InitResult::InitErr to represent the Option None.
pub(crate) struct OptionallyNone;

impl<T: ?Sized> FutureExt for T where T: Future {}

pub trait FutureExt: Future {
    fn boxed<'a, T>(self) -> BoxFuture<'a, T>
    where
        Self: Future<Output = T> + Sized + Send + 'a,
    {
        Box::pin(self)
    }
}

/// Iterator visiting all key-value pairs in a cache in arbitrary order.
///
/// Call [`Cache::iter`](./struct.Cache.html#method.iter) method to obtain an `Iter`.
pub struct Iter<'i, K, V>(crate::sync_base::iter::Iter<'i, K, V>);

impl<'i, K, V> Iter<'i, K, V> {
    pub(crate) fn new(inner: crate::sync_base::iter::Iter<'i, K, V>) -> Self {
        Self(inner)
    }
}

impl<'i, K, V> Iterator for Iter<'i, K, V>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    type Item = (Arc<K>, V);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

/// Operation that has been interrupted by async cancellation.
pub(crate) enum PendingOp<K, V> {
    CallEvictionListener {
        ts: Instant,
        // 'static means that the future can capture only owned value and/or static
        // references. No non-static references are allowed.
        future: Shared<BoxFuture<'static, ()>>,
        op: WriteOp<K, V>,
    },
    SendWriteOp {
        ts: Instant,
        op: WriteOp<K, V>,
    },
}

/// Drop guard for an operation being performed. If this guard is dropped while it is
/// still having the future or the write op, it will convert them to a PendingOp and
/// send to the pending operation channel, so that the operation can be retried
/// later.
struct PendingOpGuard<'a, K, V> {
    pending_op_ch: &'a Sender<PendingOp<K, V>>,
    ts: Instant,
    future: Option<Shared<BoxFuture<'static, ()>>>,
    op: Option<WriteOp<K, V>>,
}

impl<'a, K, V> PendingOpGuard<'a, K, V> {
    fn new(pending_op_ch: &'a Sender<PendingOp<K, V>>, ts: Instant) -> Self {
        Self {
            pending_op_ch,
            ts,
            future: Default::default(),
            op: Default::default(),
        }
    }

    fn set_future_and_op(&mut self, future: Shared<BoxFuture<'static, ()>>, op: WriteOp<K, V>) {
        self.future = Some(future);
        self.op = Some(op);
    }

    fn set_op(&mut self, op: WriteOp<K, V>) {
        self.op = Some(op);
    }

    fn unset_future(&mut self) {
        self.future = None;
    }

    fn clear(&mut self) {
        self.future = None;
        self.op = None;
    }
}

impl<'a, K, V> Drop for PendingOpGuard<'a, K, V> {
    fn drop(&mut self) {
        let pending_op = match (self.future.take(), self.op.take()) {
            (Some(future), Some(op)) => PendingOp::CallEvictionListener {
                ts: self.ts,
                future,
                op,
            },
            (None, Some(op)) => PendingOp::SendWriteOp { ts: self.ts, op },
            _ => return,
        };

        self.pending_op_ch
            .send(pending_op)
            .expect("Failed to send a pending op");
    }
}

/// May yield to other async tasks.
pub(crate) async fn may_yield() {
    static LOCK: Lazy<Mutex<()>> = Lazy::new(Default::default);

    // Acquire the lock then immediately release it. This `await` may yield to other
    // tasks.
    //
    // NOTE: This behavior was tested with Tokio and async-std.
    let _ = LOCK.lock().await;
}
