//! Cache operations.

/// Operations used by the `and_compute_with` and similar methods.
pub mod compute {

    /// Instructs the `and_compute_with` and similar methods how to modify the cached
    /// entry.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Op<V> {
        /// No-operation. Do not modify the cached entry.
        Nop,
        /// Insert or update the value of the cached entry.
        Put(V),
        /// Remove the cached entry.
        Remove,
    }

    /// Will be returned by the `and_compute_with` and similar methods to indicate
    /// what kind of operation was performed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PerformedOp {
        /// The entry did not exist, or already existed but was not modified.
        Nop,
        /// The entry did not exist and was inserted.
        Inserted,
        /// The entry already existed and its value may have been updated.
        ///
        /// Note: `Updated` is returned if `Op::Put` was requested and the entry
        /// already existed. It is _not_ related to whether the value was actually
        /// updated or not.
        Updated,
        /// The entry already existed and was removed.
        ///
        /// Note: `Nop` is returned instead of `Removed` if `Op::Remove` was
        /// requested but the entry did not exist.
        Removed,
    }
}
