#[derive(thiserror::Error, Debug)]
pub enum PredicateRegistrationError {
    #[error(
        "Support for invalidation closures is disabled. \
    Please enable it by calling the support_invalidation_closures method \
    of the builder at the cache creation time"
    )]
    InvalidationClosuresDisabled,
    #[error("No space left in the predicate registry")]
    NoSpaceLeft,
}
