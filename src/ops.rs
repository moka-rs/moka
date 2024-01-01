pub mod compute {

    /// Instructs the `and_compute` method how to modify the cache entry.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Op<V> {
        /// No-op. Do not modify the cached entry.
        Nop,
        /// Insert or replace the value of the cached entry.
        Put(V),
        /// Remove the cached entry.
        Remove,
    }

    /// Will be returned from `and_compute_with` and similar methods to indicate what
    /// kind of operation was performed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PerformedOp {
        /// The entry did not exist, or already existed but was not modified.
        Nop,
        /// The entry did not exist and was inserted.
        Inserted,
        /// The entry already existed and its value was updated.
        Updated,
        /// The entry existed and was removed.
        ///
        /// Note: If `and_compute_with` tried to remove a not-exiting entry, `Nop`
        /// will be returned.
        Removed,
    }
}
