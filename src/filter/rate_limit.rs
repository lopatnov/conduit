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

// Canonical, `\0`-separated bucket-key namespaces shared by every rate-limit
// layer (site, route, consumer) and by `GET /rate-limits` (`admin/api.rs`),
// which parses these same three shapes back apart. Unifies what was
// previously three divergent, mutually-unparseable formats (bare client key
// for site-level, `"route:{key}:{ip}"`, `"consumer:{username}"`) — see
// issues #303/#304.
//
// Site- and route-level keys are scoped by `site_label` (the same
// `"{host}:{port}"`/`"*"` value already used for
// `conduit_rate_limit_rejected_total{site=…}`) so two sites sharing a
// client key no longer collide into one shared bucket (#304). Consumer-level
// keys are deliberately left unscoped — a consumer's quota is global across
// every site it's allowed to call, by design (see `CLAUDE.md` decision #14).

/// Build the site-level bucket key: `site\0{site_label}\0{client_key}`.
pub fn site_key(site_label: &str, client_key: &str) -> String {
    format!("site\0{site_label}\0{client_key}")
}

/// Build the route-level bucket key:
/// `route\0{site_label}\0{route_key}\0{client_key}`.
pub fn route_key(site_label: &str, route_key: &str, client_key: &str) -> String {
    format!("route\0{site_label}\0{route_key}\0{client_key}")
}

/// Build the consumer-level bucket key: `consumer\0{username}` — global
/// per consumer, not site-scoped (see the module-level doc above).
pub fn consumer_key(username: &str) -> String {
    format!("consumer\0{username}")
}

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
/// `site_label` scopes the bucket key so two sites sharing a client key
/// don't collide into one shared bucket (#304) — see [`site_key`].
///
/// Returns `true` if the request is within the limit (allowed to proceed).
/// Admission (including the `MAX_BUCKETS` cap) is delegated to
/// `conduit_ratelimit::check_key_for` — the single admission point shared by
/// every rate-limit layer (site, route, consumer, Redis fallback; see issue
/// #305).
pub fn check(
    cfg: &RateLimitConfig,
    session: &Session,
    limiter: &RateLimiter,
    site_label: &str,
) -> bool {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return true;
    }

    let client_key = extract_key(cfg, session);
    let key = site_key(site_label, &client_key);
    conduit_ratelimit::check_key_for(limiter, &key, cfg)
}

// extract_key/check need a real Session, exercised end-to-end via
// tests/security.rs (rate_limit_* integration tests). The pure
// TokenBucket/RateLimiter/check_key logic is unit-tested in
// crates/conduit-ratelimit/src/bucket.rs now.

#[cfg(test)]
mod tests {
    use super::*;

    // ── site_key / route_key / consumer_key ─────────────────────────────

    #[test]
    fn site_key_scopes_by_site_label() {
        let a = site_key("a.example.com:80", "1.2.3.4");
        let b = site_key("b.example.com:80", "1.2.3.4");
        assert_ne!(
            a, b,
            "two sites sharing a client key must not produce the same bucket key (#304)"
        );
        assert_eq!(a, "site\0a.example.com:80\x001.2.3.4");
    }

    #[test]
    fn route_key_scopes_by_site_label_and_route() {
        let a = route_key("a.example.com:80", "/api", "1.2.3.4");
        let b = route_key("b.example.com:80", "/api", "1.2.3.4");
        assert_ne!(
            a, b,
            "two sites with the same route key and client key must not collide"
        );
        assert_eq!(a, "route\0a.example.com:80\0/api\x001.2.3.4");
    }

    #[test]
    fn consumer_key_is_not_site_scoped() {
        // Deliberately global — a consumer's quota follows them across every
        // site they're allowed to call, not scoped per site (CLAUDE.md #14).
        assert_eq!(consumer_key("alice"), "consumer\0alice");
    }

    #[test]
    fn the_three_namespaces_never_collide_with_each_other() {
        let s = site_key("x", "y");
        let r = route_key("x", "y", "z");
        let c = consumer_key("x");
        assert_ne!(s, r);
        assert_ne!(s, c);
        assert_ne!(r, c);
    }
}
