#[macro_export]
macro_rules! write_test_cases_for_me {
    ($m:ident) => {
        #[test]
        fn insertion() {
            const MAX_VALUE: i32 = 512;

            let map = $m::with_capacity(MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                assert_eq!(map.insert(i, i), None);

                assert!(!map.is_empty());
                assert_eq!(map.len(), (i + 1) as usize);

                for j in 0..=i {
                    assert_eq!(map.get(&j), Some(j));
                    assert_eq!(map.insert(j, j), Some(j));
                }

                for k in i + 1..MAX_VALUE {
                    assert_eq!(map.get(&k), None);
                }
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn growth() {
            const MAX_VALUE: i32 = 512;

            let map = $m::new();

            for i in 0..MAX_VALUE {
                assert_eq!(map.insert(i, i), None);

                assert!(!map.is_empty());
                assert_eq!(map.len(), (i + 1) as usize);

                for j in 0..=i {
                    assert_eq!(map.get(&j), Some(j));
                    assert_eq!(map.insert(j, j), Some(j));
                }

                for k in i + 1..MAX_VALUE {
                    assert_eq!(map.get(&k), None);
                }
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_insertion() {
            const MAX_VALUE: i32 = 512;
            const NUM_THREADS: usize = 64;
            const MAX_INSERTED_VALUE: i32 = (NUM_THREADS as i32) * MAX_VALUE;

            let map = std::sync::Arc::new($m::with_capacity(MAX_INSERTED_VALUE as usize));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in (0..MAX_VALUE).map(|j| j + (i as i32 * MAX_VALUE)) {
                            assert_eq!(map.insert(j, j), None);
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(std::thread::JoinHandle::join) {
                assert!(result.is_ok());
            }

            assert!(!map.is_empty());
            assert_eq!(map.len(), MAX_INSERTED_VALUE as usize);

            for i in 0..MAX_INSERTED_VALUE {
                assert_eq!(map.get(&i), Some(i));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_growth() {
            const MAX_VALUE: i32 = 512;
            const NUM_THREADS: usize = 64;
            const MAX_INSERTED_VALUE: i32 = (NUM_THREADS as i32) * MAX_VALUE;

            let map = std::sync::Arc::new($m::new());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in (0..MAX_VALUE).map(|j| j + (i as i32 * MAX_VALUE)) {
                            assert_eq!(map.insert(j, j), None);
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(|t| t.join()) {
                assert!(result.is_ok());
            }

            assert!(!map.is_empty());
            assert_eq!(map.len(), MAX_INSERTED_VALUE as usize);

            for i in 0..MAX_INSERTED_VALUE {
                assert_eq!(map.get(&i), Some(i));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn removal() {
            const MAX_VALUE: i32 = 512;

            let map = $m::with_capacity(MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                assert_eq!(map.insert(i, i), None);
            }

            for i in 0..MAX_VALUE {
                assert_eq!(map.remove(&i), Some(i));
            }

            assert!(map.is_empty());
            assert_eq!(map.len(), 0);

            for i in 0..MAX_VALUE {
                assert_eq!(map.get(&i), None);
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_removal() {
            const MAX_VALUE: i32 = 512;
            const NUM_THREADS: usize = 64;
            const MAX_INSERTED_VALUE: i32 = (NUM_THREADS as i32) * MAX_VALUE;

            let map = $m::with_capacity(MAX_INSERTED_VALUE as usize);

            for i in 0..MAX_INSERTED_VALUE {
                assert_eq!(map.insert(i, i), None);
            }

            let map = std::sync::Arc::new(map);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in (0..MAX_VALUE).map(|j| j + (i as i32 * MAX_VALUE)) {
                            assert_eq!(map.remove(&j), Some(j));
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(|t| t.join()) {
                assert!(result.is_ok());
            }

            assert_eq!(map.len(), 0);

            for i in 0..MAX_INSERTED_VALUE {
                assert_eq!(map.get(&i), None);
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_insertion_and_removal() {
            const MAX_VALUE: i32 = 512;
            const NUM_THREADS: usize = 64;
            const MAX_INSERTED_VALUE: i32 = (NUM_THREADS as i32) * MAX_VALUE * 2;
            const INSERTED_MIDPOINT: i32 = MAX_INSERTED_VALUE / 2;

            let map = $m::with_capacity(MAX_INSERTED_VALUE as usize);

            for i in INSERTED_MIDPOINT..MAX_INSERTED_VALUE {
                assert_eq!(map.insert(i, i), None);
            }

            let map = std::sync::Arc::new(map);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS * 2));

            let insert_threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in (0..MAX_VALUE).map(|j| j + (i as i32 * MAX_VALUE)) {
                            assert_eq!(map.insert(j, j), None);
                        }
                    })
                })
                .collect();

            let remove_threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in
                            (0..MAX_VALUE).map(|j| INSERTED_MIDPOINT + j + (i as i32 * MAX_VALUE))
                        {
                            assert_eq!(map.remove(&j), Some(j));
                        }
                    })
                })
                .collect();

            for result in insert_threads
                .into_iter()
                .chain(remove_threads.into_iter())
                .map(|t| t.join())
            {
                assert!(result.is_ok());
            }

            assert!(!map.is_empty());
            assert_eq!(map.len(), INSERTED_MIDPOINT as usize);

            for i in 0..INSERTED_MIDPOINT {
                assert_eq!(map.get(&i), Some(i));
            }

            for i in INSERTED_MIDPOINT..MAX_INSERTED_VALUE {
                assert_eq!(map.get(&i), None);
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_growth_and_removal() {
            const MAX_VALUE: i32 = 512;
            const NUM_THREADS: usize = 64;
            const MAX_INSERTED_VALUE: i32 = (NUM_THREADS as i32) * MAX_VALUE * 2;
            const INSERTED_MIDPOINT: i32 = MAX_INSERTED_VALUE / 2;

            let map = $m::with_capacity(INSERTED_MIDPOINT as usize);

            for i in INSERTED_MIDPOINT..MAX_INSERTED_VALUE {
                assert_eq!(map.insert(i, i), None);
            }

            let map = std::sync::Arc::new(map);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS * 2));

            let insert_threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in (0..MAX_VALUE).map(|j| j + (i as i32 * MAX_VALUE)) {
                            assert_eq!(map.insert(j, j), None);
                        }
                    })
                })
                .collect();

            let remove_threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in
                            (0..MAX_VALUE).map(|j| INSERTED_MIDPOINT + j + (i as i32 * MAX_VALUE))
                        {
                            assert_eq!(map.remove(&j), Some(j));
                        }
                    })
                })
                .collect();

            for result in insert_threads
                .into_iter()
                .chain(remove_threads.into_iter())
                .map(std::thread::JoinHandle::join)
            {
                assert!(result.is_ok());
            }

            assert!(!map.is_empty());
            assert_eq!(map.len(), INSERTED_MIDPOINT as usize);

            for i in 0..INSERTED_MIDPOINT {
                assert_eq!(map.get(&i), Some(i));
            }

            for i in INSERTED_MIDPOINT..MAX_INSERTED_VALUE {
                assert_eq!(map.get(&i), None);
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn modify() {
            let map = $m::new();

            assert!(map.is_empty());
            assert_eq!(map.len(), 0);

            assert_eq!(map.modify("foo", |_, x| x * 2), None);

            assert!(map.is_empty());
            assert_eq!(map.len(), 0);

            map.insert("foo", 1);
            assert_eq!(map.modify("foo", |_, x| x * 2), Some(1));

            assert!(!map.is_empty());
            assert_eq!(map.len(), 1);

            map.remove("foo");
            assert_eq!(map.modify("foo", |_, x| x * 2), None);

            assert!(map.is_empty());
            assert_eq!(map.len(), 0);

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_modification() {
            const MAX_VALUE: i32 = 512;
            const NUM_THREADS: usize = 64;
            const MAX_INSERTED_VALUE: i32 = (NUM_THREADS as i32) * MAX_VALUE;

            let map = $m::with_capacity(MAX_INSERTED_VALUE as usize);

            for i in 0..MAX_INSERTED_VALUE {
                map.insert(i, i);
            }

            let map = std::sync::Arc::new(map);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|i| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in (i as i32 * MAX_VALUE)..((i as i32 + 1) * MAX_VALUE) {
                            assert_eq!(map.modify(j, |_, x| x * 2), Some(j));
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(std::thread::JoinHandle::join) {
                assert!(result.is_ok());
            }

            assert!(!map.is_empty());
            assert_eq!(map.len(), MAX_INSERTED_VALUE as usize);

            for i in 0..MAX_INSERTED_VALUE {
                assert_eq!(map.get(&i), Some(i * 2));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_overlapped_modification() {
            const MAX_VALUE: i32 = 512;
            const NUM_THREADS: usize = 64;

            let map = $m::with_capacity(MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                assert_eq!(map.insert(i, 0), None);
            }

            let map = std::sync::Arc::new(map);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for i in 0..MAX_VALUE {
                            assert!(map.modify(i, |_, x| x + 1).is_some());
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(std::thread::JoinHandle::join) {
                assert!(result.is_ok());
            }

            assert!(!map.is_empty());
            assert_eq!(map.len(), MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                assert_eq!(map.get(&i), Some(NUM_THREADS as i32));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn insert_or_modify() {
            let map = $m::new();

            assert_eq!(map.insert_or_modify("foo", 1, |_, x| x + 1), None);
            assert_eq!(map.get("foo"), Some(1));

            assert_eq!(map.insert_or_modify("foo", 1, |_, x| x + 1), Some(1));
            assert_eq!(map.get("foo"), Some(2));

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_insert_or_modify() {
            const NUM_THREADS: usize = 64;
            const MAX_VALUE: i32 = 512;

            let map = std::sync::Arc::new($m::new());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in 0..MAX_VALUE {
                            map.insert_or_modify(j, 1, |_, x| x + 1);
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(std::thread::JoinHandle::join) {
                assert!(result.is_ok());
            }

            assert_eq!(map.len(), MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                assert_eq!(map.get(&i), Some(NUM_THREADS as i32));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_overlapped_insertion() {
            const NUM_THREADS: usize = 64;
            const MAX_VALUE: i32 = 512;

            let map = std::sync::Arc::new($m::with_capacity(MAX_VALUE as usize));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in 0..MAX_VALUE {
                            map.insert(j, j);
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(std::thread::JoinHandle::join) {
                assert!(result.is_ok());
            }

            assert_eq!(map.len(), MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                assert_eq!(map.get(&i), Some(i));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_overlapped_growth() {
            const NUM_THREADS: usize = 64;
            const MAX_VALUE: i32 = 512;

            let map = std::sync::Arc::new($m::with_capacity(1));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in 0..MAX_VALUE {
                            map.insert(j, j);
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(std::thread::JoinHandle::join) {
                assert!(result.is_ok());
            }

            assert_eq!(map.len(), MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                assert_eq!(map.get(&i), Some(i));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn concurrent_overlapped_removal() {
            const NUM_THREADS: usize = 64;
            const MAX_VALUE: i32 = 512;

            let map = $m::with_capacity(MAX_VALUE as usize);

            for i in 0..MAX_VALUE {
                map.insert(i, i);
            }

            let map = std::sync::Arc::new(map);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

            let threads: Vec<_> = (0..NUM_THREADS)
                .map(|_| {
                    let map = std::sync::Arc::clone(&map);
                    let barrier = std::sync::Arc::clone(&barrier);

                    std::thread::spawn(move || {
                        barrier.wait();

                        for j in 0..MAX_VALUE {
                            let prev_value = map.remove(&j);

                            if let Some(v) = prev_value {
                                assert_eq!(v, j);
                            }
                        }
                    })
                })
                .collect();

            for result in threads.into_iter().map(std::thread::JoinHandle::join) {
                assert!(result.is_ok());
            }

            assert!(map.is_empty());
            assert_eq!(map.len(), 0);

            for i in 0..MAX_VALUE {
                assert_eq!(map.get(&i), None);
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn drop_value() {
            let key_parent = std::sync::Arc::new($crate::test_util::DropNotifier::new());
            let value_parent = std::sync::Arc::new($crate::test_util::DropNotifier::new());

            {
                let map = $m::new();

                assert_eq!(
                    map.insert_and(
                        $crate::test_util::NoisyDropper::new(std::sync::Arc::clone(&key_parent), 0),
                        $crate::test_util::NoisyDropper::new(
                            std::sync::Arc::clone(&value_parent),
                            0
                        ),
                        |_| ()
                    ),
                    None
                );
                assert!(!map.is_empty());
                assert_eq!(map.len(), 1);
                map.get_and(&0, |v| assert_eq!(v, &0));

                map.remove_and(&0, |v| assert_eq!(v, &0));
                assert!(map.is_empty());
                assert_eq!(map.len(), 0);
                assert_eq!(map.get_and(&0, |_| ()), None);

                $crate::test_util::run_deferred();

                assert!(!key_parent.was_dropped());
                assert!(value_parent.was_dropped());
            }

            $crate::test_util::run_deferred();

            assert!(key_parent.was_dropped());
            assert!(value_parent.was_dropped());
        }

        #[test]
        fn drop_many_values() {
            const NUM_VALUES: usize = 1 << 16;

            let key_parents: Vec<_> = std::iter::repeat_with(|| {
                std::sync::Arc::new($crate::test_util::DropNotifier::new())
            })
            .take(NUM_VALUES)
            .collect();
            let value_parents: Vec<_> = std::iter::repeat_with(|| {
                std::sync::Arc::new($crate::test_util::DropNotifier::new())
            })
            .take(NUM_VALUES)
            .collect();

            {
                let map = $m::new();
                assert!(map.is_empty());
                assert_eq!(map.len(), 0);

                for (i, (this_key_parent, this_value_parent)) in
                    key_parents.iter().zip(value_parents.iter()).enumerate()
                {
                    assert_eq!(
                        map.insert_and(
                            $crate::test_util::NoisyDropper::new(
                                std::sync::Arc::clone(&this_key_parent),
                                i
                            ),
                            $crate::test_util::NoisyDropper::new(
                                std::sync::Arc::clone(&this_value_parent),
                                i
                            ),
                            |_| ()
                        ),
                        None
                    );

                    assert!(!map.is_empty());
                    assert_eq!(map.len(), i + 1);
                }

                for i in 0..NUM_VALUES {
                    assert_eq!(
                        map.get_key_value_and(&i, |k, v| {
                            assert_eq!(*k, i);
                            assert_eq!(*v, i);
                        }),
                        Some(())
                    );
                }

                for i in 0..NUM_VALUES {
                    assert_eq!(
                        map.remove_entry_and(&i, |k, v| {
                            assert_eq!(*k, i);
                            assert_eq!(*v, i);
                        }),
                        Some(())
                    );
                }

                assert!(map.is_empty());
                assert_eq!(map.len(), 0);

                $crate::test_util::run_deferred();

                for this_key_parent in key_parents.iter() {
                    assert!(!this_key_parent.was_dropped());
                }

                for this_value_parent in value_parents.iter() {
                    assert!(this_value_parent.was_dropped());
                }

                for i in 0..NUM_VALUES {
                    assert_eq!(map.get_and(&i, |_| ()), None);
                }
            }

            $crate::test_util::run_deferred();

            for this_key_parent in key_parents.into_iter() {
                assert!(this_key_parent.was_dropped());
            }

            for this_value_parent in value_parents.into_iter() {
                assert!(this_value_parent.was_dropped());
            }
        }

        #[test]
        fn drop_many_values_concurrent() {
            const NUM_THREADS: usize = 64;
            const NUM_VALUES_PER_THREAD: usize = 512;
            const NUM_VALUES: usize = NUM_THREADS * NUM_VALUES_PER_THREAD;

            let key_parents: std::sync::Arc<Vec<_>> = std::sync::Arc::new(
                std::iter::repeat_with(|| {
                    std::sync::Arc::new($crate::test_util::DropNotifier::new())
                })
                .take(NUM_VALUES)
                .collect(),
            );
            let value_parents: std::sync::Arc<Vec<_>> = std::sync::Arc::new(
                std::iter::repeat_with(|| {
                    std::sync::Arc::new($crate::test_util::DropNotifier::new())
                })
                .take(NUM_VALUES)
                .collect(),
            );

            {
                let map = std::sync::Arc::new($m::new());
                assert!(map.is_empty());
                assert_eq!(map.len(), 0);

                let barrier = std::sync::Arc::new(std::sync::Barrier::new(NUM_THREADS));

                let handles: Vec<_> = (0..NUM_THREADS)
                    .map(|i| {
                        let map = std::sync::Arc::clone(&map);
                        let barrier = std::sync::Arc::clone(&barrier);
                        let key_parents = std::sync::Arc::clone(&key_parents);
                        let value_parents = std::sync::Arc::clone(&value_parents);

                        std::thread::spawn(move || {
                            barrier.wait();

                            let these_key_parents = &key_parents
                                [i * NUM_VALUES_PER_THREAD..(i + 1) * NUM_VALUES_PER_THREAD];
                            let these_value_parents = &value_parents
                                [i * NUM_VALUES_PER_THREAD..(i + 1) * NUM_VALUES_PER_THREAD];

                            for (j, (this_key_parent, this_value_parent)) in these_key_parents
                                .iter()
                                .zip(these_value_parents.iter())
                                .enumerate()
                            {
                                let key_value = i * NUM_VALUES_PER_THREAD + j;

                                assert_eq!(
                                    map.insert_and(
                                        $crate::test_util::NoisyDropper::new(
                                            std::sync::Arc::clone(&this_key_parent),
                                            key_value as i32
                                        ),
                                        $crate::test_util::NoisyDropper::new(
                                            std::sync::Arc::clone(&this_value_parent),
                                            key_value as i32
                                        ),
                                        |_| ()
                                    ),
                                    None
                                );
                            }
                        })
                    })
                    .collect();

                for result in handles.into_iter().map(std::thread::JoinHandle::join) {
                    assert!(result.is_ok());
                }

                assert!(!map.is_empty());
                assert_eq!(map.len(), NUM_VALUES);

                $crate::test_util::run_deferred();

                for this_key_parent in key_parents.iter() {
                    assert!(!this_key_parent.was_dropped());
                }

                for this_value_parent in value_parents.iter() {
                    assert!(!this_value_parent.was_dropped());
                }

                for i in (0..NUM_VALUES).map(|i| i as i32) {
                    assert_eq!(
                        map.get_key_value_and(&i, |k, v| {
                            assert_eq!(*k, i);
                            assert_eq!(*v, i);
                        }),
                        Some(())
                    );
                }

                let handles: Vec<_> = (0..NUM_THREADS)
                    .map(|i| {
                        let map = std::sync::Arc::clone(&map);
                        let barrier = std::sync::Arc::clone(&barrier);

                        std::thread::spawn(move || {
                            barrier.wait();

                            for j in 0..NUM_VALUES_PER_THREAD {
                                let key_value = (i * NUM_VALUES_PER_THREAD + j) as i32;

                                assert_eq!(
                                    map.remove_entry_and(&key_value, |k, v| {
                                        assert_eq!(*k, key_value);
                                        assert_eq!(*v, key_value);
                                    }),
                                    Some(())
                                );
                            }
                        })
                    })
                    .collect();

                for result in handles.into_iter().map(std::thread::JoinHandle::join) {
                    assert!(result.is_ok());
                }

                assert!(map.is_empty());
                assert_eq!(map.len(), 0);

                $crate::test_util::run_deferred();

                for this_key_parent in key_parents.iter() {
                    assert!(!this_key_parent.was_dropped());
                }

                for this_value_parent in value_parents.iter() {
                    assert!(this_value_parent.was_dropped());
                }

                for i in (0..NUM_VALUES).map(|i| i as i32) {
                    assert_eq!(map.get_and(&i, |_| ()), None);
                }
            }

            $crate::test_util::run_deferred();

            for this_key_parent in key_parents.iter() {
                assert!(this_key_parent.was_dropped());
            }

            for this_value_parent in value_parents.iter() {
                assert!(this_value_parent.was_dropped());
            }
        }

        #[test]
        fn remove_if() {
            const NUM_VALUES: i32 = 512;

            let is_even = |_: &i32, v: &i32| *v % 2 == 0;

            let map = $m::new();

            for i in 0..NUM_VALUES {
                assert_eq!(map.insert(i, i), None);
            }

            for i in 0..NUM_VALUES {
                if is_even(&i, &i) {
                    assert_eq!(map.remove_if(&i, is_even), Some(i));
                } else {
                    assert_eq!(map.remove_if(&i, is_even), None);
                }
            }

            for i in (0..NUM_VALUES).filter(|i| i % 2 == 0) {
                assert_eq!(map.get(&i), None);
            }

            for i in (0..NUM_VALUES).filter(|i| i % 2 != 0) {
                assert_eq!(map.get(&i), Some(i));
            }

            $crate::test_util::run_deferred();
        }

        #[test]
        fn default() {
            let map = $m::<_, _, $crate::map::DefaultHashBuilder>::default();

            assert!(map.is_empty());
            assert_eq!(map.len(), 0);

            assert_eq!(map.insert("foo", 5), None);
            assert_eq!(map.insert("bar", 10), None);
            assert_eq!(map.insert("baz", 15), None);
            assert_eq!(map.insert("qux", 20), None);

            assert!(!map.is_empty());
            assert_eq!(map.len(), 4);

            assert_eq!(map.insert("foo", 5), Some(5));
            assert_eq!(map.insert("bar", 10), Some(10));
            assert_eq!(map.insert("baz", 15), Some(15));
            assert_eq!(map.insert("qux", 20), Some(20));

            assert!(!map.is_empty());
            assert_eq!(map.len(), 4);

            assert_eq!(map.remove("foo"), Some(5));
            assert_eq!(map.remove("bar"), Some(10));
            assert_eq!(map.remove("baz"), Some(15));
            assert_eq!(map.remove("qux"), Some(20));

            assert!(map.is_empty());
            assert_eq!(map.len(), 0);

            $crate::test_util::run_deferred();
        }
    };
}
