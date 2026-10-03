//! Facade over `conduit_runtime` (issue #145): the Redis proxy-cache registry glue moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.
//!
//! `connect_all` is no longer re-exported here (issue #147): its three call sites (the Admin API supervisor's
//! `start()`/`/reload`, and `server/builder.rs`'s config-update watcher) all moved into `crates/conduit-server`,
//! which reaches `conduit_runtime::proxy::cache_redis::connect_all` through its own alias module instead.

pub use conduit_runtime::proxy::cache_redis::get;
