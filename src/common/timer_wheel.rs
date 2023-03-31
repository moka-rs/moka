// License and Copyright Notice:
//
// Some of the code and doc comments in this module were ported or copied from
// a Java class `com.github.benmanes.caffeine.cache.TimerWheel` of Caffeine.
// https://github.com/ben-manes/caffeine/blob/master/caffeine/src/main/java/com/github/benmanes/caffeine/cache/TimerWheel.java
//
// The original code/comments from Caffeine are licensed under the Apache License,
// Version 2.0 <https://github.com/ben-manes/caffeine/blob/master/LICENSE>
//
// Copyrights of the original code/comments are retained by their contributors.
// For full authorship information, see the version control history of
// https://github.com/ben-manes/caffeine/

#![allow(unused)] // TODO: Remove this.

use std::{convert::TryInto, ptr::NonNull, time::Duration};

use super::{
    deque::{DeqNode, Deque},
    time::{CheckedTimeOps, Instant},
};

const BUCKET_COUNTS: &[u64] = &[
    64, // roughly seconds
    64, // roughly minutes
    32, // roughly hours
    4,  // roughly days
    1,  // overflow (> ~6.5 days)
];

const OVERFLOW_QUEUE_INDEX: usize = BUCKET_COUNTS.len() - 1;
const NUM_LEVELS: usize = OVERFLOW_QUEUE_INDEX - 1;

const DAY: Duration = Duration::from_secs(60 * 60 * 24);

const SPANS: &[u64] = &[
    aligned_duration(Duration::from_secs(1)),       // 1.07s
    aligned_duration(Duration::from_secs(60)),      // 1.14m
    aligned_duration(Duration::from_secs(60 * 60)), // 1.22h
    aligned_duration(DAY),                          // 1.63d
    BUCKET_COUNTS[3] * aligned_duration(DAY),        // 6.5d
    BUCKET_COUNTS[3] * aligned_duration(DAY),        // 6.5d
];

const SHIFT: &[u64] = &[
    SPANS[0].trailing_zeros() as u64,
    SPANS[1].trailing_zeros() as u64,
    SPANS[2].trailing_zeros() as u64,
    SPANS[3].trailing_zeros() as u64,
    SPANS[4].trailing_zeros() as u64,
];

/// Returns the next power of two of the duration in nanoseconds.
const fn aligned_duration(duration: Duration) -> u64 {
    // NOTE: as_nanos() returns u128, so convert it to u64 by using `as`.
    // We cannot call TryInto::try_into() here because it is not a const fn.
    (duration.as_nanos() as u64).next_power_of_two()
}

/// A hierarchical timer wheel to add, remove, and fire expiration events in
/// amortized O(1) time.
///
/// The expiration events are deferred until the timer is advanced, which is
/// performed as part of the cache's housekeeping cycle.
pub(crate) struct TimerWheel<T> {
    wheels: Box<[Box<[Deque<T>]>]>,
    /// The time when this timer wheel was created.
    origin: Instant,
    /// The time when this timer wheel was last advanced.
    current: Instant,
}

