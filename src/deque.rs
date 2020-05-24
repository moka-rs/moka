// Licence and Copyright Notice:
//
// Some of the code and doc comments in this module were copied from
// `std::collections::LinkedList` in the Rust standard library.
// https://github.com/rust-lang/rust/blob/master/src/liballoc/collections/linked_list.rs
//
// The original code/comments from LinkedList are dual-licensed under
// the Apache License, Version 2.0 <https://github.com/rust-lang/rust/blob/master/LICENSE-APACHE>
// or the MIT license <https://github.com/rust-lang/rust/blob/master/LICENSE-MIT>
//
// Copyrights of the original code/comments are retained by their contributors.
// For full authorship information, see the version control history of
// https://github.com/rust-lang/rust/ or https://thanks.rust-lang.org

use std::{marker::PhantomData, ptr::NonNull, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CacheRegion {
    Window,
    MainProbation,
    MainProtected,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeqNode<T> {
    pub(crate) region: CacheRegion,
    next: Option<NonNull<DeqNode<T>>>,
    prev: Option<NonNull<DeqNode<T>>>,
    pub(crate) element: Arc<T>,
}

impl<T> DeqNode<T> {
    pub(crate) fn new(region: CacheRegion, element: Arc<T>) -> Self {
        Self {
            region,
            next: None,
            prev: None,
            element,
        }
    }
}

pub(crate) struct Deque<T> {
    region: CacheRegion,
    len: usize,
    head: Option<NonNull<DeqNode<T>>>,
    tail: Option<NonNull<DeqNode<T>>>,
    marker: PhantomData<Box<DeqNode<T>>>,
}

impl<T> Drop for Deque<T> {
    fn drop(&mut self) {
        struct DropGuard<'a, T>(&'a mut Deque<T>);

        impl<'a, T> Drop for DropGuard<'a, T> {
            fn drop(&mut self) {
                // Continue the same loop we do below. This only runs when a destructor has
                // panicked. If another one panics this will abort.
                while self.0.pop_front().is_some() {}
            }
        }

        while let Some(node) = self.pop_front() {
            let guard = DropGuard(self);
            drop(node);
            std::mem::forget(guard);
        }
    }
}

// Inner crate public function/methods
impl<T> Deque<T> {
    pub(crate) fn new(region: CacheRegion) -> Self {
        Self {
            region,
            head: None,
            tail: None,
            len: 0,
            marker: PhantomData::default(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_member(&self, node: &DeqNode<T>) -> bool {
        self.region == node.region && (node.prev.is_some() || self.is_head(node))
    }

    /// Adds the given node to the front of the list.
    // pub(crate) fn push_front(&mut self, mut node: Box<DeqNode<T>>) {
    //     // This method takes care not to create mutable references to whole nodes,
    //     // to maintain validity of aliasing pointers into `element`.
    //     unsafe {
    //         node.next = self.head;
    //         node.prev = None;
    //         let node = Some(NonNull::new(Box::into_raw(node)).expect("Got a null ptr"));

    //         match self.head {
    //             None => self.tail = node,
    //             // Not creating new mutable (unique!) references overlapping `element`.
    //             Some(head) => (*head.as_ptr()).prev = node,
    //         }

    //         self.head = node;
    //         self.len += 1;
    //     }
    // }

    pub(crate) fn peek_front(&self) -> Option<&DeqNode<T>> {
        // This method takes care not to create mutable references to whole nodes,
        // to maintain validity of aliasing pointers into `element`.
        self.head.as_ref().map(|node| unsafe { node.as_ref() })
    }

    /// Removes and returns the node at the front of the list.
    pub(crate) fn pop_front(&mut self) -> Option<Box<DeqNode<T>>> {
        // This method takes care not to create mutable references to whole nodes,
        // to maintain validity of aliasing pointers into `element`.
        self.head.map(|node| unsafe {
            let node = Box::from_raw(node.as_ptr());
            self.head = node.next;

            match self.head {
                None => self.tail = None,
                // Not creating new mutable (unique!) references overlapping `element`.
                Some(head) => (*head.as_ptr()).prev = None,
            }

            self.len -= 1;
            node
        })
    }

    #[cfg(test)]
    fn peek_back(&mut self) -> Option<&DeqNode<T>> {
        // This method takes care not to create mutable references to whole nodes,
        // to maintain validity of aliasing pointers into `element`.
        self.tail.as_ref().map(|node| unsafe { node.as_ref() })
    }

    /// Adds the given node to the back of the list.
    pub(crate) fn push_back(&mut self, mut node: Box<DeqNode<T>>) -> NonNull<DeqNode<T>> {
        // This method takes care not to create mutable references to whole nodes,
        // to maintain validity of aliasing pointers into `element`.
        unsafe {
            node.next = None;
            node.prev = self.tail;
            let node = NonNull::new(Box::into_raw(node)).expect("Got a null ptr");

            match self.tail {
                None => self.head = Some(node),
                // Not creating new mutable (unique!) references overlapping `element`.
                Some(tail) => (*tail.as_ptr()).next = Some(node),
            }

            self.tail = Some(node);
            self.len += 1;
            node
        }
    }

    /// Removes and returns the node at the back of the list.
    // pub(crate) fn pop_back(&mut self) -> Option<Box<DeqNode<T>>> {
    //     // This method takes care not to create mutable references to whole nodes,
    //     // to maintain validity of aliasing pointers into `element`.
    //     self.tail.map(|node| unsafe {
    //         let node = Box::from_raw(node.as_ptr());
    //         self.tail = node.prev;

    //         match self.tail {
    //             None => self.head = None,
    //             // Not creating new mutable (unique!) references overlapping `element`.
    //             Some(tail) => (*tail.as_ptr()).next = None,
    //         }

    //         self.len -= 1;
    //         node
    //     })
    // }

    /// Panics:
    pub(crate) unsafe fn move_to_back(&mut self, mut node: NonNull<DeqNode<T>>) {
        // noop when this node is the only one in this deque.
        if self.len() == 1 {
            return;
        }

        let node = node.as_mut(); // this one is ours now, we can create an &mut.

        // Not creating new mutable (unique!) references overlapping `element`.
        match node.prev {
            Some(prev) if node.next.is_some() => (*prev.as_ptr()).next = node.next,
            Some(..) => (),
            // this node is the head node
            None => self.head = node.next,
        };

        if let Some(next) = node.next {
            (*next.as_ptr()).prev = node.prev;
            node.next = None;
        }

        let mut node = NonNull::from(node);
        match self.tail {
            None => unreachable!(),
            // Not creating new mutable (unique!) references overlapping `element`.
            Some(tail) => {
                node.as_mut().prev = Some(tail);
                (*tail.as_ptr()).next = Some(node)
            }
        }
        self.tail = Some(node);
    }

    /// Unlinks the specified node from the current list.
    ///
    /// This method takes care not to create mutable references to `element`, to
    /// maintain validity of aliasing pointers.
    ///
    /// Panics:
    pub(crate) unsafe fn unlink(&mut self, mut node: NonNull<DeqNode<T>>) {
        assert_eq!(self.region, node.as_ref().region);

        let node = node.as_mut(); // this one is ours now, we can create an &mut.

        // Not creating new mutable (unique!) references overlapping `element`.
        match node.prev {
            Some(prev) => (*prev.as_ptr()).next = node.next,
            // this node is the head node
            None => self.head = node.next,
        };

        match node.next {
            Some(next) => (*next.as_ptr()).prev = node.prev,
            // this node is the tail node
            None => self.tail = node.prev,
        };

        node.prev = None;
        node.next = None;

        self.len -= 1;
    }
}

// Private function/methods
impl<T> Deque<T> {
    fn is_head(&self, node: &DeqNode<T>) -> bool {
        if let Some(head) = self.head {
            std::ptr::eq(unsafe { head.as_ref() }, node)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CacheRegion::MainProbation,
        DeqNode, Deque,
    };
    use std::sync::Arc;

    #[test]
    fn basics() {
        let mut deque: Deque<String> = Deque::new(MainProbation);
        assert_eq!(deque.len(), 0);
        assert_eq!(deque.peek_front(), None);

        // push_back(node1)
        let node1 = DeqNode::new(MainProbation, Arc::new("a".into()));
        assert!(!deque.is_member(&node1));
        let node1 = Box::new(node1);
        let node1_ptr = deque.push_back(node1);
        assert_eq!(deque.len(), 1);

        // peek_front() -> node1
        let head_a = deque.peek_front();
        assert!(head_a.is_some());
        let head_a = head_a.unwrap();
        assert!(deque.is_member(&head_a));
        assert_eq!(head_a.element, Arc::new("a".into()));

        // move_to_back(node1)
        unsafe { deque.move_to_back(node1_ptr.clone()) };
        assert_eq!(deque.len(), 1);

        // peek_front() -> node1
        let head_b = deque.peek_front();
        assert!(head_b.is_some());
        assert!(std::ptr::eq(head_b.unwrap(), unsafe { node1_ptr.as_ref() }));

        // push_back(node2)
        let node2 = DeqNode::new(MainProbation, Arc::new("b".into()));
        assert!(!deque.is_member(&node2));
        let node2_ptr = deque.push_back(Box::new(node2));
        assert_eq!(deque.len(), 2);

        // peek_front() -> node1
        let head_c = deque.peek_front();
        assert!(head_c.is_some());
        assert!(std::ptr::eq(head_c.unwrap(), unsafe { node1_ptr.as_ref() }));

        // move_to_back(node2)
        unsafe { deque.move_to_back(node2_ptr.clone()) };
        assert_eq!(deque.len(), 2);

        // peek_front() -> node1
        let head_d = deque.peek_front();
        assert!(head_d.is_some());
        assert!(std::ptr::eq(head_d.unwrap(), unsafe { node1_ptr.as_ref() }));

        // peek_back() -> node2
        let tail_a = deque.peek_back();
        assert!(tail_a.is_some());
        assert!(std::ptr::eq(tail_a.unwrap(), unsafe { node2_ptr.as_ref() }));
        assert_eq!(tail_a.unwrap().element, Arc::new("b".into()));

        // move_to_back(node1)
        unsafe { deque.move_to_back(node1_ptr.clone()) };
        assert_eq!(deque.len(), 2);

        // peek_front() -> node2
        let head_e = deque.peek_front();
        assert!(head_e.is_some());
        assert!(std::ptr::eq(head_e.unwrap(), unsafe { node2_ptr.as_ref() }));

        // peek_back() -> node1
        let tail_b = deque.peek_back();
        assert!(tail_b.is_some());
        assert!(std::ptr::eq(tail_b.unwrap(), unsafe { node1_ptr.as_ref() }));

        // push_back(node3)
        let node3 = DeqNode::new(MainProbation, Arc::new("c".into()));
        assert!(!deque.is_member(&node3));
        let node3_ptr = deque.push_back(Box::new(node3));
        assert_eq!(deque.len(), 3);

        // peek_front() -> node2
        let head_f = deque.peek_front();
        assert!(head_f.is_some());
        assert!(std::ptr::eq(head_f.unwrap(), unsafe { node2_ptr.as_ref() }));

        // peek_back() -> node3
        let tail_c = deque.peek_back();
        assert!(tail_c.is_some());
        assert!(std::ptr::eq(tail_c.unwrap(), unsafe { node3_ptr.as_ref() }));
        assert_eq!(tail_c.unwrap().element, Arc::new("c".into()));

        // move_to_back(node1)
        unsafe { deque.move_to_back(node1_ptr.clone()) };
        assert_eq!(deque.len(), 3);

        // peek_front() -> node2
        let head_g = deque.peek_front();
        assert!(head_g.is_some());
        assert!(std::ptr::eq(head_g.unwrap(), unsafe { node2_ptr.as_ref() }));

        // peek_back() -> node1
        let tail_d = deque.peek_back();
        assert!(tail_d.is_some());
        assert!(std::ptr::eq(tail_d.unwrap(), unsafe { node1_ptr.as_ref() }));

        // unlink(node3)
        unsafe { deque.unlink(node3_ptr) };
        assert_eq!(deque.len(), 2);
        assert!(!deque.is_member(unsafe { node3_ptr.as_ref() }));
        std::mem::drop(node3_ptr);

        // peek_front() -> node2
        let head_h = deque.peek_front();
        assert!(head_h.is_some());
        assert!(std::ptr::eq(head_h.unwrap(), unsafe { node2_ptr.as_ref() }));

        // peek_back() -> node1
        let tail_e = deque.peek_back();
        assert!(tail_e.is_some());
        assert!(std::ptr::eq(tail_e.unwrap(), unsafe { node1_ptr.as_ref() }));

        // unlink(node2)
        unsafe { deque.unlink(node2_ptr) };
        assert_eq!(deque.len(), 1);
        assert!(!deque.is_member(unsafe { node2_ptr.as_ref() }));
        std::mem::drop(node2_ptr);

        // peek_front() -> node1
        let head_h = deque.peek_front();
        assert!(head_h.is_some());
        assert!(std::ptr::eq(head_h.unwrap(), unsafe { node1_ptr.as_ref() }));

        // peek_back() -> node1
        let tail_e = deque.peek_back();
        assert!(tail_e.is_some());
        assert!(std::ptr::eq(tail_e.unwrap(), unsafe { node1_ptr.as_ref() }));

        // unlink(node1)
        unsafe { deque.unlink(node1_ptr) };
        assert_eq!(deque.len(), 0);
        assert!(!deque.is_member(unsafe { node1_ptr.as_ref() }));
        std::mem::drop(node1_ptr);

        // peek_front() -> node1
        let head_h = deque.peek_front();
        assert!(head_h.is_none());

        // peek_back() -> node1
        let tail_e = deque.peek_back();
        assert!(tail_e.is_none());
    }

    #[test]
    fn drop() {
        use std::{
            rc::Rc,
            sync::atomic::{AtomicU32, Ordering},
        };

        struct X(u32, Rc<AtomicU32>);

        impl Drop for X {
            fn drop(&mut self) {
                self.1.fetch_add(self.0, Ordering::Relaxed);
            }
        }

        let mut deque: Deque<X> = Deque::new(MainProbation);
        let dropped = Rc::new(AtomicU32::new(0));

        let node1 = DeqNode::new(MainProbation, Arc::new(X(1, Rc::clone(&dropped))));
        let node2 = DeqNode::new(MainProbation, Arc::new(X(2, Rc::clone(&dropped))));
        let node3 = DeqNode::new(MainProbation, Arc::new(X(3, Rc::clone(&dropped))));
        let node4 = DeqNode::new(MainProbation, Arc::new(X(4, Rc::clone(&dropped))));
        deque.push_back(Box::new(node1));
        deque.push_back(Box::new(node2));
        deque.push_back(Box::new(node3));
        deque.push_back(Box::new(node4));
        assert_eq!(deque.len(), 4);

        std::mem::drop(deque);

        assert_eq!(dropped.load(Ordering::Relaxed), (1..=4).sum());
    }
}
