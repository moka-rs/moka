#[derive(thiserror::Error, Debug)]
pub enum PredicateRegistrationError {
    #[error(
        "Support for invalidation closures is disabled. \
    Please enable it by calling the enable_invalidation_with_closures method \
    of the builder at the cache creation time"
    )]
    InvalidationClosuresDisabled,
    #[error("No space left in the predicate registry")]
    NoSpaceLeft,
}
