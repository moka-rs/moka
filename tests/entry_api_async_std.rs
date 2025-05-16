#![cfg(all(test, feature = "future"))]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_lock::Barrier;
use moka::future::Cache;

const NUM_THREADS: u8 = 16;

#[async_std::test]
async fn test_get_with() {
    const TEN_MIB: usize = 10 * 1024 * 1024; // 10MiB
    let cache = Cache::new(100);
    let call_counter = Arc::new(AtomicUsize::default());
    let barrier = Arc::new(Barrier::new(NUM_THREADS as usize));

    let tasks: Vec<_> = (0..NUM_THREADS)
        .map(|task_id| {
            let my_cache = cache.clone();
            let my_call_counter = Arc::clone(&call_counter);
            let my_barrier = Arc::clone(&barrier);

            async_std::task::spawn(async move {
                my_barrier.wait().await;

                println!("Task {task_id} started.");

                let key: Arc<str> = "key1".into();
                let value = match task_id % 4 {
                    0 => {
                        my_cache
                            .get_with_by_ref(key.as_ref(), async move {
                                println!("Task {task_id} inserting a value.");
                                my_call_counter.fetch_add(1, Ordering::AcqRel);
                                Arc::new(vec![0u8; TEN_MIB])
                            })
                            .await
                    }
                    1 => {
                        my_cache
                            .get_with_by_ref(key.as_ref(), async move {
                                println!("Task {task_id} inserting a value.");
                                my_call_counter.fetch_add(1, Ordering::AcqRel);
                                Arc::new(vec![0u8; TEN_MIB])
                            })
                            .await
                    }
                    2 => my_cache
                        .entry_by_ref(key.as_ref())
                        .or_insert_with(async move {
                            println!("Task {task_id} inserting a value.");
                            my_call_counter.fetch_add(1, Ordering::AcqRel);
                            Arc::new(vec![0u8; TEN_MIB])
                        })
                        .await
                        .into_value(),
                    3 => my_cache
                        .entry_by_ref(key.as_ref())
                        .or_insert_with(async move {
                            println!("Task {task_id} inserting a value.");
                            my_call_counter.fetch_add(1, Ordering::AcqRel);
                            Arc::new(vec![0u8; TEN_MIB])
                        })
                        .await
                        .into_value(),
                    _ => unreachable!(),
                };

                assert_eq!(value.len(), TEN_MIB);
                assert!(my_cache.get(key.as_ref()).await.is_some());

                println!("Task {task_id} got the value. (len: {})", value.len());
            })
        })
        .collect();

    futures_util::future::join_all(tasks).await;
    assert_eq!(call_counter.load(Ordering::Acquire), 1);
}
