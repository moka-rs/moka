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
                while let Some(_) = self.0.pop_front() {}
            }
        }

        while let Some(node) = self.pop_front() {
            let guard = DropGuard(self);
            drop(node);
            std::mem::forget(guard);
        }
    }
}

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

    // pub(crate) fn len(&self) -> usize {
    //     self.len
    // }

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

    pub(crate) fn peek_front(&mut self) -> Option<&DeqNode<T>> {
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

    // pub(crate) fn peek_back(&mut self) -> Option<&DeqNode<T>> {
    //     todo!()
    // }

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

        let node = NonNull::from(node);
        match self.tail {
            None => unreachable!(),
            // Not creating new mutable (unique!) references overlapping `element`.
            Some(tail) => (*tail.as_ptr()).next = Some(node),
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

        self.len -= 1;
    }
}

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
