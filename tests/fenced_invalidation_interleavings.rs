#![cfg(all(feature = "sync", feature = "future"))]

mod boundary_interleavings {
    use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
    use futures_util::future::poll_immediate;
    use moka::{future::Cache, policy::Expiry};
    use std::{
        hash::{Hash, Hasher},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    struct CloneGate {
        armed: std::sync::atomic::AtomicBool,
        worker: Mutex<Option<thread::ThreadId>>,
        reached: Sender<()>,
        release: Receiver<()>,
    }
    struct Value {
        number: usize,
        gate: Arc<CloneGate>,
    }
    impl Clone for Value {
        fn clone(&self) -> Self {
            if self.number == 1
                && *self.gate.worker.lock().unwrap() == Some(thread::current().id())
                && self.gate.armed.swap(false, Ordering::SeqCst)
            {
                self.gate.reached.send(()).unwrap();
                self.gate.release.recv().unwrap();
            }
            Self {
                number: self.number,
                gate: self.gate.clone(),
            }
        }
    }

    #[derive(Eq, PartialEq)]
    struct Key(usize);
    impl Hash for Key {
        fn hash<H: Hasher>(&self, h: &mut H) {
            0u8.hash(h);
        }
    }

    struct Query {
        checkpoint: usize,
        count: AtomicUsize,
        reached: Sender<()>,
        release: Receiver<()>,
    }
    impl Hash for Query {
        fn hash<H: Hasher>(&self, h: &mut H) {
            0u8.hash(h);
        }
    }
    impl equivalent::Equivalent<Key> for Query {
        fn equivalent(&self, key: &Key) -> bool {
            if self.count.fetch_add(1, Ordering::SeqCst) == self.checkpoint {
                self.reached.send(()).unwrap();
                // Finite work only: admission may use a synchronous mutex held by
                // this comparison. Never wait for another cache call to progress.
                for _ in 0..32768 {
                    if self.release.try_recv().is_ok() {
                        break;
                    }
                    thread::yield_now();
                }
            }
            key.0 == 1
        }
    }
    struct HoldCreate {
        entered: Sender<()>,
        release: Mutex<Receiver<()>>,
    }
    impl Expiry<Key, Value> for HoldCreate {
        fn expire_after_create(&self, key: &Key, value: &Value, _: Instant) -> Option<Duration> {
            if key.0 == 1 && value.number == 1 {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            None
        }
    }

    pub(super) fn empty_admission_history() {
        for checkpoint in 0..8 {
            let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
            let (created_tx, created) = unbounded();
            let (release_create, release_create_rx) = unbounded();
            let (clone_tx, cloned) = unbounded();
            let (release_clone, release_clone_rx) = unbounded();
            let gate = Arc::new(CloneGate {
                armed: false.into(),
                worker: Mutex::new(None),
                reached: clone_tx,
                release: release_clone_rx,
            });
            let cache = Cache::<Key, Value>::builder()
                .max_capacity(100)
                .expire_after(HoldCreate {
                    entered: created_tx,
                    release: Mutex::new(release_create_rx),
                })
                .build();
            runtime.block_on(cache.insert(
                Key(0),
                Value {
                    number: 99,
                    gate: gate.clone(),
                },
            ));
            let (reached_tx, reached) = bounded(1);
            let (release_query, release_query_rx) = unbounded();
            let query = Query {
                checkpoint,
                count: AtomicUsize::new(0),
                reached: reached_tx.clone(),
                release: release_query_rx,
            };
            let (fence_polled_tx, fence_polled) = unbounded();
            let (resume_fence, resume_fence_rx) = unbounded();
            let fence_cache = cache.clone();
            let rt = runtime.clone();
            let fence = thread::spawn(move || {
                let mut future = Box::pin(fence_cache.invalidate_and_fence(&query));
                let done = rt.block_on(poll_immediate(future.as_mut())).is_some();
                fence_polled_tx.send(done).unwrap();
                if !done {
                    resume_fence_rx.recv().unwrap();
                    rt.block_on(future);
                }
            });
            let mut fence_done = None;
            crossbeam_channel::select! {
                recv(reached) -> event => { event.unwrap(); },
                recv(fence_polled) -> done => { fence_done = Some(done.unwrap()); },
            };
            let (init_polled_tx, init_polled) = unbounded();
            let (resume_init, resume_init_rx) = unbounded();
            let init_cache = cache.clone();
            let rt = runtime.clone();
            let init_gate = gate.clone();
            let init = thread::spawn(move || {
                *init_gate.worker.lock().unwrap() = Some(thread::current().id());
                let mut future = Box::pin(init_cache.get_with(Key(1), async {
                    Value {
                        number: 1,
                        gate: init_gate,
                    }
                }));
                let result = rt.block_on(poll_immediate(future.as_mut()));
                init_polled_tx
                    .send(result.as_ref().map(|v| v.number))
                    .unwrap();
                match result {
                    Some(value) => value.number,
                    None => {
                        resume_init_rx.recv().unwrap();
                        rt.block_on(future).number
                    }
                }
            });
            let in_create = crossbeam_channel::select! {
                recv(created) -> event => { event.unwrap(); true },
                recv(init_polled) -> result => { result.unwrap(); false },
            };
            let published =
                in_create && runtime.block_on(cache.get(&Key(1))).map(|v| v.number) == Some(1);
            let _ = release_query.send(());
            let fence_ready = fence_done.unwrap_or_else(|| fence_polled.recv().unwrap());
            if !fence_ready {
                release_create.send(()).unwrap();
                resume_init.send(()).unwrap();
                resume_fence.send(()).unwrap();
            }
            fence.join().unwrap();
            let mapping = runtime.block_on(cache.get(&Key(1))).map(|v| v.number);
            // A mapping actually removed by this fence belongs to the earlier side;
            // no assumption about the meaning of a comparison checkpoint is needed.
            let was_removed = published && mapping.is_none();
            let mut after = Box::pin(cache.get_with(Key(1), async {
                Value {
                    number: 2,
                    gate: gate.clone(),
                }
            }));
            let mut first = runtime.block_on(poll_immediate(after.as_mut()));
            if was_removed && fence_ready {
                gate.armed.store(true, Ordering::SeqCst);
            }
            let _ = release_create.send(());
            let _ = resume_init.send(());
            if was_removed && fence_ready {
                crossbeam_channel::select! {
                    recv(cloned) -> event => event.unwrap(),
                    recv(init_polled) -> result => { result.unwrap(); },
                }
                if first.is_none() {
                    first = runtime.block_on(poll_immediate(after.as_mut()));
                }
            }
            let _ = release_clone.send(());
            let later = first.unwrap_or_else(|| runtime.block_on(after)).number;
            let original = init.join().unwrap();
            if was_removed {
                assert_eq!(original, 1);
                assert_eq!(
                    later, 2,
                    "checkpoint {checkpoint}: post-return caller joined the removed group"
                );
                assert_eq!(
                    runtime.block_on(cache.get(&Key(1))).map(|v| v.number),
                    Some(2)
                );
            } else {
                assert!(original == 1 || original == 2);
                assert_eq!(
                    runtime.block_on(cache.get(&Key(1))).map(|v| v.number),
                    Some(later)
                );
            }
            assert_eq!(
                runtime.block_on(cache.get(&Key(0))).map(|v| v.number),
                Some(99)
            );
        }
    }

    #[allow(deprecated)]
    pub(super) async fn listener_reentry_while_another_fence_waits() {
        use moka::notification::{ListenerFuture, RemovalCause};
        let slot = Arc::new(Mutex::new(None::<Cache<&'static str, usize>>));
        let listener_slot = slot.clone();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let cache = Cache::builder()
            .max_capacity(100)
            .async_eviction_listener(move |key, _, cause| {
                let entered_tx = entered_tx.clone();
                let release_rx = release_rx.clone();
                let cache = listener_slot.lock().unwrap().as_ref().unwrap().clone();
                Box::pin(async move {
                    if *key == "blocker" && cause == RemovalCause::Replaced {
                        entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
                        let release = release_rx.lock().unwrap().take().unwrap();
                        release.await.unwrap();
                    } else if *key == "outer" && cause == RemovalCause::Explicit {
                        let fence_cache = cache.clone();
                        let mut other_fence =
                            Box::pin(
                                async move { fence_cache.invalidate_and_fence("blocker").await },
                            );
                        let _ = poll_immediate(other_fence.as_mut()).await;
                        assert_eq!(cache.get_with("nested", async { 3 }).await, 3);
                        // The unrelated fence is allowed to wait. Cancellation here
                        // is independent of the outer listener's required reentry.
                        drop(other_fence);
                    }
                }) as ListenerFuture
            })
            .build();
        *slot.lock().unwrap() = Some(cache.clone());
        cache.insert("outer", 1).await;
        cache.insert("blocker", 1).await;
        let publishing_cache = cache.clone();
        let publishing = tokio::spawn(async move {
            publishing_cache
                .get_with_if("blocker", async { 2 }, |_| true)
                .await
        });
        entered.await.unwrap();
        let completed =
            tokio::time::timeout(Duration::from_secs(30), cache.invalidate_and_fence("outer"))
                .await;
        let _ = release.send(());
        if completed.is_err() {
            publishing.abort();
            slot.lock().unwrap().take();
            panic!("fenced listener deadlocked while an unrelated fence was pending");
        }
        assert_eq!(publishing.await.unwrap(), 2);
        assert_eq!(cache.get("outer").await, None);
        assert_eq!(cache.get("nested").await, Some(3));
        slot.lock().unwrap().take();
    }
}

use moka::{
    notification::{ListenerFuture, RemovalCause},
    ops::compute::Op,
    sync::{Cache as SyncCache, SegmentedCache},
};
use std::{
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Barrier, Mutex,
    },
    task::Poll,
    thread::{self, JoinHandle},
};
use tokio::sync::oneshot;

#[derive(Debug, Eq, PartialEq)]
struct ReclaimKey(usize);

impl Hash for ReclaimKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        0_u8.hash(state);
    }
}

fn assert_sync_key_state_is_reclaimable(cache: SyncCache<Arc<ReclaimKey>, usize>) {
    let empty_key = Arc::new(ReclaimKey(0));
    let empty_weak = Arc::downgrade(&empty_key);

    assert_eq!(
        cache.optionally_get_with(Arc::clone(&empty_key), || None::<usize>),
        None
    );
    cache.invalidate_and_fence(&empty_key);
    drop(empty_key);

    for index in 1..=4096 {
        assert_eq!(
            cache.optionally_get_with(Arc::new(ReclaimKey(index)), || None::<usize>),
            None
        );
        if index % 64 == 0 {
            cache.run_pending_tasks();
            if empty_weak.upgrade().is_none() {
                break;
            }
        }
    }

    assert!(
        empty_weak.upgrade().is_none(),
        "quiescent fence state retained the empty-result cache key"
    );

    let populated_key = Arc::new(ReclaimKey(4097));
    let populated_weak = Arc::downgrade(&populated_key);
    assert_eq!(cache.get_with(Arc::clone(&populated_key), || 1), 1);
    cache.invalidate_and_fence(&populated_key);
    assert_eq!(cache.get(&populated_key), None);
    drop(populated_key);

    for offset in 1..=4096 {
        let index = 4097 + offset;
        let churn_key = Arc::new(ReclaimKey(index));
        assert_eq!(cache.get_with(Arc::clone(&churn_key), || index), index);
        cache.invalidate_and_fence(&churn_key);
        if offset % 64 == 0 {
            cache.run_pending_tasks();
            if populated_weak.upgrade().is_none() {
                break;
            }
        }
    }

    assert!(
        populated_weak.upgrade().is_none(),
        "quiescent fence state retained the successfully initialized cache key (strong count: {})",
        populated_weak.strong_count()
    );
}

fn assert_segmented_key_state_is_reclaimable(cache: SegmentedCache<Arc<ReclaimKey>, usize>) {
    let empty_key = Arc::new(ReclaimKey(0));
    let empty_weak = Arc::downgrade(&empty_key);

    assert_eq!(
        cache.optionally_get_with(Arc::clone(&empty_key), || None::<usize>),
        None
    );
    cache.invalidate_and_fence(&empty_key);
    drop(empty_key);

    for index in 1..=4096 {
        assert_eq!(
            cache.optionally_get_with(Arc::new(ReclaimKey(index)), || None::<usize>),
            None
        );
        if index % 64 == 0 {
            cache.run_pending_tasks();
            if empty_weak.upgrade().is_none() {
                break;
            }
        }
    }

    assert!(
        empty_weak.upgrade().is_none(),
        "quiescent fence state retained the empty-result segmented-cache key"
    );

    let populated_key = Arc::new(ReclaimKey(4097));
    let populated_weak = Arc::downgrade(&populated_key);
    assert_eq!(cache.get_with(Arc::clone(&populated_key), || 1), 1);
    cache.invalidate_and_fence(&populated_key);
    assert_eq!(cache.get(&populated_key), None);
    drop(populated_key);

    for offset in 1..=4096 {
        let index = 4097 + offset;
        let churn_key = Arc::new(ReclaimKey(index));
        assert_eq!(cache.get_with(Arc::clone(&churn_key), || index), index);
        cache.invalidate_and_fence(&churn_key);
        if offset % 64 == 0 {
            cache.run_pending_tasks();
            if populated_weak.upgrade().is_none() {
                break;
            }
        }
    }

    assert!(
        populated_weak.upgrade().is_none(),
        "quiescent fence state retained the successfully initialized segmented-cache key (strong count: {})",
        populated_weak.strong_count()
    );
}

async fn assert_future_key_state_is_reclaimable(
    cache: moka::future::Cache<Arc<ReclaimKey>, usize>,
) {
    let empty_key = Arc::new(ReclaimKey(0));
    let empty_weak = Arc::downgrade(&empty_key);

    assert_eq!(
        cache
            .optionally_get_with(Arc::clone(&empty_key), async { None::<usize> })
            .await,
        None
    );
    cache.invalidate_and_fence(&empty_key).await;
    drop(empty_key);

    for index in 1..=4096 {
        assert_eq!(
            cache
                .optionally_get_with(Arc::new(ReclaimKey(index)), async { None::<usize> })
                .await,
            None
        );
        if index % 64 == 0 {
            cache.run_pending_tasks().await;
            if empty_weak.upgrade().is_none() {
                break;
            }
        }
    }

    assert!(
        empty_weak.upgrade().is_none(),
        "quiescent fence state retained the empty-result future-cache key"
    );

    let populated_key = Arc::new(ReclaimKey(4097));
    let populated_weak = Arc::downgrade(&populated_key);
    assert_eq!(
        cache
            .get_with(Arc::clone(&populated_key), async { 1 })
            .await,
        1
    );
    cache.invalidate_and_fence(&populated_key).await;
    assert_eq!(cache.get(&populated_key).await, None);
    drop(populated_key);

    for offset in 1..=4096 {
        let index = 4097 + offset;
        let churn_key = Arc::new(ReclaimKey(index));
        assert_eq!(
            cache
                .get_with(Arc::clone(&churn_key), async move { index })
                .await,
            index
        );
        cache.invalidate_and_fence(&churn_key).await;
        if offset % 64 == 0 {
            cache.run_pending_tasks().await;
            if populated_weak.upgrade().is_none() {
                break;
            }
        }
    }

    assert!(
        populated_weak.upgrade().is_none(),
        "quiescent fence state retained the successfully initialized future-cache key (strong count: {})",
        populated_weak.strong_count()
    );
}

struct SyncLoad {
    started: Receiver<()>,
    release: SyncSender<()>,
    handle: JoinHandle<String>,
}

struct FutureLoad {
    started: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<String>,
}

fn start_sync_load(cache: SyncCache<String, String>, key: &str, value: &str) -> SyncLoad {
    let key = key.to_owned();
    let value = value.to_owned();
    let (started_tx, started) = sync_channel(0);
    let (release, release_rx) = sync_channel(0);
    let handle = thread::spawn(move || {
        cache.get_with(key, || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            value
        })
    });
    SyncLoad {
        started,
        release,
        handle,
    }
}

