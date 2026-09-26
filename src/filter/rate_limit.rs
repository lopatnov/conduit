//! Facade over `conduit_runtime` (issue #145): rate-limit key building and the check wrappers moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::filter::rate_limit::{
    check, cleanup, consumer_key, extract_client_key, redis_route_scope, route_key, site_key,
    RateLimiter, TokenBucket, MAX_BUCKETS,
};
