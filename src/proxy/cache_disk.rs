//! Extracted into `crates/conduit-cache` (issue #114/#135) — this is a
//! facade re-export so `crate::proxy::cache_disk::get_or_create` keeps
//! resolving to the same item at the same location for backward
//! compatibility (`src/proxy/request_phase.rs`). See `conduit_cache::disk`
//! for the implementation.

pub use conduit_cache::disk::get_or_create;
