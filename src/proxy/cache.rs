//! Extracted into `crates/conduit-cache` (issue #114/#135) — this is a
//! facade re-export so `crate::proxy::cache::{build_cache_key, cache_storage,
//! cache_lock, should_cache_request, response_cacheable, should_early_refresh}`
//! keep resolving to the same items at the same location for backward
//! compatibility (`src/admin/api.rs`'s cache-purge handler,
//! `src/proxy/request_phase.rs`, `src/proxy/response_phase.rs`). See
//! `conduit_cache::cache` for the implementation and its own doc comment for
//! why almost all of this is compiled regardless of `--features cache`.

pub use conduit_cache::cache::should_cache_request;
#[cfg(feature = "cache")]
pub use conduit_cache::cache::should_early_refresh;
pub use conduit_cache::cache::{build_cache_key, cache_lock, cache_storage, response_cacheable};
