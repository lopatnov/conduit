//! Facade over `conduit_runtime` (issue #145): the Redis proxy-cache registry glue moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

#[cfg(feature = "cache")]
pub(crate) use conduit_runtime::proxy::cache_redis::connect_all;
pub use conduit_runtime::proxy::cache_redis::get;
