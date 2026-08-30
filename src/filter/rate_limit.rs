//! `Session`-aware rate-limit key extraction and admission check.
//!
//! The pure, `Session`-independent parts — [`RateLimitConfig`](crate::config::
//! schema::RateLimitConfig), `TokenBucket`, `RateLimiter`, `MAX_BUCKETS`,
//! `cleanup` — moved to `crates/conduit-ratelimit` (issue #114/#137, slice 1)
//! and are re-exported below so every existing `crate::filter::rate_limit::*`
//! path keeps resolving. This file keeps only the code that needs
//! `pingora_proxy::Session`.

use pingora_proxy::Session;

use crate::config::schema::RateLimitConfig;
use crate::filter::auth::is_path_skipped;

pub use conduit_ratelimit::{cleanup, RateLimiter, TokenBucket, MAX_BUCKETS};

/// Extract the rate-limit key from the request.
///
/// `keyBy`:
/// - `"ip"` (default) — client IP address
/// - `"header:X-Foo"` — value of the named request header
///
/// Public so the Redis rate limiter can reuse the same key derivation logic.
pub fn extract_client_key(cfg: &RateLimitConfig, session: &Session) -> String {
    extract_key(cfg, session)
}

fn extract_key(cfg: &RateLimitConfig, session: &Session) -> String {
    let key_by = cfg.key_by.as_deref().unwrap_or("ip");

    if let Some(header_name) = key_by.strip_prefix("header:") {
        return session
            .req_header()
            .headers
            .get(header_name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_owned();
    }

    // Default: key by client IP address.
    session
        .client_addr()
        .and_then(|a| a.as_inet())
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Check the token-bucket rate limit for this request.
///
/// Returns `true` if the request is within the limit (allowed to proceed).
/// Admission (including the `MAX_BUCKETS` cap) is delegated to
/// `conduit_ratelimit::check_key_for` — the single admission point shared by
/// every rate-limit layer (site, route, consumer, Redis fallback; see issue
/// #305).
pub fn check(cfg: &RateLimitConfig, session: &Session, limiter: &RateLimiter) -> bool {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return true;
    }

    let key = extract_key(cfg, session);
    conduit_ratelimit::check_key_for(limiter, &key, cfg)
}

// extract_key/check need a real Session, exercised end-to-end via
// tests/security.rs (rate_limit_* integration tests). The pure
// TokenBucket/RateLimiter/check_key logic is unit-tested in
// crates/conduit-ratelimit/src/bucket.rs now.