impl<T> TimerWheel<T> {
    fn new(now: Instant) -> Self {
        let wheels = BUCKET_COUNTS
            .iter()
            .map(|b| {
                (0..*b)
                    .map(|_| Deque::new(super::CacheRegion::Other))
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            wheels,
            origin: now,
            current: now,
        }
    }

    /// Schedules a timer event for the node.
    // pub(crate) fn schedule(&mut self, node: Box<DeqNode<T>>) {
    //     if let Some(t) = node.element.expiration_time() {
    //         let (level, index) = self.bucket_indices(t);
    //         self.wheels[level][index].push_back(node);
    //     }
    // }

    // /// Reschedules an active timer event for the node.
    // pub(crate) fn reschedule(&mut self, node: NonNull<DeqNode<T>>) {}

    /// Removes a timer event for this node if present.
    // pub(crate) fn deschedule(&mut self, node: NonNull<DeqNode<T>>) {
    //     if let Some(t) = node.element.expiration_time() {
    //         let (level, index) = self.bucket_indices(t);
    //         unsafe { self.wheels[level][index].unlink_and_drop(node) };
    //     }
    // }

    /// Returns the bucket indices to locate the bucket that the timer event
    /// should be added to.
    fn bucket_indices(&self, time: Instant) -> (usize, usize) {
        let duration = time
            .checked_duration_since(self.current)
            // FIXME: unwrap will panic if the time is earlier than self.current.
            .unwrap()
            .as_nanos() as u64;
        // ENHANCEME: Check overflow? (u128 -> u64)
        // FIXME: unwrap will panic if the time is earlier than self.origin.
        let time_nano = time.checked_duration_since(self.origin).unwrap().as_nanos() as u64;
        for level in 0..=NUM_LEVELS {
            if duration < SPANS[level + 1] {
                let ticks = time_nano >> SHIFT[level];
                let index = ticks & (BUCKET_COUNTS[level] - 1);
                return (level, index as usize);
            }
        }
        (OVERFLOW_QUEUE_INDEX, 0)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{TimerWheel, SPANS};
    use crate::common::time::{CheckedTimeOps, Clock, Instant};

    #[test]
    fn test_bucket_indices() {
        fn dur(nanos: u64) -> Duration {
            Duration::from_nanos(nanos)
        }

        fn bi(timer: &TimerWheel<()>, now: Instant, dur: Duration) -> (usize, usize) {
            let t = now.checked_add(dur).unwrap();
            timer.bucket_indices(t)
        }

        let (clock, mock) = Clock::mock();
        let now = Instant::new(clock.now());

        let mut timer = TimerWheel::<()>::new(now);
        assert_eq!(timer.bucket_indices(now), (0, 0));

        // Level 0: 1.07s
        assert_eq!(bi(&timer, now, dur(SPANS[0] - 1)), (0, 0));
        assert_eq!(bi(&timer, now, dur(SPANS[0])), (0, 1));
        assert_eq!(bi(&timer, now, dur(SPANS[0] * 63)), (0, 63));

        // Level 1: 1.14m
        assert_eq!(bi(&timer, now, dur(SPANS[0] * 64)), (1, 1));
        assert_eq!(bi(&timer, now, dur(SPANS[1])), (1, 1));
        assert_eq!(bi(&timer, now, dur(SPANS[1] * 63 + SPANS[0] * 63)), (1, 63));

        // Level 2: 1.22h
        assert_eq!(bi(&timer, now, dur(SPANS[1] * 64)), (2, 1));
        assert_eq!(bi(&timer, now, dur(SPANS[2])), (2, 1));
        assert_eq!(
            bi(
                &timer,
                now,
                dur(SPANS[2] * 31 + SPANS[1] * 63 + SPANS[0] * 63)
            ),
            (2, 31)
        );

        // Level 3: 1.63dh
        assert_eq!(bi(&timer, now, dur(SPANS[2] * 32)), (3, 1));
        assert_eq!(bi(&timer, now, dur(SPANS[3])), (3, 1));
        assert_eq!(bi(&timer, now, dur(SPANS[3] * 3)), (3, 3));

        // Overflow
        assert_eq!(bi(&timer, now, dur(SPANS[3] * 4)), (4, 0));
        assert_eq!(bi(&timer, now, dur(SPANS[4])), (4, 0));
        assert_eq!(bi(&timer, now, dur(SPANS[4] * 100)), (4, 0));

        // Increment the clock by 5 ticks. (1 tick ~= 1.07s)
        mock.increment(dur(SPANS[0] * 5));
        let now = Instant::new(clock.now());
        timer.current = now;

        // Level 0: 1.07s
        assert_eq!(bi(&timer, now, dur(SPANS[0] - 1)), (0, 5));
        assert_eq!(bi(&timer, now, dur(SPANS[0])), (0, 6));
        assert_eq!(bi(&timer, now, dur(SPANS[0] * 63)), (0, 4));

        // Level 1: 1.14m
        assert_eq!(bi(&timer, now, dur(SPANS[0] * 64)), (1, 1));
        assert_eq!(bi(&timer, now, dur(SPANS[1])), (1, 1));
        assert_eq!(
            bi(&timer, now, dur(SPANS[1] * 63 + SPANS[0] * (63 - 5))),
            (1, 63)
        );

        // Increment the clock by 61 ticks. (total 66 ticks)
        mock.increment(dur(SPANS[0] * 61));
        let now = Instant::new(clock.now());
        timer.current = now;

        // Level 0: 1.07s
        assert_eq!(bi(&timer, now, dur(SPANS[0] - 1)), (0, 2));
        assert_eq!(bi(&timer, now, dur(SPANS[0])), (0, 3));
        assert_eq!(bi(&timer, now, dur(SPANS[0] * 63)), (0, 1));

        // Level 1: 1.14m
        assert_eq!(bi(&timer, now, dur(SPANS[0] * 64)), (1, 2));
        assert_eq!(bi(&timer, now, dur(SPANS[1])), (1, 2));
        assert_eq!(
            bi(&timer, now, dur(SPANS[1] * 63 + SPANS[0] * (63 - 2))),
            (1, 0)
        );
    }
}
