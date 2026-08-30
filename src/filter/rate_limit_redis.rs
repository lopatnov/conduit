#![cfg(feature = "redis")]
//! Extracted into `crates/conduit-ratelimit` (issue #114/#137, slice 2) —
//! this is a facade re-export so `crate::filter::rate_limit_redis::
//! RedisRateLimiter` keeps resolving to the same type at the same location
//! for every existing call site/test. See that crate's `src/redis.rs` for
//! the implementation.

pub use conduit_ratelimit::redis::RedisRateLimiter;