fn start_segmented_load(cache: SegmentedCache<String, String>, key: &str, value: &str) -> SyncLoad {
    let key = key.to_owned();
    let value = value.to_owned();
    let (started_tx, started) = sync_channel(0);
    let (release, release_rx) = sync_channel(0);
    let handle = thread::spawn(move || {
        cache.get_with(key, || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            value
        })
    });
    SyncLoad {
        started,
        release,
        handle,
    }
}

fn start_future_load(
    cache: moka::future::Cache<String, String>,
    key: &str,
    value: &str,
) -> FutureLoad {
    let key = key.to_owned();
    let value = value.to_owned();
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        cache
            .get_with(key, async move {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                value
            })
            .await
    });
    FutureLoad {
        started,
        release,
        handle,
    }
}

#[test]
fn sync_fence_suppresses_an_inflight_get_with_value() {
    let cache = SyncCache::new(100);
    let load = start_sync_load(cache.clone(), "key", "stale");
    load.started.recv().unwrap();
    cache.invalidate_and_fence("key");
    load.release.send(()).unwrap();
    assert_eq!(load.handle.join().unwrap(), "stale");
    assert_eq!(cache.get("key"), None);
}

#[test]
fn sync_post_fence_initializers_form_a_new_single_flight_group() {
    let cache = SyncCache::new(100);
    let old = start_sync_load(cache.clone(), "key", "stale");
    old.started.recv().unwrap();

    cache.invalidate_and_fence("key");
    let fresh = cache.get_with("key".to_owned(), || "fresh".to_owned());
    assert_eq!(fresh, "fresh");

    old.release.send(()).unwrap();
    assert_eq!(old.handle.join().unwrap(), "stale");
    assert_eq!(cache.get("key").as_deref(), Some("fresh"));
}

#[test]
fn sync_a_later_direct_insert_wins_over_a_prefence_initializer() {
    let cache = SyncCache::new(100);
    let old = start_sync_load(cache.clone(), "key", "stale");
    old.started.recv().unwrap();

    cache.invalidate_and_fence("key");
    cache.insert("key".to_owned(), "direct".to_owned());
    old.release.send(()).unwrap();

    assert_eq!(old.handle.join().unwrap(), "stale");
    assert_eq!(cache.get("key").as_deref(), Some("direct"));
}

#[test]
fn ordinary_sync_invalidate_keeps_its_existing_inflight_behavior() {
    let cache = SyncCache::new(100);
    let load = start_sync_load(cache.clone(), "key", "loaded");
    load.started.recv().unwrap();

    cache.invalidate("key");
    load.release.send(()).unwrap();

    assert_eq!(load.handle.join().unwrap(), "loaded");
    assert_eq!(cache.get("key").as_deref(), Some("loaded"));
}

#[test]
#[allow(deprecated)]
fn sync_fence_applies_to_get_with_if_and_entry_initialization() {
    let cache = SyncCache::new(100);
    cache.insert("replace".to_owned(), "old".to_owned());

    let replace_cache = cache.clone();
    let (replace_started_tx, replace_started) = sync_channel(0);
    let (replace_release, replace_release_rx) = sync_channel(0);
    let replace = thread::spawn(move || {
        replace_cache.get_with_if(
            "replace".to_owned(),
            || {
                replace_started_tx.send(()).unwrap();
                replace_release_rx.recv().unwrap();
                "stale replacement".to_owned()
            },
            |_| true,
        )
    });
    replace_started.recv().unwrap();
    cache.invalidate_and_fence("replace");
    replace_release.send(()).unwrap();
    assert_eq!(replace.join().unwrap(), "stale replacement");
    assert_eq!(cache.get("replace"), None);

    let entry_cache = cache.clone();
    let (entry_started_tx, entry_started) = sync_channel(0);
    let (entry_release, entry_release_rx) = sync_channel(0);
    let entry = thread::spawn(move || {
        entry_cache
            .entry("entry".to_owned())
            .or_insert_with(|| {
                entry_started_tx.send(()).unwrap();
                entry_release_rx.recv().unwrap();
                "entry value".to_owned()
            })
            .into_value()
    });
    entry_started.recv().unwrap();
    cache.invalidate_and_fence("entry");
    entry_release.send(()).unwrap();
    assert_eq!(entry.join().unwrap(), "entry value");
    assert_eq!(cache.get("entry"), None);
}

#[test]
fn sync_optional_and_fallible_initializers_are_fenced_and_recover() {
    let cache = SyncCache::new(100);
    let optional_cache = cache.clone();
    let (optional_started_tx, optional_started) = sync_channel(0);
    let (optional_release, optional_release_rx) = sync_channel(0);
    let optional = thread::spawn(move || {
        optional_cache.optionally_get_with("optional".to_owned(), || {
            optional_started_tx.send(()).unwrap();
            optional_release_rx.recv().unwrap();
            Some("stale optional".to_owned())
        })
    });
    optional_started.recv().unwrap();
    cache.invalidate_and_fence("optional");
    optional_release.send(()).unwrap();
    assert_eq!(optional.join().unwrap().as_deref(), Some("stale optional"));
    assert_eq!(cache.get("optional"), None);

    let try_cache = cache.clone();
    let (try_started_tx, try_started) = sync_channel(0);
    let (try_release, try_release_rx) = sync_channel(0);
    let fallible = thread::spawn(move || {
        try_cache.try_get_with("fallible".to_owned(), || {
            try_started_tx.send(()).unwrap();
            try_release_rx.recv().unwrap();
            Ok::<_, &'static str>("stale fallible".to_owned())
        })
    });
    try_started.recv().unwrap();
    cache.invalidate_and_fence("fallible");
    try_release.send(()).unwrap();
    assert_eq!(fallible.join().unwrap().unwrap().as_str(), "stale fallible");
    assert_eq!(cache.get("fallible"), None);

    let error = cache.try_get_with("fallible".to_owned(), || Err::<String, _>("expected error"));
    assert_eq!(*error.unwrap_err(), "expected error");
    assert_eq!(
        cache.get_with("fallible".to_owned(), || "recovered".to_owned()),
        "recovered"
    );
}

#[test]
fn panicking_prefence_sync_initializer_does_not_poison_the_key() {
    let cache: SyncCache<String, String> = SyncCache::new(100);
    let panic_cache = cache.clone();
    let (started_tx, started) = sync_channel(0);
    let (release, release_rx) = sync_channel(0);
    let panicking = thread::spawn(move || {
        panic_cache.get_with("key".to_owned(), || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            panic!("expected initializer panic");
        })
    });
    started.recv().unwrap();

    cache.invalidate_and_fence("key");
    release.send(()).unwrap();
    assert!(panicking.join().is_err());
    assert_eq!(
        cache.get_with("key".to_owned(), || "recovered".to_owned()),
        "recovered"
    );
}

#[test]
fn sync_prefence_none_and_error_do_not_poison_later_initialization() {
    let cache: SyncCache<String, String> = SyncCache::new(100);
    let none_cache = cache.clone();
    let (none_started_tx, none_started) = sync_channel(0);
    let (none_release, none_release_rx) = sync_channel(0);
    let none = thread::spawn(move || {
        none_cache.optionally_get_with("none".to_owned(), || {
            none_started_tx.send(()).unwrap();
            none_release_rx.recv().unwrap();
            None
        })
    });
    none_started.recv().unwrap();
    cache.invalidate_and_fence("none");
    none_release.send(()).unwrap();
    assert_eq!(none.join().unwrap(), None);
    assert_eq!(
        cache.get_with("none".to_owned(), || "after none".to_owned()),
        "after none"
    );

    let error_cache = cache.clone();
    let (error_started_tx, error_started) = sync_channel(0);
    let (error_release, error_release_rx) = sync_channel(0);
    let error = thread::spawn(move || {
        error_cache.try_get_with("error".to_owned(), || {
            error_started_tx.send(()).unwrap();
            error_release_rx.recv().unwrap();
            Err::<String, _>("expected")
        })
    });
    error_started.recv().unwrap();
    cache.invalidate_and_fence("error");
    error_release.send(()).unwrap();
    assert_eq!(*error.join().unwrap().unwrap_err(), "expected");
    assert_eq!(
        cache.get_with("error".to_owned(), || "after error".to_owned()),
        "after error"
    );

    assert_sync_key_state_is_reclaimable(SyncCache::new(512));
}

#[test]
fn sync_repeated_fences_and_borrowed_keys_are_supported() {
    let cache: SyncCache<String, String> = SyncCache::new(100);
    let old = start_sync_load(cache.clone(), "borrowed", "stale");
    old.started.recv().unwrap();

    cache.invalidate_and_fence("borrowed");
    cache.invalidate_and_fence("borrowed");
    old.release.send(()).unwrap();

    assert_eq!(old.handle.join().unwrap(), "stale");
    assert_eq!(cache.get("borrowed"), None);
    assert_eq!(
        cache.get_with_by_ref("borrowed", || "fresh".to_owned()),
        "fresh"
    );

    let arc_key_cache = SyncCache::<Arc<str>, usize>::new(100);
    arc_key_cache.insert(Arc::<str>::from("arc key"), 1);
    arc_key_cache.invalidate_and_fence("arc key");
    assert!(!arc_key_cache.contains_key("arc key"));
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CollisionKey(&'static str);

impl Hash for CollisionKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        0_u8.hash(state);
    }
}

#[test]
fn sync_fence_does_not_cross_equal_hashes() {
    let cache: SyncCache<CollisionKey, String> = SyncCache::new(100);
    let key_a = CollisionKey("a");
    let key_b = CollisionKey("b");

    let (a_started_tx, a_started) = sync_channel(0);
    let (a_release, a_release_rx) = sync_channel(0);
    let cache_a = cache.clone();
    let a = key_a.clone();
    let load_a = thread::spawn(move || {
        cache_a.get_with(a, || {
            a_started_tx.send(()).unwrap();
            a_release_rx.recv().unwrap();
            "stale a".to_owned()
        })
    });

    let (b_started_tx, b_started) = sync_channel(0);
    let (b_release, b_release_rx) = sync_channel(0);
    let cache_b = cache.clone();
    let b = key_b.clone();
    let load_b = thread::spawn(move || {
        cache_b.get_with(b, || {
            b_started_tx.send(()).unwrap();
            b_release_rx.recv().unwrap();
            "value b".to_owned()
        })
    });

    a_started.recv().unwrap();
    b_started.recv().unwrap();
    cache.invalidate_and_fence(&key_a);
    a_release.send(()).unwrap();
    b_release.send(()).unwrap();

    assert_eq!(load_a.join().unwrap(), "stale a");
    assert_eq!(load_b.join().unwrap(), "value b");
    assert_eq!(cache.get(&key_a), None);
    assert_eq!(cache.get(&key_b).as_deref(), Some("value b"));
}

#[test]
fn sync_fence_reports_normal_explicit_removal() {
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let listener_notifications = Arc::clone(&notifications);
    let cache = SyncCache::builder()
        .max_capacity(100)
        .eviction_listener(move |key, value, cause| {
            listener_notifications
                .lock()
                .unwrap()
                .push((key, value, cause));
        })
        .build();

    cache.insert("key".to_owned(), "value".to_owned());
    cache.invalidate_and_fence("key");

    let actual = notifications.lock().unwrap();
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].0.as_str(), "key");
    assert_eq!(actual[0].1, "value");
    assert_eq!(actual[0].2, RemovalCause::Explicit);
}

