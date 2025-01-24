use std::sync::Arc;

use crate::{
    common::deque::DeqNode,
    policy::{EntrySnapshot, EntrySnapshotConfig, ExpirationPolicy},
};

use super::{
    concurrent::{deques::Deques, KeyHashDateNodePtr},
    entry::EntryMetadata,
    time::Clock,
};

impl<K, V> ExpirationPolicy<K, V> {
    pub(crate) fn capture_entry_snapshot(
        &self,
        sc: EntrySnapshotConfig,
        deqs: &Deques<K>,
        clock: &Clock,
    ) -> EntrySnapshot<K> {
        use crate::entry::EntryRegion;

        let coldest_entries = if sc.coldest == 0 {
            Vec::new()
        } else {
            self.top_entries(
                EntryRegion::Main,
                sc.coldest,
                deqs.probation.peek_front_ptr(),
                DeqNode::next_node_ptr,
                clock,
            )
        };

        let hottest_entries = if sc.hottest == 0 {
            Vec::new()
        } else {
            self.top_entries(
                EntryRegion::Main,
                sc.hottest,
                deqs.probation.peek_back_ptr(),
                DeqNode::prev_node_ptr,
                clock,
            )
        };

        EntrySnapshot::new(coldest_entries, hottest_entries)
    }

    fn top_entries(
        &self,
        region: crate::entry::EntryRegion,
        count: usize,
        head: Option<KeyHashDateNodePtr<K>>,
        next_fun: fn(KeyHashDateNodePtr<K>) -> Option<KeyHashDateNodePtr<K>>,
        clock: &Clock,
    ) -> Vec<(Arc<K>, EntryMetadata)> {
        let mut entries = Vec::with_capacity(count);

        let mut next = head;
        while entries.len() < count {
            let Some(current) = next.take() else {
                break;
            };
            next = next_fun(current);

            let elem = &unsafe { current.as_ref() }.element;
            if elem.is_dirty() {
                continue;
            }

            let snapshot_at = clock.to_std_instant(clock.now());
            let md = EntryMetadata::from_element(
                region,
                elem,
                clock,
                self.time_to_live(),
                self.time_to_idle(),
                snapshot_at,
            );
            entries.push((Arc::clone(elem.key()), md));
        }

        entries
    }
}
