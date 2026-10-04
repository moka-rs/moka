//! Publishes cache-removal records to application-owned asynchronous work without
//! blocking Moka's synchronous post-removal observer.
//!
//! The bounded queue is intentionally accessed with `try_send`. A full or closed
//! queue is an application-level delivery failure: production code should expose
//! it through a metric or dirty/reconciliation state rather than waiting inside
//! the observer.

use moka::{future::Cache, notification::RemovalCause};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::mpsc::{self, error::TrySendError};

#[derive(Debug)]
struct RemovalTicket {
    key: Arc<String>,
    value: String,
    cause: RemovalCause,
}

#[tokio::main]
async fn main() {
    let (sender, mut receiver) = mpsc::channel(64);
    let rejected = Arc::new(AtomicUsize::new(0));
    let rejected_by_observer = Arc::clone(&rejected);
    let cache = Cache::builder()
        .max_capacity(10_000)
        .post_removal_observer(move |key, value, cause| {
            // The observer must not wait for capacity. The application decides
            // how to reconcile state if its bounded queue is full or closed.
            let ticket = RemovalTicket { key, value, cause };
            if let Err(TrySendError::Full(_) | TrySendError::Closed(_)) = sender.try_send(ticket) {
                rejected_by_observer.fetch_add(1, Ordering::Relaxed);
            }
        })
        .build();

    let key = "alice".to_owned();
    cache.insert(key.clone(), "first".to_owned()).await;
    cache.insert(key, "second".to_owned()).await;

    let ticket = receiver.recv().await.expect("replacement ticket");
    assert_eq!(ticket.key.as_str(), "alice");
    assert_eq!(ticket.value, "first");
    assert_eq!(ticket.cause, RemovalCause::Replaced);
    assert_eq!(rejected.load(Ordering::Relaxed), 0);
}
