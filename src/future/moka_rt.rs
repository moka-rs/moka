//
// actix
//
#[cfg(all(
    any(feature = "runtime-actix"),
    not(any(feature = "runtime-tokio", feature = "runtime-async-std")),
))]
pub use tokio::{task::spawn, time::sleep};

//
// async-std
//

#[cfg(all(
    feature = "runtime-async-std",
    not(any(feature = "runtime-actix", feature = "runtime-tokio")),
))]
pub use async_std::task::{sleep, spawn};

//
// tokio
//

#[cfg(all(
    any(feature = "runtime-tokio"),
    not(any(feature = "runtime-actix", feature = "runtime-async-std")),
))]
pub use tokio::{task::spawn, time::sleep};