#[test]
fn segmented_cache_supports_fenced_invalidation() {
    let observed_cache = SegmentedCache::new(100, 4);
    let load = start_segmented_load(observed_cache.clone(), "key", "stale");
    load.started.recv().unwrap();
    observed_cache.invalidate_and_fence("key");
    assert_eq!(
        observed_cache.get_with("key".to_owned(), || "fresh".to_owned()),
        "fresh"
    );
    load.release.send(()).unwrap();
    assert_eq!(load.handle.join().unwrap(), "stale");
    assert_eq!(observed_cache.get("key").as_deref(), Some("fresh"));

    let cache: SegmentedCache<String, String> = SegmentedCache::new(100, 4);
    let direct = start_segmented_load(cache.clone(), "direct", "stale direct");
    direct.started.recv().unwrap();
    cache.invalidate_and_fence("direct");
    cache.insert("direct".to_owned(), "direct value".to_owned());
    direct.release.send(()).unwrap();
    assert_eq!(direct.handle.join().unwrap(), "stale direct");
    assert_eq!(cache.get("direct").as_deref(), Some("direct value"));

    let oldest = start_segmented_load(cache.clone(), "repeated", "oldest");
    oldest.started.recv().unwrap();
    cache.invalidate_and_fence("repeated");
    let middle = start_segmented_load(cache.clone(), "repeated", "middle");
    middle.started.recv().unwrap();
    cache.invalidate_and_fence("repeated");
    let newest = start_segmented_load(cache.clone(), "repeated", "newest");
    newest.started.recv().unwrap();

    middle.release.send(()).unwrap();
    assert_eq!(middle.handle.join().unwrap(), "middle");
    assert_eq!(cache.get("repeated"), None);
    oldest.release.send(()).unwrap();
    assert_eq!(oldest.handle.join().unwrap(), "oldest");
    assert_eq!(cache.get("repeated"), None);
    newest.release.send(()).unwrap();
    assert_eq!(newest.handle.join().unwrap(), "newest");
    assert_eq!(cache.get("repeated").as_deref(), Some("newest"));

    assert_segmented_key_state_is_reclaimable(SegmentedCache::new(512, 4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_fence_separates_groups_and_preserves_the_new_value() {
    let cache = moka::future::Cache::new(100);
    let old_cache = cache.clone();
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let old = tokio::spawn(async move {
        old_cache
            .get_with("key".to_owned(), async move {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                "stale".to_owned()
            })
            .await
    });
    started.await.unwrap();

    let follower_polled = Arc::new(AtomicBool::new(false));
    let follower_marker = Arc::clone(&follower_polled);
    let mut follower = Box::pin(cache.get_with("key".to_owned(), async move {
        follower_marker.store(true, Ordering::SeqCst);
        "unexpected".to_owned()
    }));
    assert!(matches!(futures_util::poll!(&mut follower), Poll::Pending));
    assert!(!follower_polled.load(Ordering::SeqCst));

    cache.invalidate_and_fence("key").await;
    assert_eq!(
        cache
            .get_with("key".to_owned(), async { "fresh".to_owned() })
            .await,
        "fresh"
    );
    release.send(()).unwrap();

    assert_eq!(old.await.unwrap(), "stale");
    assert_eq!(follower.await, "stale");
    assert_eq!(cache.get("key").await.as_deref(), Some("fresh"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(deprecated)]
async fn future_direct_insert_wins_over_a_prefence_initializer() {
    let cache = moka::future::Cache::new(100);
    let old_cache = cache.clone();
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let old = tokio::spawn(async move {
        old_cache
            .get_with("key".to_owned(), async move {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                "stale".to_owned()
            })
            .await
    });
    started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    cache.insert("key".to_owned(), "direct".to_owned()).await;
    release.send(()).unwrap();

    assert_eq!(old.await.unwrap(), "stale");
    assert_eq!(cache.get("key").await.as_deref(), Some("direct"));

    let (weight_started_tx, weight_started) = oneshot::channel();
    let weight_started_tx = Arc::new(Mutex::new(Some(weight_started_tx)));
    let (weight_release, weight_release_rx) = std::sync::mpsc::channel();
    let weight_release_rx = Arc::new(Mutex::new(weight_release_rx));
    let cache = moka::future::Cache::<String, String>::builder()
        .max_capacity(100)
        .weigher({
            let weight_started_tx = Arc::clone(&weight_started_tx);
            let weight_release_rx = Arc::clone(&weight_release_rx);
            move |_, value| {
                if value == "racing insert" {
                    weight_started_tx
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .send(())
                        .unwrap();
                    weight_release_rx.lock().unwrap().recv().unwrap();
                }
                1
            }
        })
        .async_eviction_listener(|_, _, _| Box::pin(async {}))
        .build();
    cache.insert("key".to_owned(), "old".to_owned()).await;

    let (initializer_started_tx, initializer_started) = oneshot::channel();
    let (initializer_release, initializer_release_rx) = oneshot::channel();
    let initializer_cache = cache.clone();
    let initializer = tokio::spawn(async move {
        initializer_cache
            .get_with_if(
                "key".to_owned(),
                async move {
                    initializer_started_tx.send(()).unwrap();
                    initializer_release_rx.await.unwrap();
                    "stale initializer".to_owned()
                },
                |_| true,
            )
            .await
    });
    initializer_started.await.unwrap();

    let insert_cache = cache.clone();
    let insert = tokio::spawn(async move {
        insert_cache
            .insert("key".to_owned(), "racing insert".to_owned())
            .await;
    });
    weight_started.await.unwrap();

    let mut fence = Box::pin(cache.invalidate_and_fence("key"));
    let fence_was_pending = matches!(futures_util::poll!(&mut fence), Poll::Pending);

    weight_release.send(()).unwrap();
    if fence_was_pending {
        (&mut fence).await;
    }
    drop(fence);

    insert.await.unwrap();
    initializer_release.send(()).unwrap();
    initializer.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_future_invalidate_keeps_its_existing_inflight_behavior() {
    let cache = moka::future::Cache::new(100);
    let load_cache = cache.clone();
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let load = tokio::spawn(async move {
        load_cache
            .get_with("key".to_owned(), async move {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                "loaded".to_owned()
            })
            .await
    });
    started.await.unwrap();

    cache.invalidate("key").await;
    release.send(()).unwrap();

    assert_eq!(load.await.unwrap(), "loaded");
    assert_eq!(cache.get("key").await.as_deref(), Some("loaded"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_entry_initialization_is_fenced() {
    let cache = moka::future::Cache::new(100);
    let entry_cache = cache.clone();
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let entry = tokio::spawn(async move {
        entry_cache
            .entry("key".to_owned())
            .or_insert_with(async move {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                "stale entry".to_owned()
            })
            .await
            .into_value()
    });
    started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    release.send(()).unwrap();

    assert_eq!(entry.await.unwrap(), "stale entry");
    assert_eq!(cache.get("key").await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_fence_does_not_cross_equal_hashes() {
    let cache: moka::future::Cache<CollisionKey, String> = moka::future::Cache::new(100);
    let key_a = CollisionKey("a");
    let key_b = CollisionKey("b");

    let cache_a = cache.clone();
    let load_key_a = key_a.clone();
    let (a_started_tx, a_started) = oneshot::channel();
    let (a_release, a_release_rx) = oneshot::channel();
    let load_a = tokio::spawn(async move {
        cache_a
            .get_with(load_key_a, async move {
                a_started_tx.send(()).unwrap();
                a_release_rx.await.unwrap();
                "stale a".to_owned()
            })
            .await
    });

    let cache_b = cache.clone();
    let load_key_b = key_b.clone();
    let (b_started_tx, b_started) = oneshot::channel();
    let (b_release, b_release_rx) = oneshot::channel();
    let load_b = tokio::spawn(async move {
        cache_b
            .get_with(load_key_b, async move {
                b_started_tx.send(()).unwrap();
                b_release_rx.await.unwrap();
                "value b".to_owned()
            })
            .await
    });

    a_started.await.unwrap();
    b_started.await.unwrap();
    cache.invalidate_and_fence(&key_a).await;
    a_release.send(()).unwrap();
    b_release.send(()).unwrap();

    assert_eq!(load_a.await.unwrap(), "stale a");
    assert_eq!(load_b.await.unwrap(), "value b");
    assert_eq!(cache.get(&key_a).await, None);
    assert_eq!(cache.get(&key_b).await.as_deref(), Some("value b"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_optional_and_fallible_initializers_are_fenced() {
    let cache = moka::future::Cache::new(100);
    let optional_cache = cache.clone();
    let (optional_started_tx, optional_started) = oneshot::channel();
    let (optional_release, optional_release_rx) = oneshot::channel();
    let optional = tokio::spawn(async move {
        optional_cache
            .optionally_get_with("optional".to_owned(), async move {
                optional_started_tx.send(()).unwrap();
                optional_release_rx.await.unwrap();
                Some("stale optional".to_owned())
            })
            .await
    });
    optional_started.await.unwrap();
    cache.invalidate_and_fence("optional").await;
    optional_release.send(()).unwrap();
    assert_eq!(optional.await.unwrap().as_deref(), Some("stale optional"));
    assert_eq!(cache.get("optional").await, None);

    let try_cache = cache.clone();
    let (try_started_tx, try_started) = oneshot::channel();
    let (try_release, try_release_rx) = oneshot::channel();
    let fallible = tokio::spawn(async move {
        try_cache
            .try_get_with("fallible".to_owned(), async move {
                try_started_tx.send(()).unwrap();
                try_release_rx.await.unwrap();
                Ok::<_, &'static str>("stale fallible".to_owned())
            })
            .await
    });
    try_started.await.unwrap();
    cache.invalidate_and_fence("fallible").await;
    try_release.send(()).unwrap();
    assert_eq!(fallible.await.unwrap().unwrap().as_str(), "stale fallible");
    assert_eq!(cache.get("fallible").await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_get_with_if_initialization_is_fenced() {
    let cache = moka::future::Cache::new(100);
    cache.insert("key".to_owned(), "old".to_owned()).await;
    let load_cache = cache.clone();
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let load = tokio::spawn(async move {
        #[allow(deprecated)]
        load_cache
            .get_with_if(
                "key".to_owned(),
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    "stale replacement".to_owned()
                },
                |_| true,
            )
            .await
    });
    started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    release.send(()).unwrap();

    assert_eq!(load.await.unwrap(), "stale replacement");
    assert_eq!(cache.get("key").await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_prefence_none_and_error_do_not_poison_later_initialization() {
    let cache = moka::future::Cache::new(100);
    let none_cache = cache.clone();
    let (none_started_tx, none_started) = oneshot::channel();
    let (none_release, none_release_rx) = oneshot::channel();
    let none = tokio::spawn(async move {
        none_cache
            .optionally_get_with("none".to_owned(), async move {
                none_started_tx.send(()).unwrap();
                none_release_rx.await.unwrap();
                None
            })
            .await
    });
    none_started.await.unwrap();
    cache.invalidate_and_fence("none").await;
    none_release.send(()).unwrap();
    assert_eq!(none.await.unwrap(), None);
    assert_eq!(
        cache
            .get_with("none".to_owned(), async { "after none".to_owned() })
            .await,
        "after none"
    );

    let error_cache = cache.clone();
    let (error_started_tx, error_started) = oneshot::channel();
    let (error_release, error_release_rx) = oneshot::channel();
    let error = tokio::spawn(async move {
        error_cache
            .try_get_with("error".to_owned(), async move {
                error_started_tx.send(()).unwrap();
                error_release_rx.await.unwrap();
                Err::<String, _>("expected")
            })
            .await
    });
    error_started.await.unwrap();
    cache.invalidate_and_fence("error").await;
    error_release.send(()).unwrap();
    assert_eq!(*error.await.unwrap().unwrap_err(), "expected");
    assert_eq!(
        cache
            .get_with("error".to_owned(), async { "after error".to_owned() })
            .await,
        "after error"
    );

    let panic_cache = cache.clone();
    let (panic_started_tx, panic_started) = oneshot::channel();
    let (panic_release, panic_release_rx) = oneshot::channel();
    let panicking = tokio::spawn(async move {
        panic_cache
            .get_with("panic".to_owned(), async move {
                panic_started_tx.send(()).unwrap();
                panic_release_rx.await.unwrap();
                panic!("expected initializer panic");
            })
            .await
    });
    panic_started.await.unwrap();
    cache.invalidate_and_fence("panic").await;
    panic_release.send(()).unwrap();
    assert!(panicking.await.unwrap_err().is_panic());
    assert_eq!(
        cache
            .get_with("panic".to_owned(), async { "after panic".to_owned() })
            .await,
        "after panic"
    );

    assert_future_key_state_is_reclaimable(moka::future::Cache::new(512)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_prefence_future_does_not_poison_later_initialization() {
    let cache = moka::future::Cache::new(100);
    let old_cache = cache.clone();
    let (started_tx, started) = oneshot::channel();
    let old = tokio::spawn(async move {
        old_cache
            .get_with("key".to_owned(), async move {
                started_tx.send(()).unwrap();
                std::future::pending::<String>().await
            })
            .await
    });
    started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    old.abort();
    assert!(old.await.unwrap_err().is_cancelled());
    assert_eq!(
        cache
            .get_with_by_ref("key", async { "recovered".to_owned() })
            .await,
        "recovered"
    );
}

#[test]
fn sync_repeated_fences_separate_three_inflight_generations() {
    let cache = SyncCache::new(100);
    let oldest = start_sync_load(cache.clone(), "key", "oldest");
    oldest.started.recv().unwrap();

    cache.invalidate_and_fence("key");
    let middle = start_sync_load(cache.clone(), "key", "middle");
    middle.started.recv().unwrap();

    cache.invalidate_and_fence("key");
    let newest = start_sync_load(cache.clone(), "key", "newest");
    newest.started.recv().unwrap();

    middle.release.send(()).unwrap();
    assert_eq!(middle.handle.join().unwrap(), "middle");
    assert_eq!(cache.get("key"), None);

    oldest.release.send(()).unwrap();
    assert_eq!(oldest.handle.join().unwrap(), "oldest");
    assert_eq!(cache.get("key"), None);

    newest.release.send(()).unwrap();
    assert_eq!(newest.handle.join().unwrap(), "newest");
    assert_eq!(cache.get("key").as_deref(), Some("newest"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_repeated_fences_separate_three_inflight_generations() {
    let cache = moka::future::Cache::new(100);
    let oldest = start_future_load(cache.clone(), "key", "oldest");
    oldest.started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    let middle = start_future_load(cache.clone(), "key", "middle");
    middle.started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    let newest = start_future_load(cache.clone(), "key", "newest");
    newest.started.await.unwrap();

    middle.release.send(()).unwrap();
    assert_eq!(middle.handle.await.unwrap(), "middle");
    assert_eq!(cache.get("key").await, None);

    oldest.release.send(()).unwrap();
    assert_eq!(oldest.handle.await.unwrap(), "oldest");
    assert_eq!(cache.get("key").await, None);

    newest.release.send(()).unwrap();
    assert_eq!(newest.handle.await.unwrap(), "newest");
    assert_eq!(cache.get("key").await.as_deref(), Some("newest"));
}

#[test]
fn sync_one_fence_covers_concurrent_initializer_families() {
    let cache = SyncCache::new(100);
    let plain = start_sync_load(cache.clone(), "key", "plain");
    plain.started.recv().unwrap();

    let optional_cache = cache.clone();
    let (optional_started_tx, optional_started) = sync_channel(0);
    let (optional_release, optional_release_rx) = sync_channel(0);
    let optional = thread::spawn(move || {
        optional_cache.optionally_get_with("key".to_owned(), || {
            optional_started_tx.send(()).unwrap();
            optional_release_rx.recv().unwrap();
            Some("optional".to_owned())
        })
    });
    optional_started.recv().unwrap();

    let fallible_cache = cache.clone();
    let (fallible_started_tx, fallible_started) = sync_channel(0);
    let (fallible_release, fallible_release_rx) = sync_channel(0);
    let fallible = thread::spawn(move || {
        fallible_cache.try_get_with("key".to_owned(), || {
            fallible_started_tx.send(()).unwrap();
            fallible_release_rx.recv().unwrap();
            Ok::<_, &'static str>("fallible".to_owned())
        })
    });
    fallible_started.recv().unwrap();

    cache.invalidate_and_fence("key");
    assert_eq!(
        cache.get_with("key".to_owned(), || "fresh".to_owned()),
        "fresh"
    );

    optional_release.send(()).unwrap();
    fallible_release.send(()).unwrap();
    plain.release.send(()).unwrap();
    assert_eq!(optional.join().unwrap().as_deref(), Some("optional"));
    assert_eq!(fallible.join().unwrap().unwrap(), "fallible");
    assert_eq!(plain.handle.join().unwrap(), "plain");
    assert_eq!(cache.get("key").as_deref(), Some("fresh"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn future_one_fence_covers_concurrent_initializer_families() {
    let cache = moka::future::Cache::new(100);
    let plain = start_future_load(cache.clone(), "key", "plain");
    plain.started.await.unwrap();

    let optional_cache = cache.clone();
    let (optional_started_tx, optional_started) = oneshot::channel();
    let (optional_release, optional_release_rx) = oneshot::channel();
    let optional = tokio::spawn(async move {
        optional_cache
            .optionally_get_with("key".to_owned(), async move {
                optional_started_tx.send(()).unwrap();
                optional_release_rx.await.unwrap();
                Some("optional".to_owned())
            })
            .await
    });
    optional_started.await.unwrap();

    let fallible_cache = cache.clone();
    let (fallible_started_tx, fallible_started) = oneshot::channel();
    let (fallible_release, fallible_release_rx) = oneshot::channel();
    let fallible = tokio::spawn(async move {
        fallible_cache
            .try_get_with("key".to_owned(), async move {
                fallible_started_tx.send(()).unwrap();
                fallible_release_rx.await.unwrap();
                Ok::<_, &'static str>("fallible".to_owned())
            })
            .await
    });
    fallible_started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    assert_eq!(
        cache
            .get_with("key".to_owned(), async { "fresh".to_owned() })
            .await,
        "fresh"
    );

    optional_release.send(()).unwrap();
    fallible_release.send(()).unwrap();
    plain.release.send(()).unwrap();
    assert_eq!(optional.await.unwrap().as_deref(), Some("optional"));
    assert_eq!(fallible.await.unwrap().unwrap(), "fallible");
    assert_eq!(plain.handle.await.unwrap(), "plain");
    assert_eq!(cache.get("key").await.as_deref(), Some("fresh"));
}

#[test]
fn sync_borrowed_and_entry_variant_initializers_are_fenced() {
    let cache = SyncCache::new(100);

    let borrowed_cache = cache.clone();
    let (borrowed_started_tx, borrowed_started) = sync_channel(0);
    let (borrowed_release, borrowed_release_rx) = sync_channel(0);
    let borrowed = thread::spawn(move || {
        borrowed_cache.get_with_by_ref("borrowed", || {
            borrowed_started_tx.send(()).unwrap();
            borrowed_release_rx.recv().unwrap();
            "borrowed value".to_owned()
        })
    });
    borrowed_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed");
    borrowed_release.send(()).unwrap();
    assert_eq!(borrowed.join().unwrap(), "borrowed value");
    assert_eq!(cache.get("borrowed"), None);

    let borrowed_entry_cache = cache.clone();
    let (borrowed_entry_started_tx, borrowed_entry_started) = sync_channel(0);
    let (borrowed_entry_release, borrowed_entry_release_rx) = sync_channel(0);
    let borrowed_entry = thread::spawn(move || {
        borrowed_entry_cache
            .entry_by_ref("borrowed plain entry")
            .or_insert_with(|| {
                borrowed_entry_started_tx.send(()).unwrap();
                borrowed_entry_release_rx.recv().unwrap();
                "borrowed plain entry value".to_owned()
            })
            .into_value()
    });
    borrowed_entry_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed plain entry");
    borrowed_entry_release.send(()).unwrap();
    assert_eq!(borrowed_entry.join().unwrap(), "borrowed plain entry value");
    assert_eq!(cache.get("borrowed plain entry"), None);

    let borrowed_optional_cache = cache.clone();
    let (borrowed_optional_started_tx, borrowed_optional_started) = sync_channel(0);
    let (borrowed_optional_release, borrowed_optional_release_rx) = sync_channel(0);
    let borrowed_optional = thread::spawn(move || {
        borrowed_optional_cache.optionally_get_with_by_ref("borrowed optional", || {
            borrowed_optional_started_tx.send(()).unwrap();
            borrowed_optional_release_rx.recv().unwrap();
            Some("borrowed optional value".to_owned())
        })
    });
    borrowed_optional_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed optional");
    borrowed_optional_release.send(()).unwrap();
    assert_eq!(
        borrowed_optional.join().unwrap().as_deref(),
        Some("borrowed optional value")
    );
    assert_eq!(cache.get("borrowed optional"), None);

    let borrowed_fallible_cache = cache.clone();
    let (borrowed_fallible_started_tx, borrowed_fallible_started) = sync_channel(0);
    let (borrowed_fallible_release, borrowed_fallible_release_rx) = sync_channel(0);
    let borrowed_fallible = thread::spawn(move || {
        borrowed_fallible_cache.try_get_with_by_ref("borrowed fallible", || {
            borrowed_fallible_started_tx.send(()).unwrap();
            borrowed_fallible_release_rx.recv().unwrap();
            Ok::<_, &'static str>("borrowed fallible value".to_owned())
        })
    });
    borrowed_fallible_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed fallible");
    borrowed_fallible_release.send(()).unwrap();
    assert_eq!(
        borrowed_fallible.join().unwrap().unwrap(),
        "borrowed fallible value"
    );
    assert_eq!(cache.get("borrowed fallible"), None);

    cache.insert("conditional entry".to_owned(), "old".to_owned());
    let conditional_entry_cache = cache.clone();
    let (conditional_entry_started_tx, conditional_entry_started) = sync_channel(0);
    let (conditional_entry_release, conditional_entry_release_rx) = sync_channel(0);
    let conditional_entry = thread::spawn(move || {
        conditional_entry_cache
            .entry_by_ref("conditional entry")
            .or_insert_with_if(
                || {
                    conditional_entry_started_tx.send(()).unwrap();
                    conditional_entry_release_rx.recv().unwrap();
                    "conditional entry value".to_owned()
                },
                |_| true,
            )
            .into_value()
    });
    conditional_entry_started.recv().unwrap();
    cache.invalidate_and_fence("conditional entry");
    conditional_entry_release.send(()).unwrap();
    assert_eq!(conditional_entry.join().unwrap(), "conditional entry value");
    assert_eq!(cache.get("conditional entry"), None);

    let optional_cache = cache.clone();
    let (optional_started_tx, optional_started) = sync_channel(0);
    let (optional_release, optional_release_rx) = sync_channel(0);
    let optional = thread::spawn(move || {
        optional_cache
            .entry_by_ref("optional entry")
            .or_optionally_insert_with(|| {
                optional_started_tx.send(()).unwrap();
                optional_release_rx.recv().unwrap();
                Some("optional entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    optional_started.recv().unwrap();
    cache.invalidate_and_fence("optional entry");
    optional_release.send(()).unwrap();
    assert_eq!(
        optional.join().unwrap().as_deref(),
        Some("optional entry value")
    );
    assert_eq!(cache.get("optional entry"), None);

    let fallible_cache = cache.clone();
    let (fallible_started_tx, fallible_started) = sync_channel(0);
    let (fallible_release, fallible_release_rx) = sync_channel(0);
    let fallible = thread::spawn(move || {
        fallible_cache
            .entry_by_ref("fallible entry")
            .or_try_insert_with(|| {
                fallible_started_tx.send(()).unwrap();
                fallible_release_rx.recv().unwrap();
                Ok::<_, &'static str>("fallible entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    fallible_started.recv().unwrap();
    cache.invalidate_and_fence("fallible entry");
    fallible_release.send(()).unwrap();
    assert_eq!(fallible.join().unwrap().unwrap(), "fallible entry value");
    assert_eq!(cache.get("fallible entry"), None);

    cache.insert("owned conditional entry".to_owned(), "old".to_owned());
    let owned_conditional_cache = cache.clone();
    let (owned_conditional_started_tx, owned_conditional_started) = sync_channel(0);
    let (owned_conditional_release, owned_conditional_release_rx) = sync_channel(0);
    let owned_conditional = thread::spawn(move || {
        owned_conditional_cache
            .entry("owned conditional entry".to_owned())
            .or_insert_with_if(
                || {
                    owned_conditional_started_tx.send(()).unwrap();
                    owned_conditional_release_rx.recv().unwrap();
                    "owned conditional entry value".to_owned()
                },
                |_| true,
            )
            .into_value()
    });
    owned_conditional_started.recv().unwrap();
    cache.invalidate_and_fence("owned conditional entry");
    owned_conditional_release.send(()).unwrap();
    assert_eq!(
        owned_conditional.join().unwrap(),
        "owned conditional entry value"
    );
    assert_eq!(cache.get("owned conditional entry"), None);

    let owned_optional_cache = cache.clone();
    let (owned_optional_started_tx, owned_optional_started) = sync_channel(0);
    let (owned_optional_release, owned_optional_release_rx) = sync_channel(0);
    let owned_optional = thread::spawn(move || {
        owned_optional_cache
            .entry("owned optional entry".to_owned())
            .or_optionally_insert_with(|| {
                owned_optional_started_tx.send(()).unwrap();
                owned_optional_release_rx.recv().unwrap();
                Some("owned optional entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    owned_optional_started.recv().unwrap();
    cache.invalidate_and_fence("owned optional entry");
    owned_optional_release.send(()).unwrap();
    assert_eq!(
        owned_optional.join().unwrap().as_deref(),
        Some("owned optional entry value")
    );
    assert_eq!(cache.get("owned optional entry"), None);

    let owned_fallible_cache = cache.clone();
    let (owned_fallible_started_tx, owned_fallible_started) = sync_channel(0);
    let (owned_fallible_release, owned_fallible_release_rx) = sync_channel(0);
    let owned_fallible = thread::spawn(move || {
        owned_fallible_cache
            .entry("owned fallible entry".to_owned())
            .or_try_insert_with(|| {
                owned_fallible_started_tx.send(()).unwrap();
                owned_fallible_release_rx.recv().unwrap();
                Ok::<_, &'static str>("owned fallible entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    owned_fallible_started.recv().unwrap();
    cache.invalidate_and_fence("owned fallible entry");
    owned_fallible_release.send(()).unwrap();
    assert_eq!(
        owned_fallible.join().unwrap().unwrap(),
        "owned fallible entry value"
    );
    assert_eq!(cache.get("owned fallible entry"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_borrowed_and_entry_variant_initializers_are_fenced() {
    let cache = moka::future::Cache::new(100);

    let borrowed_cache = cache.clone();
    let (borrowed_started_tx, borrowed_started) = oneshot::channel();
    let (borrowed_release, borrowed_release_rx) = oneshot::channel();
    let borrowed = tokio::spawn(async move {
        borrowed_cache
            .get_with_by_ref("borrowed", async move {
                borrowed_started_tx.send(()).unwrap();
                borrowed_release_rx.await.unwrap();
                "borrowed value".to_owned()
            })
            .await
    });
    borrowed_started.await.unwrap();
    cache.invalidate_and_fence("borrowed").await;
    borrowed_release.send(()).unwrap();
    assert_eq!(borrowed.await.unwrap(), "borrowed value");
    assert_eq!(cache.get("borrowed").await, None);

    let borrowed_entry_cache = cache.clone();
    let (borrowed_entry_started_tx, borrowed_entry_started) = oneshot::channel();
    let (borrowed_entry_release, borrowed_entry_release_rx) = oneshot::channel();
    let borrowed_entry = tokio::spawn(async move {
        borrowed_entry_cache
            .entry_by_ref("borrowed plain entry")
            .or_insert_with(async move {
                borrowed_entry_started_tx.send(()).unwrap();
                borrowed_entry_release_rx.await.unwrap();
                "borrowed plain entry value".to_owned()
            })
            .await
            .into_value()
    });
    borrowed_entry_started.await.unwrap();
    cache.invalidate_and_fence("borrowed plain entry").await;
    borrowed_entry_release.send(()).unwrap();
    assert_eq!(borrowed_entry.await.unwrap(), "borrowed plain entry value");
    assert_eq!(cache.get("borrowed plain entry").await, None);

    let borrowed_optional_cache = cache.clone();
    let (borrowed_optional_started_tx, borrowed_optional_started) = oneshot::channel();
    let (borrowed_optional_release, borrowed_optional_release_rx) = oneshot::channel();
    let borrowed_optional = tokio::spawn(async move {
        borrowed_optional_cache
            .optionally_get_with_by_ref("borrowed optional", async move {
                borrowed_optional_started_tx.send(()).unwrap();
                borrowed_optional_release_rx.await.unwrap();
                Some("borrowed optional value".to_owned())
            })
            .await
    });
    borrowed_optional_started.await.unwrap();
    cache.invalidate_and_fence("borrowed optional").await;
    borrowed_optional_release.send(()).unwrap();
    assert_eq!(
        borrowed_optional.await.unwrap().as_deref(),
        Some("borrowed optional value")
    );
    assert_eq!(cache.get("borrowed optional").await, None);

    let borrowed_fallible_cache = cache.clone();
    let (borrowed_fallible_started_tx, borrowed_fallible_started) = oneshot::channel();
    let (borrowed_fallible_release, borrowed_fallible_release_rx) = oneshot::channel();
    let borrowed_fallible = tokio::spawn(async move {
        borrowed_fallible_cache
            .try_get_with_by_ref("borrowed fallible", async move {
                borrowed_fallible_started_tx.send(()).unwrap();
                borrowed_fallible_release_rx.await.unwrap();
                Ok::<_, &'static str>("borrowed fallible value".to_owned())
            })
            .await
    });
    borrowed_fallible_started.await.unwrap();
    cache.invalidate_and_fence("borrowed fallible").await;
    borrowed_fallible_release.send(()).unwrap();
    assert_eq!(
        borrowed_fallible.await.unwrap().unwrap(),
        "borrowed fallible value"
    );
    assert_eq!(cache.get("borrowed fallible").await, None);

    cache
        .insert("conditional entry".to_owned(), "old".to_owned())
        .await;
    let conditional_entry_cache = cache.clone();
    let (conditional_entry_started_tx, conditional_entry_started) = oneshot::channel();
    let (conditional_entry_release, conditional_entry_release_rx) = oneshot::channel();
    let conditional_entry = tokio::spawn(async move {
        conditional_entry_cache
            .entry_by_ref("conditional entry")
            .or_insert_with_if(
                async move {
                    conditional_entry_started_tx.send(()).unwrap();
                    conditional_entry_release_rx.await.unwrap();
                    "conditional entry value".to_owned()
                },
                |_| true,
            )
            .await
            .into_value()
    });
    conditional_entry_started.await.unwrap();
    cache.invalidate_and_fence("conditional entry").await;
    conditional_entry_release.send(()).unwrap();
    assert_eq!(conditional_entry.await.unwrap(), "conditional entry value");
    assert_eq!(cache.get("conditional entry").await, None);

    let optional_cache = cache.clone();
    let (optional_started_tx, optional_started) = oneshot::channel();
    let (optional_release, optional_release_rx) = oneshot::channel();
    let optional = tokio::spawn(async move {
        optional_cache
            .entry_by_ref("optional entry")
            .or_optionally_insert_with(async move {
                optional_started_tx.send(()).unwrap();
                optional_release_rx.await.unwrap();
                Some("optional entry value".to_owned())
            })
            .await
            .map(|entry| entry.into_value())
    });
    optional_started.await.unwrap();
    cache.invalidate_and_fence("optional entry").await;
    optional_release.send(()).unwrap();
    assert_eq!(
        optional.await.unwrap().as_deref(),
        Some("optional entry value")
    );
    assert_eq!(cache.get("optional entry").await, None);

    let fallible_cache = cache.clone();
    let (fallible_started_tx, fallible_started) = oneshot::channel();
    let (fallible_release, fallible_release_rx) = oneshot::channel();
    let fallible = tokio::spawn(async move {
        fallible_cache
            .entry_by_ref("fallible entry")
            .or_try_insert_with(async move {
                fallible_started_tx.send(()).unwrap();
                fallible_release_rx.await.unwrap();
                Ok::<_, &'static str>("fallible entry value".to_owned())
            })
            .await
            .map(|entry| entry.into_value())
    });
    fallible_started.await.unwrap();
    cache.invalidate_and_fence("fallible entry").await;
    fallible_release.send(()).unwrap();
    assert_eq!(fallible.await.unwrap().unwrap(), "fallible entry value");
    assert_eq!(cache.get("fallible entry").await, None);

    cache
        .insert("owned conditional entry".to_owned(), "old".to_owned())
        .await;
    let owned_conditional_cache = cache.clone();
    let (owned_conditional_started_tx, owned_conditional_started) = oneshot::channel();
    let (owned_conditional_release, owned_conditional_release_rx) = oneshot::channel();
    let owned_conditional = tokio::spawn(async move {
        owned_conditional_cache
            .entry("owned conditional entry".to_owned())
            .or_insert_with_if(
                async move {
                    owned_conditional_started_tx.send(()).unwrap();
                    owned_conditional_release_rx.await.unwrap();
                    "owned conditional entry value".to_owned()
                },
                |_| true,
            )
            .await
            .into_value()
    });
    owned_conditional_started.await.unwrap();
    cache.invalidate_and_fence("owned conditional entry").await;
    owned_conditional_release.send(()).unwrap();
    assert_eq!(
        owned_conditional.await.unwrap(),
        "owned conditional entry value"
    );
    assert_eq!(cache.get("owned conditional entry").await, None);

    let owned_optional_cache = cache.clone();
    let (owned_optional_started_tx, owned_optional_started) = oneshot::channel();
    let (owned_optional_release, owned_optional_release_rx) = oneshot::channel();
    let owned_optional = tokio::spawn(async move {
        owned_optional_cache
            .entry("owned optional entry".to_owned())
            .or_optionally_insert_with(async move {
                owned_optional_started_tx.send(()).unwrap();
                owned_optional_release_rx.await.unwrap();
                Some("owned optional entry value".to_owned())
            })
            .await
            .map(|entry| entry.into_value())
    });
    owned_optional_started.await.unwrap();
    cache.invalidate_and_fence("owned optional entry").await;
    owned_optional_release.send(()).unwrap();
    assert_eq!(
        owned_optional.await.unwrap().as_deref(),
        Some("owned optional entry value")
    );
    assert_eq!(cache.get("owned optional entry").await, None);

    let owned_fallible_cache = cache.clone();
    let (owned_fallible_started_tx, owned_fallible_started) = oneshot::channel();
    let (owned_fallible_release, owned_fallible_release_rx) = oneshot::channel();
    let owned_fallible = tokio::spawn(async move {
        owned_fallible_cache
            .entry("owned fallible entry".to_owned())
            .or_try_insert_with(async move {
                owned_fallible_started_tx.send(()).unwrap();
                owned_fallible_release_rx.await.unwrap();
                Ok::<_, &'static str>("owned fallible entry value".to_owned())
            })
            .await
            .map(|entry| entry.into_value())
    });
    owned_fallible_started.await.unwrap();
    cache.invalidate_and_fence("owned fallible entry").await;
    owned_fallible_release.send(()).unwrap();
    assert_eq!(
        owned_fallible.await.unwrap().unwrap(),
        "owned fallible entry value"
    );
    assert_eq!(cache.get("owned fallible entry").await, None);
}

#[test]
#[allow(deprecated)]
fn segmented_cache_fences_borrowed_conditional_and_entry_initializers() {
    let cache = SegmentedCache::new(100, 4);

    let borrowed_cache = cache.clone();
    let (borrowed_started_tx, borrowed_started) = sync_channel(0);
    let (borrowed_release, borrowed_release_rx) = sync_channel(0);
    let borrowed = thread::spawn(move || {
        borrowed_cache.get_with_by_ref("borrowed", || {
            borrowed_started_tx.send(()).unwrap();
            borrowed_release_rx.recv().unwrap();
            "borrowed value".to_owned()
        })
    });
    borrowed_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed");
    borrowed_release.send(()).unwrap();
    assert_eq!(borrowed.join().unwrap(), "borrowed value");
    assert_eq!(cache.get("borrowed"), None);

    let owned_entry_cache = cache.clone();
    let (owned_entry_started_tx, owned_entry_started) = sync_channel(0);
    let (owned_entry_release, owned_entry_release_rx) = sync_channel(0);
    let owned_entry = thread::spawn(move || {
        owned_entry_cache
            .entry("owned plain entry".to_owned())
            .or_insert_with(|| {
                owned_entry_started_tx.send(()).unwrap();
                owned_entry_release_rx.recv().unwrap();
                "owned plain entry value".to_owned()
            })
            .into_value()
    });
    owned_entry_started.recv().unwrap();
    cache.invalidate_and_fence("owned plain entry");
    owned_entry_release.send(()).unwrap();
    assert_eq!(owned_entry.join().unwrap(), "owned plain entry value");
    assert_eq!(cache.get("owned plain entry"), None);

    let borrowed_entry_cache = cache.clone();
    let (borrowed_entry_started_tx, borrowed_entry_started) = sync_channel(0);
    let (borrowed_entry_release, borrowed_entry_release_rx) = sync_channel(0);
    let borrowed_entry = thread::spawn(move || {
        borrowed_entry_cache
            .entry_by_ref("borrowed plain entry")
            .or_insert_with(|| {
                borrowed_entry_started_tx.send(()).unwrap();
                borrowed_entry_release_rx.recv().unwrap();
                "borrowed plain entry value".to_owned()
            })
            .into_value()
    });
    borrowed_entry_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed plain entry");
    borrowed_entry_release.send(()).unwrap();
    assert_eq!(borrowed_entry.join().unwrap(), "borrowed plain entry value");
    assert_eq!(cache.get("borrowed plain entry"), None);

    let borrowed_optional_cache = cache.clone();
    let (borrowed_optional_started_tx, borrowed_optional_started) = sync_channel(0);
    let (borrowed_optional_release, borrowed_optional_release_rx) = sync_channel(0);
    let borrowed_optional = thread::spawn(move || {
        borrowed_optional_cache.optionally_get_with_by_ref("borrowed optional", || {
            borrowed_optional_started_tx.send(()).unwrap();
            borrowed_optional_release_rx.recv().unwrap();
            Some("borrowed optional value".to_owned())
        })
    });
    borrowed_optional_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed optional");
    borrowed_optional_release.send(()).unwrap();
    assert_eq!(
        borrowed_optional.join().unwrap().as_deref(),
        Some("borrowed optional value")
    );
    assert_eq!(cache.get("borrowed optional"), None);

    let borrowed_optional_entry_cache = cache.clone();
    let (borrowed_optional_entry_started_tx, borrowed_optional_entry_started) = sync_channel(0);
    let (borrowed_optional_entry_release, borrowed_optional_entry_release_rx) = sync_channel(0);
    let borrowed_optional_entry = thread::spawn(move || {
        borrowed_optional_entry_cache
            .entry_by_ref("borrowed optional entry")
            .or_optionally_insert_with(|| {
                borrowed_optional_entry_started_tx.send(()).unwrap();
                borrowed_optional_entry_release_rx.recv().unwrap();
                Some("borrowed optional entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    borrowed_optional_entry_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed optional entry");
    borrowed_optional_entry_release.send(()).unwrap();
    assert_eq!(
        borrowed_optional_entry.join().unwrap().as_deref(),
        Some("borrowed optional entry value")
    );
    assert_eq!(cache.get("borrowed optional entry"), None);

    let borrowed_fallible_cache = cache.clone();
    let (borrowed_fallible_started_tx, borrowed_fallible_started) = sync_channel(0);
    let (borrowed_fallible_release, borrowed_fallible_release_rx) = sync_channel(0);
    let borrowed_fallible = thread::spawn(move || {
        borrowed_fallible_cache.try_get_with_by_ref("borrowed fallible", || {
            borrowed_fallible_started_tx.send(()).unwrap();
            borrowed_fallible_release_rx.recv().unwrap();
            Ok::<_, &'static str>("borrowed fallible value".to_owned())
        })
    });
    borrowed_fallible_started.recv().unwrap();
    cache.invalidate_and_fence("borrowed fallible");
    borrowed_fallible_release.send(()).unwrap();
    assert_eq!(
        borrowed_fallible.join().unwrap().unwrap(),
        "borrowed fallible value"
    );
    assert_eq!(cache.get("borrowed fallible"), None);

    cache.insert("conditional entry".to_owned(), "old".to_owned());
    let conditional_entry_cache = cache.clone();
    let (conditional_entry_started_tx, conditional_entry_started) = sync_channel(0);
    let (conditional_entry_release, conditional_entry_release_rx) = sync_channel(0);
    let conditional_entry = thread::spawn(move || {
        conditional_entry_cache
            .entry_by_ref("conditional entry")
            .or_insert_with_if(
                || {
                    conditional_entry_started_tx.send(()).unwrap();
                    conditional_entry_release_rx.recv().unwrap();
                    "conditional entry value".to_owned()
                },
                |_| true,
            )
            .into_value()
    });
    conditional_entry_started.recv().unwrap();
    cache.invalidate_and_fence("conditional entry");
    conditional_entry_release.send(()).unwrap();
    assert_eq!(conditional_entry.join().unwrap(), "conditional entry value");
    assert_eq!(cache.get("conditional entry"), None);

    cache.insert("conditional".to_owned(), "old".to_owned());
    let conditional_cache = cache.clone();
    let (conditional_started_tx, conditional_started) = sync_channel(0);
    let (conditional_release, conditional_release_rx) = sync_channel(0);
    let conditional = thread::spawn(move || {
        conditional_cache.get_with_if(
            "conditional".to_owned(),
            || {
                conditional_started_tx.send(()).unwrap();
                conditional_release_rx.recv().unwrap();
                "conditional value".to_owned()
            },
            |_| true,
        )
    });
    conditional_started.recv().unwrap();
    cache.invalidate_and_fence("conditional");
    conditional_release.send(()).unwrap();
    assert_eq!(conditional.join().unwrap(), "conditional value");
    assert_eq!(cache.get("conditional"), None);

    let entry_cache = cache.clone();
    let (entry_started_tx, entry_started) = sync_channel(0);
    let (entry_release, entry_release_rx) = sync_channel(0);
    let entry = thread::spawn(move || {
        entry_cache
            .entry_by_ref("entry")
            .or_try_insert_with(|| {
                entry_started_tx.send(()).unwrap();
                entry_release_rx.recv().unwrap();
                Ok::<_, &'static str>("entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    entry_started.recv().unwrap();
    cache.invalidate_and_fence("entry");
    entry_release.send(()).unwrap();
    assert_eq!(entry.join().unwrap().unwrap(), "entry value");
    assert_eq!(cache.get("entry"), None);

    cache.insert("owned conditional entry".to_owned(), "old".to_owned());
    let owned_conditional_cache = cache.clone();
    let (owned_conditional_started_tx, owned_conditional_started) = sync_channel(0);
    let (owned_conditional_release, owned_conditional_release_rx) = sync_channel(0);
    let owned_conditional = thread::spawn(move || {
        owned_conditional_cache
            .entry("owned conditional entry".to_owned())
            .or_insert_with_if(
                || {
                    owned_conditional_started_tx.send(()).unwrap();
                    owned_conditional_release_rx.recv().unwrap();
                    "owned conditional entry value".to_owned()
                },
                |_| true,
            )
            .into_value()
    });
    owned_conditional_started.recv().unwrap();
    cache.invalidate_and_fence("owned conditional entry");
    owned_conditional_release.send(()).unwrap();
    assert_eq!(
        owned_conditional.join().unwrap(),
        "owned conditional entry value"
    );
    assert_eq!(cache.get("owned conditional entry"), None);

    let owned_optional_cache = cache.clone();
    let (owned_optional_started_tx, owned_optional_started) = sync_channel(0);
    let (owned_optional_release, owned_optional_release_rx) = sync_channel(0);
    let owned_optional = thread::spawn(move || {
        owned_optional_cache
            .entry("owned optional entry".to_owned())
            .or_optionally_insert_with(|| {
                owned_optional_started_tx.send(()).unwrap();
                owned_optional_release_rx.recv().unwrap();
                Some("owned optional entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    owned_optional_started.recv().unwrap();
    cache.invalidate_and_fence("owned optional entry");
    owned_optional_release.send(()).unwrap();
    assert_eq!(
        owned_optional.join().unwrap().as_deref(),
        Some("owned optional entry value")
    );
    assert_eq!(cache.get("owned optional entry"), None);

    let owned_fallible_cache = cache.clone();
    let (owned_fallible_started_tx, owned_fallible_started) = sync_channel(0);
    let (owned_fallible_release, owned_fallible_release_rx) = sync_channel(0);
    let owned_fallible = thread::spawn(move || {
        owned_fallible_cache
            .entry("owned fallible entry".to_owned())
            .or_try_insert_with(|| {
                owned_fallible_started_tx.send(()).unwrap();
                owned_fallible_release_rx.recv().unwrap();
                Ok::<_, &'static str>("owned fallible entry value".to_owned())
            })
            .map(|entry| entry.into_value())
    });
    owned_fallible_started.recv().unwrap();
    cache.invalidate_and_fence("owned fallible entry");
    owned_fallible_release.send(()).unwrap();
    assert_eq!(
        owned_fallible.join().unwrap().unwrap(),
        "owned fallible entry value"
    );
    assert_eq!(cache.get("owned fallible entry"), None);

    let none_cache = cache.clone();
    let (none_started_tx, none_started) = sync_channel(0);
    let (none_release, none_release_rx) = sync_channel(0);
    let none = thread::spawn(move || {
        none_cache.optionally_get_with("none".to_owned(), || {
            none_started_tx.send(()).unwrap();
            none_release_rx.recv().unwrap();
            None
        })
    });
    none_started.recv().unwrap();
    cache.invalidate_and_fence("none");
    none_release.send(()).unwrap();
    assert_eq!(none.join().unwrap(), None);
    assert_eq!(
        cache.get_with("none".to_owned(), || "after none".to_owned()),
        "after none"
    );

    let error_cache = cache.clone();
    let (error_started_tx, error_started) = sync_channel(0);
    let (error_release, error_release_rx) = sync_channel(0);
    let error = thread::spawn(move || {
        error_cache.try_get_with("error".to_owned(), || {
            error_started_tx.send(()).unwrap();
            error_release_rx.recv().unwrap();
            Err::<String, _>("expected")
        })
    });
    error_started.recv().unwrap();
    cache.invalidate_and_fence("error");
    error_release.send(()).unwrap();
    assert_eq!(*error.join().unwrap().unwrap_err(), "expected");
    assert_eq!(
        cache.get_with("error".to_owned(), || "after error".to_owned()),
        "after error"
    );

    let panic_cache = cache.clone();
    let (panic_started_tx, panic_started) = sync_channel(0);
    let (panic_release, panic_release_rx) = sync_channel(0);
    let panicking = thread::spawn(move || {
        panic_cache.get_with("panic".to_owned(), || {
            panic_started_tx.send(()).unwrap();
            panic_release_rx.recv().unwrap();
            panic!("expected initializer panic");
        })
    });
    panic_started.recv().unwrap();
    cache.invalidate_and_fence("panic");
    panic_release.send(()).unwrap();
    assert!(panicking.join().is_err());
    assert_eq!(
        cache.get_with("panic".to_owned(), || "after panic".to_owned()),
        "after panic"
    );
}

#[test]
fn sync_caches_leave_inflight_compute_and_upsert_effects_intact() {
    // Both updates must survive in either scheduling order. The fence runs
    // while the first compute closure is known to be active.
    macro_rules! check_serialization {
        ($cache:expr) => {
            for variant in 0..3 {
                for round in 0..128 {
                    let cache = $cache;
                    let first_cache = cache.clone();
                    let (entered_tx, entered) = sync_channel(0);
                    let start = Arc::new(Barrier::new(2));
                    let first_start = Arc::clone(&start);
                    let first = thread::spawn(move || {
                        first_cache.entry(0).and_compute_with(|entry| {
                            let value = entry.map_or(0, |entry| *entry.value());
                            entered_tx.send(()).unwrap();
                            first_start.wait();
                            Op::Put(value + 1)
                        });
                    });
                    entered.recv().unwrap();
                    cache.invalidate_and_fence(&0);
                    let second_cache = cache.clone();
                    let second = thread::spawn(move || {
                        start.wait();
                        match variant {
                            0 => {
                                second_cache.entry(0).and_compute_with(|entry| {
                                    Op::Put(entry.map_or(0, |entry| *entry.value()) + 1)
                                });
                            }
                            1 => {
                                second_cache
                                    .entry(0)
                                    .and_try_compute_with(|entry| {
                                        Ok::<_, ()>(Op::Put(
                                            entry.map_or(0, |entry| *entry.value()) + 1,
                                        ))
                                    })
                                    .unwrap();
                            }
                            _ => {
                                second_cache.entry(0).and_upsert_with(|entry| {
                                    entry.map_or(0, |entry| *entry.value()) + 1
                                });
                            }
                        }
                    });
                    first.join().unwrap();
                    second.join().unwrap();
                    assert_eq!(
                        cache.get(&0),
                        Some(2),
                        "variant {variant}, round {round}: compute updates were not serialized"
                    );
                }
            }
        };
    }
    check_serialization!(SyncCache::<usize, usize>::new(100));
    check_serialization!(SegmentedCache::<usize, usize>::new(100, 4));

    let cache = SyncCache::new(100);

    let compute_cache = cache.clone();
    let (compute_started_tx, compute_started) = sync_channel(0);
    let (compute_release, compute_release_rx) = sync_channel(0);
    let compute = thread::spawn(move || {
        compute_cache
            .entry("compute".to_owned())
            .and_compute_with(|_| {
                compute_started_tx.send(()).unwrap();
                compute_release_rx.recv().unwrap();
                Op::Put("computed".to_owned())
            });
    });
    compute_started.recv().unwrap();
    cache.invalidate_and_fence("compute");
    compute_release.send(()).unwrap();
    compute.join().unwrap();
    assert_eq!(cache.get("compute").as_deref(), Some("computed"));

    let try_compute_cache = cache.clone();
    let (try_compute_started_tx, try_compute_started) = sync_channel(0);
    let (try_compute_release, try_compute_release_rx) = sync_channel(0);
    let try_compute = thread::spawn(move || {
        try_compute_cache
            .entry("try compute".to_owned())
            .and_try_compute_with(|_| {
                try_compute_started_tx.send(()).unwrap();
                try_compute_release_rx.recv().unwrap();
                Ok::<_, &'static str>(Op::Put("try computed".to_owned()))
            })
            .unwrap();
    });
    try_compute_started.recv().unwrap();
    cache.invalidate_and_fence("try compute");
    try_compute_release.send(()).unwrap();
    try_compute.join().unwrap();
    assert_eq!(cache.get("try compute").as_deref(), Some("try computed"));

    let upsert_cache = cache.clone();
    let (upsert_started_tx, upsert_started) = sync_channel(0);
    let (upsert_release, upsert_release_rx) = sync_channel(0);
    let upsert = thread::spawn(move || {
        upsert_cache
            .entry("upsert".to_owned())
            .and_upsert_with(|_| {
                upsert_started_tx.send(()).unwrap();
                upsert_release_rx.recv().unwrap();
                "upserted".to_owned()
            });
    });
    upsert_started.recv().unwrap();
    cache.invalidate_and_fence("upsert");
    upsert_release.send(()).unwrap();
    upsert.join().unwrap();
    assert_eq!(cache.get("upsert").as_deref(), Some("upserted"));

    let segmented: SegmentedCache<String, String> = SegmentedCache::new(100, 4);
    let segmented_compute_cache = segmented.clone();
    let (segmented_compute_started_tx, segmented_compute_started) = sync_channel(0);
    let (segmented_compute_release, segmented_compute_release_rx) = sync_channel(0);
    let segmented_compute = thread::spawn(move || {
        segmented_compute_cache
            .entry("compute".to_owned())
            .and_compute_with(|_| {
                segmented_compute_started_tx.send(()).unwrap();
                segmented_compute_release_rx.recv().unwrap();
                Op::Put("computed".to_owned())
            });
    });
    segmented_compute_started.recv().unwrap();
    segmented.invalidate_and_fence("compute");
    segmented_compute_release.send(()).unwrap();
    segmented_compute.join().unwrap();
    assert_eq!(segmented.get("compute").as_deref(), Some("computed"));

    let segmented_upsert_cache = segmented.clone();
    let (segmented_upsert_started_tx, segmented_upsert_started) = sync_channel(0);
    let (segmented_upsert_release, segmented_upsert_release_rx) = sync_channel(0);
    let segmented_upsert = thread::spawn(move || {
        segmented_upsert_cache
            .entry("upsert".to_owned())
            .and_upsert_with(|_| {
                segmented_upsert_started_tx.send(()).unwrap();
                segmented_upsert_release_rx.recv().unwrap();
                "upserted".to_owned()
            });
    });
    segmented_upsert_started.recv().unwrap();
    segmented.invalidate_and_fence("upsert");
    segmented_upsert_release.send(()).unwrap();
    segmented_upsert.join().unwrap();
    assert_eq!(segmented.get("upsert").as_deref(), Some("upserted"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_fence_leaves_inflight_compute_and_upsert_effects_intact() {
    let cache = moka::future::Cache::new(100);

    let compute_cache = cache.clone();
    let (compute_started_tx, compute_started) = oneshot::channel();
    let (compute_release, compute_release_rx) = oneshot::channel();
    let compute = tokio::spawn(async move {
        compute_cache
            .entry("compute".to_owned())
            .and_compute_with(|_| async move {
                compute_started_tx.send(()).unwrap();
                compute_release_rx.await.unwrap();
                Op::Put("computed".to_owned())
            })
            .await;
    });
    compute_started.await.unwrap();
    cache.invalidate_and_fence("compute").await;

    let later_compute_ran = Arc::new(AtomicBool::new(false));
    let later_compute_marker = Arc::clone(&later_compute_ran);
    let mut later_compute = Box::pin(cache.entry("compute".to_owned()).and_compute_with(
        move |_| async move {
            later_compute_marker.store(true, Ordering::SeqCst);
            Op::Put("computed again".to_owned())
        },
    ));
    assert!(matches!(
        futures_util::poll!(&mut later_compute),
        Poll::Pending
    ));
    assert!(
        !later_compute_ran.load(Ordering::SeqCst),
        "a post-fence compute ran concurrently with the earlier compute"
    );

    compute_release.send(()).unwrap();
    compute.await.unwrap();
    later_compute.await;
    assert!(later_compute_ran.load(Ordering::SeqCst));
    assert_eq!(
        cache.get("compute").await.as_deref(),
        Some("computed again")
    );

    let try_compute_cache = cache.clone();
    let (try_compute_started_tx, try_compute_started) = oneshot::channel();
    let (try_compute_release, try_compute_release_rx) = oneshot::channel();
    let try_compute = tokio::spawn(async move {
        try_compute_cache
            .entry("try compute".to_owned())
            .and_try_compute_with(|_| async move {
                try_compute_started_tx.send(()).unwrap();
                try_compute_release_rx.await.unwrap();
                Ok::<_, &'static str>(Op::Put("try computed".to_owned()))
            })
            .await
            .unwrap();
    });
    try_compute_started.await.unwrap();
    cache.invalidate_and_fence("try compute").await;
    try_compute_release.send(()).unwrap();
    try_compute.await.unwrap();
    assert_eq!(
        cache.get("try compute").await.as_deref(),
        Some("try computed")
    );

    let upsert_cache = cache.clone();
    let (upsert_started_tx, upsert_started) = oneshot::channel();
    let (upsert_release, upsert_release_rx) = oneshot::channel();
    let upsert = tokio::spawn(async move {
        upsert_cache
            .entry("upsert".to_owned())
            .and_upsert_with(|_| async move {
                upsert_started_tx.send(()).unwrap();
                upsert_release_rx.await.unwrap();
                "upserted".to_owned()
            })
            .await;
    });
    upsert_started.await.unwrap();
    cache.invalidate_and_fence("upsert").await;

    let later_upsert_ran = Arc::new(AtomicBool::new(false));
    let later_upsert_marker = Arc::clone(&later_upsert_ran);
    let mut later_upsert = Box::pin(cache.entry("upsert".to_owned()).and_upsert_with(
        move |_| async move {
            later_upsert_marker.store(true, Ordering::SeqCst);
            "upserted again".to_owned()
        },
    ));
    assert!(matches!(
        futures_util::poll!(&mut later_upsert),
        Poll::Pending
    ));
    assert!(
        !later_upsert_ran.load(Ordering::SeqCst),
        "a post-fence upsert ran concurrently with the earlier upsert"
    );

    upsert_release.send(()).unwrap();
    upsert.await.unwrap();
    later_upsert.await;
    assert!(later_upsert_ran.load(Ordering::SeqCst));
    assert_eq!(cache.get("upsert").await.as_deref(), Some("upserted again"));
}

#[test]
fn sync_stale_cleanup_preserves_an_active_post_fence_group() {
    let cache = SyncCache::new(100);
    let old = start_sync_load(cache.clone(), "key", "stale");
    old.started.recv().unwrap();
    cache.invalidate_and_fence("key");
    let fresh = start_sync_load(cache.clone(), "key", "fresh");
    fresh.started.recv().unwrap();
    old.release.send(()).unwrap();
    assert_eq!(old.handle.join().unwrap(), "stale");
    assert_eq!(cache.get("key"), None);

    let second_cache = cache.clone();
    let second =
        thread::spawn(move || second_cache.get_with("key".to_owned(), || "duplicate".to_owned()));
    fresh.release.send(()).unwrap();
    assert_eq!(fresh.handle.join().unwrap(), "fresh");
    assert_eq!(second.join().unwrap(), "fresh");
    assert_eq!(cache.get("key").as_deref(), Some("fresh"));
}

#[test]
fn segmented_stale_cleanup_preserves_an_active_post_fence_group() {
    let cache = SegmentedCache::new(100, 4);
    let old = start_segmented_load(cache.clone(), "key", "stale");
    old.started.recv().unwrap();
    cache.invalidate_and_fence("key");
    let fresh = start_segmented_load(cache.clone(), "key", "fresh");
    fresh.started.recv().unwrap();
    old.release.send(()).unwrap();
    assert_eq!(old.handle.join().unwrap(), "stale");
    assert_eq!(cache.get("key"), None);

    let second_cache = cache.clone();
    let second =
        thread::spawn(move || second_cache.get_with("key".to_owned(), || "duplicate".to_owned()));
    fresh.release.send(()).unwrap();
    assert_eq!(fresh.handle.join().unwrap(), "fresh");
    assert_eq!(second.join().unwrap(), "fresh");
    assert_eq!(cache.get("key").as_deref(), Some("fresh"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_stale_cleanup_preserves_an_active_post_fence_group() {
    let cache = moka::future::Cache::new(100);
    let old = start_future_load(cache.clone(), "key", "stale");
    old.started.await.unwrap();

    cache.invalidate_and_fence("key").await;
    let fresh = start_future_load(cache.clone(), "key", "fresh");
    fresh.started.await.unwrap();

    old.release.send(()).unwrap();
    assert_eq!(old.handle.await.unwrap(), "stale");
    assert_eq!(cache.get("key").await, None);

    let duplicate_polled = Arc::new(AtomicBool::new(false));
    let duplicate_marker = Arc::clone(&duplicate_polled);
    let mut second = Box::pin(cache.get_with("key".to_owned(), async move {
        duplicate_marker.store(true, Ordering::SeqCst);
        "duplicate".to_owned()
    }));
    assert!(matches!(futures_util::poll!(&mut second), Poll::Pending));
    assert!(!duplicate_polled.load(Ordering::SeqCst));

    fresh.release.send(()).unwrap();
    assert_eq!(fresh.handle.await.unwrap(), "fresh");
    assert_eq!(second.await, "fresh");
    assert_eq!(cache.get("key").await.as_deref(), Some("fresh"));
}

trait FenceRaceCache: Clone + Send + Sync + 'static {
    fn insert_value(&self, key: usize, value: usize);
    fn read_value(&self, key: usize) -> Option<usize>;
    fn fence_key(&self, key: &NumericFenceQuery);
    fn initialize(&self, key: usize, init: impl FnOnce() -> usize + Send) -> usize;
}

macro_rules! sync_race_cache {
    ($cache:ty) => {
        impl FenceRaceCache for $cache {
            fn insert_value(&self, key: usize, value: usize) {
                self.insert(ReclaimKey(key), value);
            }
            fn read_value(&self, key: usize) -> Option<usize> {
                self.get(&ReclaimKey(key))
            }
            fn fence_key(&self, key: &NumericFenceQuery) {
                self.invalidate_and_fence(key);
            }
            #[allow(deprecated)]
            fn initialize(&self, key: usize, init: impl FnOnce() -> usize + Send) -> usize {
                self.get_with_if(ReclaimKey(key), init, |_| true)
            }
        }
    };
}
sync_race_cache!(SyncCache<ReclaimKey, usize>);
sync_race_cache!(SegmentedCache<ReclaimKey, usize>);

#[derive(Clone)]
struct FutureRaceCache {
    cache: moka::future::Cache<ReclaimKey, usize>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl FenceRaceCache for FutureRaceCache {
    fn insert_value(&self, key: usize, value: usize) {
        self.runtime
            .block_on(self.cache.insert(ReclaimKey(key), value));
    }
    fn read_value(&self, key: usize) -> Option<usize> {
        self.runtime.block_on(self.cache.get(&ReclaimKey(key)))
    }
    fn fence_key(&self, key: &NumericFenceQuery) {
        self.runtime.block_on(self.cache.invalidate_and_fence(key));
    }
    #[allow(deprecated)]
    fn initialize(&self, key: usize, init: impl FnOnce() -> usize + Send) -> usize {
        self.runtime.block_on(self.cache.get_with_if(
            ReclaimKey(key),
            async move { init() },
            |_| true,
        ))
    }
}

struct NumericFenceQuery {
    digits: String,
    checkpoint: usize,
    comparisons: std::sync::atomic::AtomicUsize,
    reached: crossbeam_channel::Sender<()>,
}

impl Hash for NumericFenceQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        0_u8.hash(state);
    }
}

impl equivalent::Equivalent<ReclaimKey> for NumericFenceQuery {
    fn equivalent(&self, key: &ReclaimKey) -> bool {
        if self.comparisons.fetch_add(1, Ordering::SeqCst) == self.checkpoint {
            self.reached.send(()).unwrap();
        }
        self.digits.parse::<usize>().unwrap() == key.0
    }
}

fn check_atomic_removal<C: FenceRaceCache>(create: impl Fn() -> C) {
    // Checkpoints select interleavings, not meanings: no comparison is assumed
    // to be admission or removal. If a checkpoint is absent, the same caller
    // runs after the completed fence and is checked as usual.
    for checkpoint in 0..16 {
        let cache = create();
        for key in 0..4 {
            cache.insert_value(key, 1);
        }
        let target = 3;
        let old_cache = cache.clone();
        let (entered_tx, entered) = sync_channel(0);
        let (release, release_rx) = sync_channel(0);
        let old = thread::spawn(move || {
            old_cache.initialize(target, move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                2
            })
        });
        entered.recv().unwrap();

        let (reached_tx, reached) = crossbeam_channel::bounded(0);
        let (finished_tx, finished) = crossbeam_channel::bounded(1);
        // A borrowed decimal spelling compares by numeric value. A long legal
        // spelling lets a competing caller progress without sleeps or deadlines.
        let query = NumericFenceQuery {
            digits: format!("{}{}", "0".repeat(262144), target),
            checkpoint,
            comparisons: std::sync::atomic::AtomicUsize::new(0),
            reached: reached_tx,
        };
        let fence_cache = cache.clone();
        let fence = thread::spawn(move || {
            fence_cache.fence_key(&query);
            finished_tx.send(()).unwrap();
            release.send(()).unwrap();
        });
        crossbeam_channel::select! {
            recv(reached) -> _ => {},
            recv(finished) -> result => result.unwrap(),
        }
        let outcome = cache.initialize(target, || 3);
        fence.join().unwrap();
        assert_eq!(old.join().unwrap(), 2);
        // The old group stays active until the fence returns. A caller that
        // runs its own initializer must therefore be in the later group.
        match outcome {
            2 => assert_eq!(
                cache.read_value(target),
                None,
                "checkpoint {checkpoint}: earlier group published"
            ),
            3 => assert_eq!(
                cache.read_value(target),
                Some(3),
                "checkpoint {checkpoint}: fence removed the later group's value"
            ),
            other => panic!("checkpoint {checkpoint}: unexpected initializer outcome {other}"),
        }
        assert_eq!(cache.read_value(0), Some(1));
    }
}

#[test]
#[allow(deprecated)]
fn sync_fence_does_not_remove_a_concurrent_later_group_value() {
    check_atomic_removal(|| SyncCache::<ReclaimKey, usize>::new(1000));
    let (listener_entered_tx, listener_entered) = sync_channel(0);
    let (listener_release, listener_release_rx) = sync_channel(0);
    let listener_release_rx = Arc::new(Mutex::new(listener_release_rx));
    let cache = SyncCache::<String, String>::builder()
        .max_capacity(100)
        .eviction_listener(move |key, _, cause| {
            if key.as_str() == "key" && cause == RemovalCause::Explicit {
                listener_entered_tx.send(()).unwrap();
                listener_release_rx.lock().unwrap().recv().unwrap();
            }
        })
        .build();
    cache.insert("key".to_owned(), "cached".to_owned());

    let fence_cache = cache.clone();
    let fence = thread::spawn(move || fence_cache.invalidate_and_fence("key"));
    listener_entered.recv().unwrap();

    assert_eq!(
        cache.get_with_if("key".to_owned(), || "fresh".to_owned(), |_| true),
        "fresh"
    );
    listener_release.send(()).unwrap();
    fence.join().unwrap();
    assert_eq!(cache.get("key").as_deref(), Some("fresh"));
}

#[test]
fn segmented_fence_does_not_remove_a_concurrent_later_group_value() {
    check_atomic_removal(|| SegmentedCache::<ReclaimKey, usize>::new(1000, 4));
    let (listener_entered_tx, listener_entered) = sync_channel(0);
    let (listener_release, listener_release_rx) = sync_channel(0);
    let listener_release_rx = Arc::new(Mutex::new(listener_release_rx));
    let cache = SegmentedCache::<String, String>::builder(4)
        .max_capacity(100)
        .eviction_listener(move |key, _, cause| {
            if key.as_str() == "key" && cause == RemovalCause::Explicit {
                listener_entered_tx.send(()).unwrap();
                listener_release_rx.lock().unwrap().recv().unwrap();
            }
        })
        .build();
    cache.insert("key".to_owned(), "cached".to_owned());

    let fence_cache = cache.clone();
    let fence = thread::spawn(move || fence_cache.invalidate_and_fence("key"));
    listener_entered.recv().unwrap();

    assert_eq!(
        cache.get_with_if("key".to_owned(), || "fresh".to_owned(), |_| true),
        "fresh"
    );
    listener_release.send(()).unwrap();
    fence.join().unwrap();
    assert_eq!(cache.get("key").as_deref(), Some("fresh"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(deprecated)]
async fn future_fence_does_not_remove_a_concurrent_later_group_value() {
    tokio::task::spawn_blocking(|| {
        boundary_interleavings::empty_admission_history();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        check_atomic_removal(|| FutureRaceCache {
            cache: moka::future::Cache::new(1000),
            runtime: Arc::clone(&runtime),
        });
    })
    .await
    .unwrap();
    let (listener_entered_tx, listener_entered) = oneshot::channel();
    let listener_entered_tx = Arc::new(Mutex::new(Some(listener_entered_tx)));
    let (listener_release, listener_release_rx) = oneshot::channel();
    let listener_release_rx = Arc::new(Mutex::new(Some(listener_release_rx)));
    let cache = moka::future::Cache::<String, String>::builder()
        .max_capacity(100)
        .async_eviction_listener(move |key, _, cause| {
            let listener_entered_tx = Arc::clone(&listener_entered_tx);
            let listener_release_rx = Arc::clone(&listener_release_rx);
            Box::pin(async move {
                if key.as_str() == "key" && cause == RemovalCause::Explicit {
                    listener_entered_tx
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .send(())
                        .unwrap();
                    let release = listener_release_rx.lock().unwrap().take().unwrap();
                    release.await.unwrap();
                }
            }) as ListenerFuture
        })
        .build();
    cache.insert("key".to_owned(), "cached".to_owned()).await;

    let fence_cache = cache.clone();
    let fence = tokio::spawn(async move { fence_cache.invalidate_and_fence("key").await });
    listener_entered.await.unwrap();

    assert_eq!(
        cache
            .get_with_if("key".to_owned(), async { "fresh".to_owned() }, |_| true)
            .await,
        "fresh"
    );
    listener_release.send(()).unwrap();
    fence.await.unwrap();
    assert_eq!(cache.get("key").await.as_deref(), Some("fresh"));
}

#[test]
#[allow(deprecated)]
fn sync_fence_listener_can_reenter_same_and_colliding_keys() {
    let cache_slot = Arc::new(Mutex::new(None::<SyncCache<CollisionKey, String>>));
    let listener_slot = Arc::clone(&cache_slot);
    let (nested_tx, nested) = sync_channel(1);
    let blocker_armed = Arc::new(AtomicBool::new(false));
    let listener_blocker_armed = Arc::clone(&blocker_armed);
    let (blocker_started_tx, blocker_started) = sync_channel(0);
    let (blocker_release, blocker_release_rx) = sync_channel(0);
    let blocker_release_rx = Arc::new(Mutex::new(blocker_release_rx));
    let listener_blocker_release_rx = Arc::clone(&blocker_release_rx);
    let cache = SyncCache::<CollisionKey, String>::builder()
        .max_capacity(100)
        .eviction_listener(move |key, value, cause| {
            if key.0 == "blocker" && listener_blocker_armed.swap(false, Ordering::SeqCst) {
                blocker_started_tx.send(()).unwrap();
                listener_blocker_release_rx.lock().unwrap().recv().unwrap();
                return;
            }
            let nested_keys = match (key.0, cause) {
                ("a", RemovalCause::Explicit) => Some(("b", "c")),
                _ => None,
            };
            if let Some((get_key, insert_key)) = nested_keys {
                let cache = listener_slot.lock().unwrap().as_ref().unwrap().clone();
                let same_key_value = (cause == RemovalCause::Explicit)
                    .then(|| cache.get_with(CollisionKey(key.0), || format!("listener {}", key.0)));
                let nested_value =
                    cache.get_with(CollisionKey(get_key), || format!("value {get_key}"));
                cache.insert(CollisionKey(insert_key), format!("value {insert_key}"));
                nested_tx
                    .send((key.0, value, cause, same_key_value, nested_value))
                    .unwrap();
            }
        })
        .build();
    *cache_slot.lock().unwrap() = Some(cache.clone());
    cache.insert(CollisionKey("a"), "value a".to_owned());

    let stale_cache = cache.clone();
    let (stale_started_tx, stale_started) = sync_channel(0);
    let (stale_release, stale_release_rx) = sync_channel(0);
    let stale = thread::spawn(move || {
        stale_cache.get_with_if(
            CollisionKey("a"),
            || {
                stale_started_tx.send(()).unwrap();
                stale_release_rx.recv().unwrap();
                "stale a".to_owned()
            },
            |_| true,
        )
    });
    stale_started.recv().unwrap();

    cache.insert(CollisionKey("blocker"), "old blocker".to_owned());
    blocker_armed.store(true, Ordering::SeqCst);
    let blocker_cache = cache.clone();
    let blocker = thread::spawn(move || {
        blocker_cache.get_with_if(
            CollisionKey("blocker"),
            || "new blocker".to_owned(),
            |_| true,
        )
    });
    blocker_started.recv().unwrap();

    let fence_cache = cache.clone();
    let fence = thread::spawn(move || fence_cache.invalidate_and_fence(&CollisionKey("a")));
    fence.join().unwrap();
    let (removed_key, removed_value, removed_cause, same_key_value, nested_value) =
        nested.recv().unwrap();
    assert_eq!(removed_key, "a");
    assert_eq!(removed_value, "value a");
    assert_eq!(removed_cause, RemovalCause::Explicit);
    assert_eq!(same_key_value.as_deref(), Some("listener a"));
    assert_eq!(nested_value, "value b");
    assert_eq!(cache.get(&CollisionKey("a")).as_deref(), Some("listener a"));
    assert_eq!(cache.get(&CollisionKey("b")).as_deref(), Some("value b"));
    assert_eq!(cache.get(&CollisionKey("c")).as_deref(), Some("value c"));

    stale_release.send(()).unwrap();
    assert_eq!(stale.join().unwrap(), "stale a");
    assert_eq!(cache.get(&CollisionKey("a")).as_deref(), Some("listener a"));

    blocker_release.send(()).unwrap();
    assert_eq!(blocker.join().unwrap(), "new blocker");
    assert_eq!(
        cache.get(&CollisionKey("blocker")).as_deref(),
        Some("new blocker")
    );
}

#[test]
fn segmented_fence_listener_can_reenter_same_and_colliding_keys() {
    let cache_slot = Arc::new(Mutex::new(None::<SegmentedCache<CollisionKey, String>>));
    let listener_slot = Arc::clone(&cache_slot);
    let (nested_tx, nested) = sync_channel(1);
    let blocker_armed = Arc::new(AtomicBool::new(false));
    let listener_blocker_armed = Arc::clone(&blocker_armed);
    let (blocker_started_tx, blocker_started) = sync_channel(0);
    let (blocker_release, blocker_release_rx) = sync_channel(0);
    let blocker_release_rx = Arc::new(Mutex::new(blocker_release_rx));
    let listener_blocker_release_rx = Arc::clone(&blocker_release_rx);
    let cache = SegmentedCache::<CollisionKey, String>::builder(4)
        .max_capacity(100)
        .eviction_listener(move |key, value, cause| {
            if key.0 == "blocker" && listener_blocker_armed.swap(false, Ordering::SeqCst) {
                blocker_started_tx.send(()).unwrap();
                listener_blocker_release_rx.lock().unwrap().recv().unwrap();
                return;
            }
            let nested_keys = match (key.0, cause) {
                ("a", RemovalCause::Explicit) => Some(("b", "c")),
                _ => None,
            };
            if let Some((get_key, insert_key)) = nested_keys {
                let cache = listener_slot.lock().unwrap().as_ref().unwrap().clone();
                let same_key_value = (cause == RemovalCause::Explicit)
                    .then(|| cache.get_with(CollisionKey(key.0), || format!("listener {}", key.0)));
                let nested_value =
                    cache.get_with(CollisionKey(get_key), || format!("value {get_key}"));
                cache.insert(CollisionKey(insert_key), format!("value {insert_key}"));
                nested_tx
                    .send((key.0, value, cause, same_key_value, nested_value))
                    .unwrap();
            }
        })
        .build();
    *cache_slot.lock().unwrap() = Some(cache.clone());
    cache.insert(CollisionKey("a"), "value a".to_owned());

    let stale_cache = cache.clone();
    let (stale_started_tx, stale_started) = sync_channel(0);
    let (stale_release, stale_release_rx) = sync_channel(0);
    let stale = thread::spawn(move || {
        stale_cache.get_with_if(
            CollisionKey("a"),
            || {
                stale_started_tx.send(()).unwrap();
                stale_release_rx.recv().unwrap();
                "stale a".to_owned()
            },
            |_| true,
        )
    });
    stale_started.recv().unwrap();

    cache.insert(CollisionKey("blocker"), "old blocker".to_owned());
    blocker_armed.store(true, Ordering::SeqCst);
    let blocker_cache = cache.clone();
    let blocker = thread::spawn(move || {
        blocker_cache.get_with_if(
            CollisionKey("blocker"),
            || "new blocker".to_owned(),
            |_| true,
        )
    });
    blocker_started.recv().unwrap();

    let colliding_cache = cache.clone();
    let (colliding_started_tx, colliding_started) = sync_channel(0);
    let (colliding_release, colliding_release_rx) = sync_channel(0);
    let colliding = thread::spawn(move || {
        colliding_cache.get_with(CollisionKey("inflight b"), || {
            colliding_started_tx.send(()).unwrap();
            colliding_release_rx.recv().unwrap();
            "inflight value b".to_owned()
        })
    });
    colliding_started.recv().unwrap();

    let fence_cache = cache.clone();
    let fence = thread::spawn(move || fence_cache.invalidate_and_fence(&CollisionKey("a")));
    fence.join().unwrap();
    let (removed_key, removed_value, removed_cause, same_key_value, nested_value) =
        nested.recv().unwrap();
    assert_eq!(removed_key, "a");
    assert_eq!(removed_value, "value a");
    assert_eq!(removed_cause, RemovalCause::Explicit);
    assert_eq!(same_key_value.as_deref(), Some("listener a"));
    assert_eq!(nested_value, "value b");
    assert_eq!(cache.get(&CollisionKey("a")).as_deref(), Some("listener a"));
    assert_eq!(cache.get(&CollisionKey("b")).as_deref(), Some("value b"));
    assert_eq!(cache.get(&CollisionKey("c")).as_deref(), Some("value c"));

    stale_release.send(()).unwrap();
    assert_eq!(stale.join().unwrap(), "stale a");
    assert_eq!(cache.get(&CollisionKey("a")).as_deref(), Some("listener a"));

    colliding_release.send(()).unwrap();
    assert_eq!(colliding.join().unwrap(), "inflight value b");
    assert_eq!(
        cache.get(&CollisionKey("inflight b")).as_deref(),
        Some("inflight value b")
    );

    blocker_release.send(()).unwrap();
    assert_eq!(blocker.join().unwrap(), "new blocker");
    assert_eq!(
        cache.get(&CollisionKey("blocker")).as_deref(),
        Some("new blocker")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(deprecated)]
async fn future_fence_listener_can_reenter_same_and_colliding_keys() {
    boundary_interleavings::listener_reentry_while_another_fence_waits().await;
    let cache_slot = Arc::new(Mutex::new(
        None::<moka::future::Cache<CollisionKey, String>>,
    ));
    let listener_slot = Arc::clone(&cache_slot);
    let (nested_tx, mut nested) = tokio::sync::mpsc::unbounded_channel();
    let blocker_armed = Arc::new(AtomicBool::new(false));
    let listener_blocker_armed = Arc::clone(&blocker_armed);
    let (blocker_started_tx, blocker_started) = oneshot::channel();
    let blocker_started_tx = Arc::new(Mutex::new(Some(blocker_started_tx)));
    let listener_blocker_started_tx = Arc::clone(&blocker_started_tx);
    let (blocker_release, blocker_release_rx) = oneshot::channel();
    let blocker_release_rx = Arc::new(Mutex::new(Some(blocker_release_rx)));
    let listener_blocker_release_rx = Arc::clone(&blocker_release_rx);
    let cache = moka::future::Cache::<CollisionKey, String>::builder()
        .max_capacity(100)
        .async_eviction_listener(move |key, value, cause| {
            let cache = listener_slot.lock().unwrap().as_ref().unwrap().clone();
            let nested_tx = nested_tx.clone();
            let blocker_started_tx = Arc::clone(&listener_blocker_started_tx);
            let blocker_release_rx = Arc::clone(&listener_blocker_release_rx);
            let blocker_armed = Arc::clone(&listener_blocker_armed);
            Box::pin(async move {
                if key.0 == "blocker" && blocker_armed.swap(false, Ordering::SeqCst) {
                    blocker_started_tx
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .send(())
                        .unwrap();
                    let release = blocker_release_rx.lock().unwrap().take().unwrap();
                    release.await.unwrap();
                    return;
                }
                let nested_keys = match (key.0, cause) {
                    ("a", RemovalCause::Explicit) => Some(("b", "c")),
                    _ => None,
                };
                if let Some((get_key, insert_key)) = nested_keys {
                    let same_key_value = if cause == RemovalCause::Explicit {
                        let same_key = key.0;
                        Some(
                            cache
                                .get_with(CollisionKey(same_key), async move {
                                    format!("listener {same_key}")
                                })
                                .await,
                        )
                    } else {
                        None
                    };
                    let nested_value = cache
                        .get_with(
                            CollisionKey(get_key),
                            async move { format!("value {get_key}") },
                        )
                        .await;
                    cache
                        .insert(CollisionKey(insert_key), format!("value {insert_key}"))
                        .await;
                    nested_tx
                        .send((key.0, value, cause, same_key_value, nested_value))
                        .unwrap();
                }
            }) as ListenerFuture
        })
        .build();
    *cache_slot.lock().unwrap() = Some(cache.clone());
    cache.insert(CollisionKey("a"), "value a".to_owned()).await;

    let stale_cache = cache.clone();
    let (stale_started_tx, stale_started) = oneshot::channel();
    let (stale_release, stale_release_rx) = oneshot::channel();
    let stale = tokio::spawn(async move {
        stale_cache
            .get_with_if(
                CollisionKey("a"),
                async move {
                    stale_started_tx.send(()).unwrap();
                    stale_release_rx.await.unwrap();
                    "stale a".to_owned()
                },
                |_| true,
            )
            .await
    });
    stale_started.await.unwrap();

    cache
        .insert(CollisionKey("blocker"), "old blocker".to_owned())
        .await;
    blocker_armed.store(true, Ordering::SeqCst);
    let blocker_cache = cache.clone();
    let blocker = tokio::spawn(async move {
        blocker_cache
            .get_with_if(
                CollisionKey("blocker"),
                async { "new blocker".to_owned() },
                |_| true,
            )
            .await
    });
    blocker_started.await.unwrap();

    cache.invalidate_and_fence(&CollisionKey("a")).await;

    let (removed_key, removed_value, removed_cause, same_key_value, nested_value) =
        nested.recv().await.unwrap();
    assert_eq!(removed_key, "a");
    assert_eq!(removed_value, "value a");
    assert_eq!(removed_cause, RemovalCause::Explicit);
    assert_eq!(same_key_value.as_deref(), Some("listener a"));
    assert_eq!(nested_value, "value b");
    assert_eq!(
        cache.get(&CollisionKey("a")).await.as_deref(),
        Some("listener a")
    );
    assert_eq!(
        cache.get(&CollisionKey("b")).await.as_deref(),
        Some("value b")
    );
    assert_eq!(
        cache.get(&CollisionKey("c")).await.as_deref(),
        Some("value c")
    );

    stale_release.send(()).unwrap();
    assert_eq!(stale.await.unwrap(), "stale a");
    assert_eq!(
        cache.get(&CollisionKey("a")).await.as_deref(),
        Some("listener a")
    );

    blocker_release.send(()).unwrap();
    assert_eq!(blocker.await.unwrap(), "new blocker");
    assert_eq!(
        cache.get(&CollisionKey("blocker")).await.as_deref(),
        Some("new blocker")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(deprecated)]
async fn cancelling_future_fence_during_listener_preserves_boundary() {
    let (listener_entered_tx, mut listener_entered) = oneshot::channel();
    let listener_entered_tx = Arc::new(Mutex::new(Some(listener_entered_tx)));
    let (listener_release, listener_release_rx) = oneshot::channel();
    let listener_release_rx = Arc::new(Mutex::new(Some(listener_release_rx)));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let cache = moka::future::Cache::<String, String>::builder()
        .max_capacity(100)
        .async_eviction_listener({
            let observations = Arc::clone(&observations);
            move |key, value, cause| {
                observations.lock().unwrap().push((key, value, cause));
                let entered = listener_entered_tx.lock().unwrap().take().unwrap();
                let release = listener_release_rx.lock().unwrap().take().unwrap();
                Box::pin(async move {
                    entered.send(()).unwrap();
                    release.await.unwrap();
                }) as ListenerFuture
            }
        })
        .build();
    cache
        .insert("key".to_owned(), "cached value".to_owned())
        .await;

    let stale_cache = cache.clone();
    let (stale_started_tx, stale_started) = oneshot::channel();
    let (stale_release, stale_release_rx) = oneshot::channel();
    let stale = tokio::spawn(async move {
        stale_cache
            .get_with_if(
                "key".to_owned(),
                async move {
                    stale_started_tx.send(()).unwrap();
                    stale_release_rx.await.unwrap();
                    "stale value".to_owned()
                },
                |_| true,
            )
            .await
    });
    stale_started.await.unwrap();

    let mut fence = Box::pin(cache.invalidate_and_fence("key"));
    tokio::select! {
        entered = &mut listener_entered => entered.unwrap(),
        () = &mut fence => listener_entered.await.unwrap(),
    }
    drop(fence);
    {
        let actual = observations.lock().unwrap();
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].0.as_str(), "key");
        assert_eq!(actual[0].1, "cached value");
        assert_eq!(actual[0].2, RemovalCause::Explicit);
    }

    // The boundary is observable before cancellation, regardless of how many
    // times the operation yielded before entering the listener.
    let _ = listener_release.send(());

    assert_eq!(
        cache
            .get_with("key".to_owned(), async { "fresh value".to_owned() })
            .await,
        "fresh value"
    );
    assert_eq!(cache.get("key").await.as_deref(), Some("fresh value"));

    stale_release.send(()).unwrap();
    assert_eq!(stale.await.unwrap(), "stale value");
    assert_eq!(cache.get("key").await.as_deref(), Some("fresh value"));
}

#[test]
#[allow(deprecated)]
fn sync_fenced_notifications_can_complete_out_of_order() {
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let (notified_tx, notified) = std::sync::mpsc::channel();
    let cache = SyncCache::<u32, u32>::builder()
        .max_capacity(100)
        .eviction_listener(move |_, _, cause| {
            if cause == RemovalCause::Explicit {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
            notified_tx.send(cause).unwrap();
        })
        .build();
    cache.insert(1, 1);
    let other = cache.clone();
    let fence = thread::spawn(move || other.invalidate_and_fence(&1));
    entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    cache.insert(1, 2);
    cache.insert(1, 3);
    assert_eq!(
        notified
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        RemovalCause::Replaced
    );
    release.send(()).unwrap();
    fence.join().unwrap();
    assert_eq!(
        notified
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        RemovalCause::Explicit
    );
    assert_eq!(cache.get(&1), Some(3));
}

#[tokio::test]
#[allow(deprecated)]
async fn future_fenced_notifications_can_complete_out_of_order() {
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let (notified_tx, mut notified) = tokio::sync::mpsc::unbounded_channel();
    let cache = moka::future::Cache::<u32, u32>::builder()
        .max_capacity(100)
        .async_eviction_listener(move |_, _, cause| {
            let entered_tx = entered_tx.clone();
            let release_rx = release_rx.clone();
            let notified_tx = notified_tx.clone();
            Box::pin(async move {
                if cause == RemovalCause::Explicit {
                    entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
                    let release = release_rx.lock().unwrap().take().unwrap();
                    tokio::time::timeout(std::time::Duration::from_secs(5), release)
                        .await
                        .unwrap()
                        .unwrap();
                }
                notified_tx.send(cause).unwrap();
            })
        })
        .build();
    cache.insert(1, 1).await;
    let other = cache.clone();
    let fence = tokio::spawn(async move { other.invalidate_and_fence(&1).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    cache.insert(1, 2).await;
    cache.insert(1, 3).await;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), notified.recv())
            .await
            .unwrap(),
        Some(RemovalCause::Replaced)
    );
    release.send(()).unwrap();
    fence.await.unwrap();
    assert_eq!(notified.recv().await, Some(RemovalCause::Explicit));
    assert_eq!(cache.get(&1).await, Some(3));
}
