// License and Copyright Notice:
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

use std::{marker::PhantomData, ptr::NonNull};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CacheRegion {
    Window,
    MainProbation,
    MainProtected,
}

#[derive(PartialEq, Eq)]
pub(crate) struct DeqNode<T> {
    pub(crate) region: CacheRegion,
    next: Option<NonNull<DeqNode<T>>>,
    prev: Option<NonNull<DeqNode<T>>>,
    pub(crate) element: T,
}

impl<T> std::fmt::Debug for DeqNode<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeqNode")
            .field("region", &self.region)
            .field("next", &self.next)
            .field("prev", &self.prev)
            .finish()
    }
}

impl<T> DeqNode<T> {
    pub(crate) fn new(region: CacheRegion, element: T) -> Self {
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

    pub(crate) fn contains(&self, node: &DeqNode<T>) -> bool {
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
    fn peek_back(&self) -> Option<&DeqNode<T>> {
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
        if self.is_tail(node.as_ref()) {
            return;
        }

        let node = node.as_mut(); // this one is ours now, we can create an &mut.

        // Not creating new mutable (unique!) references overlapping `element`.
        match node.prev {
            Some(prev) if node.next.is_some() => (*prev.as_ptr()).next = node.next,
            Some(..) => (),
            // This node is the head node.
            None => self.head = node.next,
        };

        // This node is not the tail node.
        if let Some(next) = node.next.take() {
            (*next.as_ptr()).prev = node.prev;

            let mut node = NonNull::from(node);
            match self.tail {
                // Not creating new mutable (unique!) references overlapping `element`.
                Some(tail) => {
                    node.as_mut().prev = Some(tail);
                    (*tail.as_ptr()).next = Some(node)
                }
                None => unreachable!(),
            }
            self.tail = Some(node);
        }
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

    fn is_tail(&self, node: &DeqNode<T>) -> bool {
        if let Some(tail) = self.tail {
            std::ptr::eq(unsafe { tail.as_ref() }, node)
        } else {
            false
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheRegion::MainProbation, DeqNode, Deque};

    #[test]
    #[allow(clippy::cognitive_complexity)]
    fn basics() {
        let mut deque: Deque<String> = Deque::new(MainProbation);
        assert_eq!(deque.len(), 0);
        assert!(deque.peek_front().is_none());
        assert!(deque.peek_back().is_none());

        // push_back(node1)
        let node1 = DeqNode::new(MainProbation, "a".to_string());
        assert!(!deque.contains(&node1));
        let node1 = Box::new(node1);
        let node1_ptr = deque.push_back(node1);
        assert_eq!(deque.len(), 1);

        // peek_front() -> node1
        let head_a = deque.peek_front().unwrap();
        assert!(deque.contains(&head_a));
        assert!(deque.is_head(&head_a));
        assert!(deque.is_tail(&head_a));
        assert_eq!(head_a.element, "a".to_string());

        // move_to_back(node1)
        unsafe { deque.move_to_back(node1_ptr.clone()) };
        assert_eq!(deque.len(), 1);

        // peek_front() -> node1
        let head_b = deque.peek_front().unwrap();
        assert!(deque.contains(&head_b));
        assert!(deque.is_head(&head_b));
        assert!(deque.is_tail(&head_b));
        assert!(std::ptr::eq(head_b, unsafe { node1_ptr.as_ref() }));
        assert!(head_b.prev.is_none());
        assert!(head_b.next.is_none());

        // peek_back() -> node1
        let tail_a = deque.peek_back().unwrap();
        assert!(deque.contains(&tail_a));
        assert!(deque.is_head(&tail_a));
        assert!(deque.is_tail(&tail_a));
        assert!(std::ptr::eq(tail_a, unsafe { node1_ptr.as_ref() }));
        assert!(tail_a.prev.is_none());
        assert!(tail_a.next.is_none());

        // push_back(node2)
        let node2 = DeqNode::new(MainProbation, "b".to_string());
        assert!(!deque.contains(&node2));
        let node2_ptr = deque.push_back(Box::new(node2));
        assert_eq!(deque.len(), 2);

        // peek_front() -> node1
        let head_c = deque.peek_front().unwrap();
        assert!(deque.contains(&head_c));
        assert!(deque.is_head(&head_c));
        assert!(!deque.is_tail(&head_c));
        assert!(std::ptr::eq(head_c, unsafe { node1_ptr.as_ref() }));
        assert!(head_c.prev.is_none());
        assert!(std::ptr::eq(
            unsafe { head_c.next.unwrap().as_ref() },
            unsafe { node2_ptr.as_ref() }
        ));

        // move_to_back(node2)
        unsafe { deque.move_to_back(node2_ptr.clone()) };
        assert_eq!(deque.len(), 2);

        // peek_front() -> node1
        let head_d = deque.peek_front().unwrap();
        assert!(deque.contains(&head_d));
        assert!(deque.is_head(&head_d));
        assert!(!deque.is_tail(&head_d));
        assert!(std::ptr::eq(head_d, unsafe { node1_ptr.as_ref() }));
        assert!(head_d.prev.is_none());
        assert!(std::ptr::eq(
            unsafe { head_d.next.unwrap().as_ref() },
            unsafe { node2_ptr.as_ref() }
        ));

        // peek_back() -> node2
        let tail_b = deque.peek_back().unwrap();
        assert!(deque.contains(&tail_b));
        assert!(!deque.is_head(&tail_b));
        assert!(deque.is_tail(&tail_b));
        assert!(std::ptr::eq(tail_b, unsafe { node2_ptr.as_ref() }));
        assert!(std::ptr::eq(
            unsafe { tail_b.prev.unwrap().as_ref() },
            unsafe { node1_ptr.as_ref() }
        ));
        assert_eq!(tail_b.element, "b".to_string());
        assert!(tail_b.next.is_none());

        // move_to_back(node1)
        unsafe { deque.move_to_back(node1_ptr.clone()) };
        assert_eq!(deque.len(), 2);

        // peek_front() -> node2
        let head_e = deque.peek_front().unwrap();
        assert!(deque.contains(&head_e));
        assert!(deque.is_head(&head_e));
        assert!(!deque.is_tail(&head_e));
        assert!(std::ptr::eq(head_e, unsafe { node2_ptr.as_ref() }));
        assert!(head_e.prev.is_none());
        assert!(std::ptr::eq(
            unsafe { head_e.next.unwrap().as_ref() },
            unsafe { node1_ptr.as_ref() }
        ));

        // peek_back() -> node1
        let tail_c = deque.peek_back().unwrap();
        assert!(deque.contains(&tail_c));
        assert!(!deque.is_head(&tail_c));
        assert!(deque.is_tail(&tail_c));
        assert!(std::ptr::eq(tail_c, unsafe { node1_ptr.as_ref() }));
        assert!(std::ptr::eq(
            unsafe { tail_c.prev.unwrap().as_ref() },
            unsafe { node2_ptr.as_ref() }
        ));
        assert!(tail_c.next.is_none());

        // push_back(node3)
        let node3 = DeqNode::new(MainProbation, "c".to_string());
        assert!(!deque.contains(&node3));
        let node3_ptr = deque.push_back(Box::new(node3));
        assert_eq!(deque.len(), 3);

        // peek_front() -> node2
        let head_f = deque.peek_front().unwrap();
        assert!(deque.contains(&head_f));
        assert!(deque.is_head(&head_f));
        assert!(!deque.is_tail(&head_f));
        assert!(std::ptr::eq(head_f, unsafe { node2_ptr.as_ref() }));
        assert!(head_f.prev.is_none());
        assert!(std::ptr::eq(
            unsafe { head_f.next.unwrap().as_ref() },
            unsafe { node1_ptr.as_ref() }
        ));

        // peek_back() -> node3
        let tail_d = deque.peek_back().unwrap();
        assert!(std::ptr::eq(tail_d, unsafe { node3_ptr.as_ref() }));
        assert_eq!(tail_d.element, "c".to_string());
        assert!(deque.contains(&tail_d));
        assert!(!deque.is_head(&tail_d));
        assert!(deque.is_tail(&tail_d));
        assert!(std::ptr::eq(tail_d, unsafe { node3_ptr.as_ref() }));
        assert!(std::ptr::eq(
            unsafe { tail_d.prev.unwrap().as_ref() },
            unsafe { node1_ptr.as_ref() }
        ));
        assert!(tail_d.next.is_none());

        // move_to_back(node1)
        unsafe { deque.move_to_back(node1_ptr.clone()) };
        assert_eq!(deque.len(), 3);

        // peek_front() -> node2
        let head_g = deque.peek_front().unwrap();
        assert!(deque.contains(&head_g));
        assert!(deque.is_head(&head_g));
        assert!(!deque.is_tail(&head_g));
        assert!(std::ptr::eq(head_g, unsafe { node2_ptr.as_ref() }));
        assert!(head_g.prev.is_none());
        assert!(std::ptr::eq(
            unsafe { head_g.next.unwrap().as_ref() },
            unsafe { node3_ptr.as_ref() }
        ));

        // peek_back() -> node1
        let tail_e = deque.peek_back().unwrap();
        assert!(deque.contains(&tail_e));
        assert!(!deque.is_head(&tail_e));
        assert!(deque.is_tail(&tail_e));
        assert!(std::ptr::eq(tail_e, unsafe { node1_ptr.as_ref() }));
        assert!(std::ptr::eq(
            unsafe { tail_e.prev.unwrap().as_ref() },
            unsafe { node3_ptr.as_ref() }
        ));
        assert!(tail_e.next.is_none());

        // unlink(node3)
        unsafe { deque.unlink(node3_ptr) };
        assert_eq!(deque.len(), 2);
        let node3_ref = unsafe { node3_ptr.as_ref() };
        assert!(!deque.contains(node3_ref));
        assert!(node3_ref.next.is_none());
        assert!(node3_ref.next.is_none());

        // This does not work as expected because NonNull implements Copy trait.
        // See clippy(clippy::drop_copy) at
        // https://rust-lang.github.io/rust-clippy/master/index.html#drop_copy
        // std::mem::drop(node3_ptr);

        // peek_front() -> node2
        let head_h = deque.peek_front().unwrap();
        assert!(deque.contains(&head_h));
        assert!(deque.is_head(&head_h));
        assert!(!deque.is_tail(&head_h));
        assert!(std::ptr::eq(head_h, unsafe { node2_ptr.as_ref() }));
        assert!(head_h.prev.is_none());
        assert!(std::ptr::eq(
            unsafe { head_h.next.unwrap().as_ref() },
            unsafe { node1_ptr.as_ref() }
        ));

        // peek_back() -> node1
        let tail_f = deque.peek_back().unwrap();
        assert!(deque.contains(&tail_f));
        assert!(!deque.is_head(&tail_f));
        assert!(deque.is_tail(&tail_f));
        assert!(std::ptr::eq(tail_f, unsafe { node1_ptr.as_ref() }));
        assert!(std::ptr::eq(
            unsafe { tail_f.prev.unwrap().as_ref() },
            unsafe { node2_ptr.as_ref() }
        ));
        assert!(tail_f.next.is_none());

        // unlink(node2)
        unsafe { deque.unlink(node2_ptr) };
        assert_eq!(deque.len(), 1);
        let node2_ref = unsafe { node2_ptr.as_ref() };
        assert!(!deque.contains(node2_ref));
        assert!(node2_ref.next.is_none());
        assert!(node2_ref.next.is_none());
        // std::mem::drop(node2_ptr);

        // peek_front() -> node1
        let head_g = deque.peek_front().unwrap();
        assert!(deque.contains(&head_g));
        assert!(deque.is_head(&head_g));
        assert!(deque.is_tail(&head_g));
        assert!(std::ptr::eq(head_g, unsafe { node1_ptr.as_ref() }));
        assert!(head_g.prev.is_none());
        assert!(head_g.next.is_none());

        // peek_back() -> node1
        let tail_g = deque.peek_back().unwrap();
        assert!(deque.contains(&tail_g));
        assert!(deque.is_head(&tail_g));
        assert!(deque.is_tail(&tail_g));
        assert!(std::ptr::eq(tail_g, unsafe { node1_ptr.as_ref() }));
        assert!(tail_g.next.is_none());
        assert!(tail_g.next.is_none());

        // unlink(node1)
        unsafe { deque.unlink(node1_ptr) };
        assert_eq!(deque.len(), 0);
        let node1_ref = unsafe { node1_ptr.as_ref() };
        assert!(!deque.contains(node1_ref));
        assert!(node1_ref.next.is_none());
        assert!(node1_ref.next.is_none());
        // std::mem::drop(node1_ptr);

        // peek_front() -> node1
        let head_h = deque.peek_front();
        assert!(head_h.is_none());

        // peek_back() -> node1
        let tail_e = deque.peek_back();
        assert!(tail_e.is_none());
    }

    #[test]
    fn drop() {
        use std::{cell::RefCell, rc::Rc};

        struct X(u32, Rc<RefCell<Vec<u32>>>);

        impl Drop for X {
            fn drop(&mut self) {
                self.1.borrow_mut().push(self.0)
            }
        }

        let mut deque: Deque<X> = Deque::new(MainProbation);
        let dropped = Rc::new(RefCell::new(Vec::default()));

        let node1 = DeqNode::new(MainProbation, X(1, Rc::clone(&dropped)));
        let node2 = DeqNode::new(MainProbation, X(2, Rc::clone(&dropped)));
        let node3 = DeqNode::new(MainProbation, X(3, Rc::clone(&dropped)));
        let node4 = DeqNode::new(MainProbation, X(4, Rc::clone(&dropped)));
        deque.push_back(Box::new(node1));
        deque.push_back(Box::new(node2));
        deque.push_back(Box::new(node3));
        deque.push_back(Box::new(node4));
        assert_eq!(deque.len(), 4);

        std::mem::drop(deque);

        assert_eq!(*dropped.borrow(), &[1, 2, 3, 4]);
    }
}
