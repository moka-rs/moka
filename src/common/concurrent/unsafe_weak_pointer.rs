use std::sync::{Arc, Weak};

/// WARNING: Do not use this struct unless you are absolutely sure what you are
/// doing. Using this struct is unsafe and may cause memory related crashes and/or
/// security vulnerabilities.
pub(crate) struct UnsafeWeakPointer<T> {
    // This is a std::sync::Weak pointer to Inner<K, V, S>.
    raw_ptr: *mut T,
}

unsafe impl<T> Send for UnsafeWeakPointer<T> {}

impl<T> UnsafeWeakPointer<T> {
    pub(crate) fn from_weak_arc(p: Weak<T>) -> Self {
        Self {
            raw_ptr: p.into_raw() as *mut T,
        }
    }

    pub(crate) unsafe fn as_weak_arc(&self) -> Weak<T> {
        Weak::from_raw(self.raw_ptr.cast())
    }

    pub(crate) fn forget_arc(p: Arc<T>) {
        // Downgrade the Arc to Weak, then forget.
        let weak = Arc::downgrade(&p);
        std::mem::forget(weak);
    }

    pub(crate) fn forget_weak_arc(p: Weak<T>) {
        std::mem::forget(p);
    }
}

/// `clone()` simply creates a copy of the `raw_ptr`, effectively creating many
/// copies of the same `Weak` pointer. We are doing this for a good reason for our
/// use case.
///
/// When you want to drop the Weak pointer, ensure that you drop it only once for the
/// same `raw_ptr` across clones.
impl<T> Clone for UnsafeWeakPointer<T> {
    fn clone(&self) -> Self {
        Self {
            raw_ptr: self.raw_ptr,
        }
    }
}
