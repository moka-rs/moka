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

use std::{
    ptr::NonNull,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

use super::{
    concurrent::entry_info::EntryInfo,
    deque::{DeqNode, Deque},
    time::{CheckedTimeOps, Instant},
};

use triomphe::Arc as TrioArc;

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
    BUCKET_COUNTS[3] * aligned_duration(DAY),       // 6.5d
    BUCKET_COUNTS[3] * aligned_duration(DAY),       // 6.5d
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

pub(crate) struct TimerNode<K> {
    level: AtomicU8, // When unset, we use `u8::MAX`.
    index: AtomicU8, // When unset, we use `u8::MAX`.
    entry_info: TrioArc<EntryInfo<K>>,
}

impl<K> TimerNode<K> {
    fn new(entry_info: TrioArc<EntryInfo<K>>, level: usize, index: usize) -> Self {
        Self {
            level: AtomicU8::new(level as u8),
            index: AtomicU8::new(index as u8),
            entry_info,
        }
    }

    pub(crate) fn entry_info(&self) -> &TrioArc<EntryInfo<K>> {
        &self.entry_info
    }
}

type Bucket<K> = Deque<TimerNode<K>>;

#[must_use = "this `ReschedulingResult` may be an `Removed` variant, which should be handled"]
pub(crate) enum ReschedulingResult<K> {
    /// The timer event was rescheduled.
    Rescheduled,
    /// The timer event was not rescheduled because the entry has no expiration time.
    Removed(Box<DeqNode<TimerNode<K>>>),
}

/// A hierarchical timer wheel to add, remove, and fire expiration events in
/// amortized O(1) time.
///
/// The expiration events are deferred until the timer is advanced, which is
/// performed as part of the cache's housekeeping cycle.
pub(crate) struct TimerWheel<K> {
    wheels: Box<[Box<[Bucket<K>]>]>,
    /// The time when this timer wheel was created.
    origin: Instant,
    /// The time when this timer wheel was last advanced.
    current: Instant,
}

#[cfg(feature = "future")]
// TODO: https://github.com/moka-rs/moka/issues/54
#[allow(clippy::non_send_fields_in_send_ty)]
// Multi-threaded async runtimes require base_cache::Inner to be Send, but it will
// not be without this `unsafe impl`. This is because DeqNodes have NonNull
// pointers.
unsafe impl<K> Send for TimerWheel<K> {}

impl<K> TimerWheel<K> {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            wheels: Default::default(), // Empty.
            origin: now,
            current: now,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_origin(&mut self, time: Instant) {
        self.origin = time;
        self.current = time;
    }

    pub(crate) fn is_enabled(&self) -> bool {
        !self.wheels.is_empty()
    }

    pub(crate) fn enable(&mut self) {
        assert!(!self.is_enabled());

        self.wheels = BUCKET_COUNTS
            .iter()
            .map(|b| {
                (0..*b)
                    .map(|_| Deque::new(super::CacheRegion::Other))
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
    }

    /// Schedules a timer event for the node.
    pub(crate) fn schedule(
        &mut self,
        entry_info: TrioArc<EntryInfo<K>>,
    ) -> Option<NonNull<DeqNode<TimerNode<K>>>> {
        debug_assert!(self.is_enabled());

        if let Some(t) = entry_info.expiration_time() {
            let (level, index) = self.bucket_indices(t);
            let node = Box::new(DeqNode::new(TimerNode::new(entry_info, level, index)));
            let node = self.wheels[level][index].push_back(node);
            Some(node)
        } else {
            None
        }
    }

    fn schedule_existing_node(
        &mut self,
        node: NonNull<DeqNode<TimerNode<K>>>,
    ) -> ReschedulingResult<K> {
        debug_assert!(self.is_enabled());

        // Since cache entry's ValueEntry has a pointer to this node, we must reuse
        // the node.
        let elem = &unsafe { node.as_ref() }.element;
        if let Some(t) = elem.entry_info.expiration_time() {
            let (level, index) = self.bucket_indices(t);
            elem.level.store(level as u8, Ordering::Release);
            elem.index.store(index as u8, Ordering::Release);
            let node = unsafe { Box::from_raw(node.as_ptr()) };
            self.wheels[level][index].push_back(node);
            ReschedulingResult::Rescheduled
        } else {
            // Unset the level and index.
            elem.level.store(u8::MAX, Ordering::Release);
            elem.index.store(u8::MAX, Ordering::Release);
            ReschedulingResult::Removed(unsafe { Box::from_raw(node.as_ptr()) })
        }
    }

    /// Reschedules an active timer event for the node.
    pub(crate) fn reschedule(
        &mut self,
        node: NonNull<DeqNode<TimerNode<K>>>,
    ) -> ReschedulingResult<K> {
        debug_assert!(self.is_enabled());
        unsafe { self.unlink_timer(node) };
        self.schedule_existing_node(node)
    }

    /// Removes a timer event for this node if present.
    pub(crate) fn deschedule(&mut self, node: NonNull<DeqNode<TimerNode<K>>>) {
        debug_assert!(self.is_enabled());
        unsafe {
            self.unlink_timer(node);
            Self::drop_node(node);
        }
    }

    /// Removes a timer event for this node if present.
    ///
    /// IMPORTANT: This method does not drop the node.
    unsafe fn unlink_timer(&mut self, node: NonNull<DeqNode<TimerNode<K>>>) {
        let p = unsafe { node.as_ref() };
        let level = p.element.level.load(Ordering::Acquire);
        let index = p.element.index.load(Ordering::Acquire);
        if level != u8::MAX && index != u8::MAX {
            self.wheels[level as usize][index as usize].unlink(node);
        }
    }

    unsafe fn drop_node(node: NonNull<DeqNode<TimerNode<K>>>) {
        std::mem::drop(Box::from_raw(node.as_ptr()));
    }

    /// Advances the timer wheel to the current time, and returns an iterator over
    /// timer events.
    pub(crate) fn advance(
        &mut self,
        current_time: Instant,
    ) -> impl Iterator<Item = TimerEvent<K>> + '_ {
        debug_assert!(self.is_enabled());

        let previous_time = self.current;
        self.current = current_time;
        TimerEventsIter::new(self, previous_time, current_time)
    }

    /// Returns a pointer to the timer event (cache entry) at the front of the queue.
    fn pop_timer_node(&mut self, level: usize, index: usize) -> Option<Box<DeqNode<TimerNode<K>>>> {
        self.wheels[level][index].pop_front()
    }

    /// Returns the bucket indices to locate the bucket that the timer event
    /// should be added to.
    fn bucket_indices(&self, time: Instant) -> (usize, usize) {
        let duration = time
            .checked_duration_since(self.current)
            // FIXME: unwrap will panic if the time is earlier than self.current.
            .unwrap()
            .as_nanos() as u64;
        let time_nanos = self.time_nanos(time);
        for level in 0..=NUM_LEVELS {
            if duration < SPANS[level + 1] {
                let ticks = time_nanos >> SHIFT[level];
                let index = ticks & (BUCKET_COUNTS[level] - 1);
                return (level, index as usize);
            }
        }
        (OVERFLOW_QUEUE_INDEX, 0)
    }

    // Nano-seconds since the timer wheel was created.
    fn time_nanos(&self, time: Instant) -> u64 {
        // ENHANCEME: Check overflow? (u128 -> u64)
        // FIXME: unwrap will panic if the time is earlier than self.origin.
        time.checked_duration_since(self.origin).unwrap().as_nanos() as u64
    }
}

/// A timer event, which is either an expired/rescheduled cache entry, or a
/// descheduled timer. `TimerWheel::advance` returns an iterator over timer events.
#[derive(Debug)]
pub(crate) enum TimerEvent<K> {
    /// This cache entry has expired.
    Expired(Box<DeqNode<TimerNode<K>>>),
    // This cache entry has been rescheduled. Rescheduling includes moving a timer
    // from one wheel to another in a lower level of the hierarchy. (This variant
    // is mainly used for testing)
    Rescheduled(TrioArc<EntryInfo<K>>),
    /// This timer node (containing a cache entry) has been removed from the timer.
    Descheduled(Box<DeqNode<TimerNode<K>>>),
}

/// An iterator over expired cache entries.
pub(crate) struct TimerEventsIter<'iter, K> {
    timer_wheel: &'iter mut TimerWheel<K>,
    previous_time: Instant,
    current_time: Instant,
    is_done: bool,
    level: usize,
    index: u8,
    end_index: u8,
    index_mask: u64,
    is_index_set: bool,
}

impl<'iter, K> TimerEventsIter<'iter, K> {
    fn new(
        timer_wheel: &'iter mut TimerWheel<K>,
        previous_time: Instant,
        current_time: Instant,
    ) -> Self {
        Self {
            timer_wheel,
            previous_time,
            current_time,
            is_done: false,
            level: 0,
            index: 0,
            end_index: 0,
            index_mask: 0,
            is_index_set: false,
        }
    }
}

impl<'iter, K> Drop for TimerEventsIter<'iter, K> {
    fn drop(&mut self) {
        // If dropped without completely consuming this iterator, reset the timer
        // wheel's current time to the previous time.
        if !self.is_done {
            self.timer_wheel.current = self.previous_time;
        }
    }
}

impl<'iter, K> Iterator for TimerEventsIter<'iter, K> {
    type Item = TimerEvent<K>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_done {
            return None;
        }

        loop {
            // Ensure that the index for the current level is set.
            if !self.is_index_set {
                let previous_time_nanos = self.timer_wheel.time_nanos(self.previous_time);
                let current_time_nanos = self.timer_wheel.time_nanos(self.current_time);
                let previous_ticks = previous_time_nanos >> SHIFT[self.level];
                let current_ticks = current_time_nanos >> SHIFT[self.level];

                if current_ticks <= previous_ticks {
                    self.is_done = true;
                    return None;
                }

                self.index_mask = BUCKET_COUNTS[self.level] - 1;
                self.index = (previous_ticks & self.index_mask) as u8;
                let steps =
                    (current_ticks - previous_ticks + 1).min(BUCKET_COUNTS[self.level]) as u8;
                self.end_index = self.index + steps;

                self.is_index_set = true;

                // dbg!(self.level, self.index, self.end_index);
            }

            // Pop the next timer event (cache entry) from the current level and
            // index.
            let i = self.index & self.index_mask as u8;
            match self.timer_wheel.pop_timer_node(self.level, i as usize) {
                Some(node) => {
                    let expiration_time = node.as_ref().element.entry_info().expiration_time();
                    if let Some(t) = expiration_time {
                        if t <= self.current_time {
                            // The cache entry has expired. Return it.
                            return Some(TimerEvent::Expired(node));
                        } else {
                            // The cache entry has not expired. Reschedule it.
                            let node_p = NonNull::new(Box::into_raw(node)).expect("Got a null ptr");
                            match self.timer_wheel.schedule_existing_node(node_p) {
                                ReschedulingResult::Rescheduled => {
                                    let entry_info =
                                        unsafe { node_p.as_ref() }.element.entry_info();
                                    return Some(TimerEvent::Rescheduled(TrioArc::clone(
                                        entry_info,
                                    )));
                                }
                                ReschedulingResult::Removed(node) => {
                                    // The timer event has been removed from the timer
                                    // wheel. Return it, so that the caller can remove the
                                    // pointer to the node from a `ValueEntry`.
                                    return Some(TimerEvent::Descheduled(node));
                                }
                            }
                        }
                    }
                }
                // Done with the current level and index. Move to the next index or
                // next level.
                None => {
                    self.index += 1;
                    if self.index >= self.end_index {
                        self.level += 1;
                        // No more levels to process. We are done.
                        if self.level >= BUCKET_COUNTS.len() {
                            self.is_done = true;
                            return None;
                        }
                        self.is_index_set = false;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::{TimerEvent, TimerWheel, SPANS};
    use crate::common::{
        concurrent::{entry_info::EntryInfo, KeyHash},
        time::{CheckedTimeOps, Clock, Instant, Mock},
    };

    use triomphe::Arc as TrioArc;

    #[test]
    fn test_bucket_indices() {
        fn bi(timer: &TimerWheel<()>, now: Instant, dur: Duration) -> (usize, usize) {
            let t = now.checked_add(dur).unwrap();
            timer.bucket_indices(t)
        }

        let (clock, mock) = Clock::mock();
        let now = now(&clock);

        let mut timer = TimerWheel::<()>::new(now);
        timer.enable();

        assert_eq!(timer.bucket_indices(now), (0, 0));

        // Level 0: 1.07s
        assert_eq!(bi(&timer, now, n2d(SPANS[0] - 1)), (0, 0));
        assert_eq!(bi(&timer, now, n2d(SPANS[0])), (0, 1));
        assert_eq!(bi(&timer, now, n2d(SPANS[0] * 63)), (0, 63));

        // Level 1: 1.14m
        assert_eq!(bi(&timer, now, n2d(SPANS[0] * 64)), (1, 1));
        assert_eq!(bi(&timer, now, n2d(SPANS[1])), (1, 1));
        assert_eq!(bi(&timer, now, n2d(SPANS[1] * 63 + SPANS[0] * 63)), (1, 63));

        // Level 2: 1.22h
        assert_eq!(bi(&timer, now, n2d(SPANS[1] * 64)), (2, 1));
        assert_eq!(bi(&timer, now, n2d(SPANS[2])), (2, 1));
        assert_eq!(
            bi(
                &timer,
                now,
                n2d(SPANS[2] * 31 + SPANS[1] * 63 + SPANS[0] * 63)
            ),
            (2, 31)
        );

        // Level 3: 1.63dh
        assert_eq!(bi(&timer, now, n2d(SPANS[2] * 32)), (3, 1));
        assert_eq!(bi(&timer, now, n2d(SPANS[3])), (3, 1));
        assert_eq!(bi(&timer, now, n2d(SPANS[3] * 3)), (3, 3));

        // Overflow
        assert_eq!(bi(&timer, now, n2d(SPANS[3] * 4)), (4, 0));
        assert_eq!(bi(&timer, now, n2d(SPANS[4])), (4, 0));
        assert_eq!(bi(&timer, now, n2d(SPANS[4] * 100)), (4, 0));

        // Increment the clock by 5 ticks. (1 tick ~= 1.07s)
        let now = advance_clock(&clock, &mock, n2d(SPANS[0] * 5));
        timer.current = now;

        // Level 0: 1.07s
        assert_eq!(bi(&timer, now, n2d(SPANS[0] - 1)), (0, 5));
        assert_eq!(bi(&timer, now, n2d(SPANS[0])), (0, 6));
        assert_eq!(bi(&timer, now, n2d(SPANS[0] * 63)), (0, 4));

        // Level 1: 1.14m
        assert_eq!(bi(&timer, now, n2d(SPANS[0] * 64)), (1, 1));
        assert_eq!(bi(&timer, now, n2d(SPANS[1])), (1, 1));
        assert_eq!(
            bi(&timer, now, n2d(SPANS[1] * 63 + SPANS[0] * (63 - 5))),
            (1, 63)
        );

        // Increment the clock by 61 ticks. (total 66 ticks)
        let now = advance_clock(&clock, &mock, n2d(SPANS[0] * 61));
        timer.current = now;

        // Level 0: 1.07s
        assert_eq!(bi(&timer, now, n2d(SPANS[0] - 1)), (0, 2));
        assert_eq!(bi(&timer, now, n2d(SPANS[0])), (0, 3));
        assert_eq!(bi(&timer, now, n2d(SPANS[0] * 63)), (0, 1));

        // Level 1: 1.14m
        assert_eq!(bi(&timer, now, n2d(SPANS[0] * 64)), (1, 2));
        assert_eq!(bi(&timer, now, n2d(SPANS[1])), (1, 2));
        assert_eq!(
            bi(&timer, now, n2d(SPANS[1] * 63 + SPANS[0] * (63 - 2))),
            (1, 0)
        );
    }

    #[test]
    fn test_advance() {
        fn schedule_timer(timer: &mut TimerWheel<u32>, key: u32, now: Instant, ttl: Duration) {
            let hash = key as u64;
            let key_hash = KeyHash::new(Arc::new(key), hash);
            let policy_weight = 0;
            let entry_info = TrioArc::new(EntryInfo::new(key_hash, now, policy_weight));
            entry_info.set_expiration_time(now.checked_add(ttl).unwrap());
            timer.schedule(entry_info);
        }

        fn expired_key(maybe_entry: Option<TimerEvent<u32>>) -> u32 {
            let entry = maybe_entry.expect("entry is none");
            match entry {
                TimerEvent::Expired(node) => *node.element.entry_info().key_hash().key,
                _ => panic!("Expected an expired entry. Got {:?}", entry),
            }
        }

        fn rescheduled_key(maybe_entry: Option<TimerEvent<u32>>) -> u32 {
            let entry = maybe_entry.expect("entry is none");
            match entry {
                TimerEvent::Rescheduled(entry) => *entry.key_hash().key,
                _ => panic!("Expected a rescheduled entry. Got {:?}", entry),
            }
        }

        let (clock, mock) = Clock::mock();
        let now = advance_clock(&clock, &mock, s2d(10));

        let mut timer = TimerWheel::<u32>::new(now);
        timer.enable();

        // Add timers that will expire in some seconds.
        schedule_timer(&mut timer, 1, now, s2d(5));
        schedule_timer(&mut timer, 2, now, s2d(1));
        schedule_timer(&mut timer, 3, now, s2d(63));
        schedule_timer(&mut timer, 4, now, s2d(3));

        let now = advance_clock(&clock, &mock, s2d(4));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 2);
        assert_eq!(expired_key(expired_entries.next()), 4);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d(4));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 1);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d(64 - 8));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 3);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        // Add timers that will expire in some minutes.
        const MINUTES: u64 = 60;
        schedule_timer(&mut timer, 1, now, s2d(5 * MINUTES));
        #[allow(clippy::identity_op)]
        schedule_timer(&mut timer, 2, now, s2d(1 * MINUTES));
        schedule_timer(&mut timer, 3, now, s2d(63 * MINUTES));
        schedule_timer(&mut timer, 4, now, s2d(3 * MINUTES));

        let now = advance_clock(&clock, &mock, s2d(4 * MINUTES));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 2);
        assert_eq!(expired_key(expired_entries.next()), 4);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d(4 * MINUTES));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 1);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d((64 - 8) * MINUTES));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 3);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        // Add timers that will expire in some hours.
        const HOURS: u64 = 60 * 60;
        schedule_timer(&mut timer, 1, now, s2d(5 * HOURS));
        #[allow(clippy::identity_op)]
        schedule_timer(&mut timer, 2, now, s2d(1 * HOURS));
        schedule_timer(&mut timer, 3, now, s2d(31 * HOURS));
        schedule_timer(&mut timer, 4, now, s2d(3 * HOURS));

        let now = advance_clock(&clock, &mock, s2d(4 * HOURS));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 2);
        assert_eq!(expired_key(expired_entries.next()), 4);
        assert_eq!(rescheduled_key(expired_entries.next()), 1);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d(4 * HOURS));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 1);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d((32 - 8) * HOURS));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 3);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        // Add timers that will expire in a few days.
        const DAYS: u64 = 24 * 60 * 60;
        schedule_timer(&mut timer, 1, now, s2d(5 * DAYS));
        #[allow(clippy::identity_op)]
        schedule_timer(&mut timer, 2, now, s2d(1 * DAYS));
        schedule_timer(&mut timer, 3, now, s2d(2 * DAYS));
        // Longer than ~6.5 days, so this should be stored in the overflow area.
        schedule_timer(&mut timer, 4, now, s2d(8 * DAYS));

        let now = advance_clock(&clock, &mock, s2d(3 * DAYS));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 2);
        assert_eq!(expired_key(expired_entries.next()), 3);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d(3 * DAYS));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 1);
        assert_eq!(rescheduled_key(expired_entries.next()), 4);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);

        let now = advance_clock(&clock, &mock, s2d(3 * DAYS));
        let mut expired_entries = timer.advance(now);
        assert_eq!(expired_key(expired_entries.next()), 4);
        assert!(expired_entries.next().is_none());
        drop(expired_entries);
    }

    //
    // Utility functions
    //

    fn now(clock: &Clock) -> Instant {
        Instant::new(clock.now())
    }

    fn advance_clock(clock: &Clock, mock: &Arc<Mock>, duration: Duration) -> Instant {
        mock.increment(duration);
        now(clock)
    }

    /// Convert nano-seconds to duration.
    fn n2d(nanos: u64) -> Duration {
        Duration::from_nanos(nanos)
    }

    /// Convert seconds to duration.
    fn s2d(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }
}
