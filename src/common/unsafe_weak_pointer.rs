use std::sync::{Arc, Weak};

/// WARNING: Do not use this struct unless you are absolutely sure what you are
/// doing. Using this struct is unsafe and may cause memory related crashes and/or
/// security vulnerabilities.
///
/// This struct exists with the sole purpose of avoiding compile errors relevant to
/// the thread pool usages. The thread pool requires that the generic parameters on
/// the `Cache` and `Inner` structs to have trait bounds `Send`, `Sync` and
/// `'static`. This will be unacceptable for many cache usages.
///
/// This struct avoids the trait bounds by transmuting a pointer between
/// `std::sync::Weak<Inner<K, V, S>>` and `usize`.
///
/// If you know a better solution than this, we would love te hear it.
pub(crate) struct UnsafeWeakPointer {
    // This is a std::sync::Weak pointer to Inner<K, V, S>.
    raw_ptr: usize,
}

impl UnsafeWeakPointer {
    pub(crate) fn from_weak_arc<T>(p: Weak<T>) -> Self {
        Self {
            raw_ptr: unsafe { std::mem::transmute(p) },
        }
    }

    pub(crate) unsafe fn as_weak_arc<T>(&self) -> Weak<T> {
        std::mem::transmute(self.raw_ptr)
    }

    pub(crate) fn forget_arc<T>(p: Arc<T>) {
        // Downgrade the Arc to Weak, then forget.
        let weak = Arc::downgrade(&p);
        std::mem::forget(weak);
    }

    pub(crate) fn forget_weak_arc<T>(p: Weak<T>) {
        std::mem::forget(p);
    }
}

/// `clone()` simply creates a copy of the `raw_ptr`, effectively creating many
/// copies of the same `Weak` pointer. We are doing this for a good reason for our
/// use case.
///
/// When you want to drop the Weak pointer, ensure that you drop it only once for the
/// same `raw_ptr` across clones.
impl Clone for UnsafeWeakPointer {
    fn clone(&self) -> Self {
        Self {
            raw_ptr: self.raw_ptr,
        }
    }
}
