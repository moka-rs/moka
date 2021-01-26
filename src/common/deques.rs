use super::{
    deque::{CacheRegion, DeqNode, Deque},
    KeyHashDate, ValueEntry,
};

use std::{ptr::NonNull, sync::Arc};

pub(crate) struct Deques<K> {
    pub(crate) window: Deque<KeyHashDate<K>>, //    Not used yet.
    pub(crate) probation: Deque<KeyHashDate<K>>,
    pub(crate) protected: Deque<KeyHashDate<K>>, // Not used yet.
}

impl<K> Default for Deques<K> {
    fn default() -> Self {
        Self {
            window: Deque::new(CacheRegion::Window),
            probation: Deque::new(CacheRegion::MainProbation),
            protected: Deque::new(CacheRegion::MainProtected),
        }
    }
}

impl<K> Deques<K> {
    pub(crate) fn push_back<V>(
        &mut self,
        region: CacheRegion,
        kh: KeyHashDate<K>,
        entry: &Arc<ValueEntry<K, V>>,
    ) {
        use CacheRegion::*;
        let node = Box::new(DeqNode::new(region, kh));
        let node = match node.as_ref().region {
            Window => self.window.push_back(node),
            MainProbation => self.probation.push_back(node),
            MainProtected => self.protected.push_back(node),
        };
        unsafe { *(entry.deq_node.get()) = Some(node) };
    }

    pub(crate) fn move_to_back<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        use CacheRegion::*;
        unsafe {
            if let Some(node) = *entry.deq_node.get() {
                let p = node.as_ref();
                match &p.region {
                    Window if self.window.contains(p) => self.window.move_to_back(node),
                    MainProbation if self.probation.contains(p) => {
                        self.probation.move_to_back(node)
                    }
                    MainProtected if self.protected.contains(p) => {
                        self.protected.move_to_back(node)
                    }
                    region => eprintln!(
                        "move_to_back - node is not a member of {:?} deque. {:?}",
                        region, p
                    ),
                }
            }
        }
    }

    pub(crate) fn unlink<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        unsafe {
            if let Some(node) = (*entry.deq_node.get()).take() {
                self.unlink_node(node);
            }
        }
    }

    pub(crate) fn unlink_node(&mut self, node: NonNull<DeqNode<KeyHashDate<K>>>) {
        use CacheRegion::*;
        unsafe {
            let p = node.as_ref();
            match &p.region {
                Window if self.window.contains(p) => self.window.unlink(node),
                MainProbation if self.probation.contains(p) => self.probation.unlink(node),
                MainProtected if self.protected.contains(p) => self.protected.unlink(node),
                region => eprintln!(
                    "unlink_node - node is not a member of {:?} deque. {:?}",
                    region, p
                ),
            }
        }
    }
}
