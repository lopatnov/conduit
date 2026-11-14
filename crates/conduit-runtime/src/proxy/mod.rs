#[cfg(feature = "redis")]
pub mod cache_redis;
pub mod ctx;
mod dispatch;
mod logging_phase;
pub(crate) mod request;
mod response_phase;
pub mod router;
pub mod service;

// Paths the moved files use through `crate::proxy::…`; in the root crate these are one-line facades over the
// Layer-1 crates, here they are module re-exports of the same modules.
pub use conduit_cache::cache;
pub use conduit_cache::disk as cache_disk;
pub use conduit_proxy_http::routes;
pub use conduit_upstream::health;

pub mod upstream {
    pub use conduit_proxy_http::targets::{target_urls, target_urls_from_proxy, weighted_targets};
    pub use conduit_upstream::targets::*;
}
