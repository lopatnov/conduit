//! Proxy-cache helpers for Phase 2.6.
//!
//! Provides:
//! - A shared `'static` in-memory storage singleton via [`cache_storage`].
//! - A deterministic [`build_cache_key`] function (host + scheme + path + query).
//! - [`should_cache_request`] — request-side policy (method, cookies, skip-paths).
//! - [`response_cacheable`] — response-side policy (status, TTL).

use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use pingora_cache::{CacheKey, CacheMeta, MemCache, NoCacheReason, RespCacheable};
use pingora_http::ResponseHeader;

use crate::config::schema::CacheConfig;

// ── Storage singleton ─────────────────────────────────────────────────────────

/// Global in-memory cache storage (initialised once per process).
///
/// `MemCache` is marked "for testing only" by Pingora but is the correct
/// backend for `store: "memory"` until a production-grade store is added in
/// Phase 3.8 (Redis / disk).
static MEM_CACHE: OnceLock<MemCache> = OnceLock::new();

/// Return a `'static` reference to the shared in-memory storage backend.
pub fn cache_storage() -> &'static MemCache {
    MEM_CACHE.get_or_init(MemCache::new)
}

// ── Cache key ─────────────────────────────────────────────────────────────────

/// Build a deterministic [`CacheKey`] from the request coordinates.
///
/// The `namespace` is the `host` value so that different virtual-hosts with
/// the same path are stored independently.  The `primary` key is
/// `scheme:path` (with `?query` appended when present).  The `user_tag` is
/// left empty — Conduit does not need per-user cache quotas.
pub fn build_cache_key(host: &str, scheme: &str, path: &str, query: Option<&str>) -> CacheKey {
    let primary = match query {
        Some(q) if !q.is_empty() => format!("{scheme}:{path}?{q}"),
        _ => format!("{scheme}:{path}"),
    };
    CacheKey::new(host, primary, "")
}

// ── Request-side policy ───────────────────────────────────────────────────────

/// Decide whether this request should be served from / admitted to the cache.
///
/// Returns `true` when caching should proceed, `false` when it should bypass:
/// - HTTP method not in the configured allow-list (default: GET + HEAD only).
/// - Request carries a `Cookie` header and `skipIfCookie` is `true`.
/// - The request path matches one of the `skipPaths` patterns.
pub fn should_cache_request(cfg: &CacheConfig, method: &str, has_cookie: bool, path: &str) -> bool {
    // Method filter (default: GET and HEAD only).
    let default_methods = ["GET", "HEAD"];
    let allowed: &[String] = cfg.methods.as_deref().unwrap_or(&[]);
    let method_ok = if allowed.is_empty() {
        default_methods.iter().any(|m| m.eq_ignore_ascii_case(method))
    } else {
        allowed.iter().any(|m| m.eq_ignore_ascii_case(method))
    };
    if !method_ok {
        return false;
    }

    // Skip personalised responses when the request sends cookies.
    if cfg.skip_if_cookie.unwrap_or(false) && has_cookie {
        return false;
    }

    // Path exclusion list — exact prefix or `/**` glob.
    if let Some(ref patterns) = cfg.skip_paths {
        if patterns.iter().any(|p| path_matches(p, path)) {
            return false;
        }
    }

    true
}

// ── Response-side policy ──────────────────────────────────────────────────────

/// Decide whether an upstream response should be cached.
///
/// Only `200 OK` responses with a non-zero `ttl_secs` are admitted.
/// Everything else returns [`RespCacheable::Uncacheable`].
pub fn response_cacheable(cfg: &CacheConfig, resp: &ResponseHeader) -> RespCacheable {
    // Only cache successful responses.
    if resp.status.as_u16() != 200 {
        return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache);
    }

    let ttl_secs = cfg.ttl_secs.unwrap_or(0);
    if ttl_secs == 0 {
        return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache);
    }

    let now = SystemTime::now();
    let fresh_until = now + Duration::from_secs(ttl_secs);
    let meta = CacheMeta::new(fresh_until, now, 0, 0, resp.clone());
    RespCacheable::Cacheable(meta)
}

// ── Path matching ─────────────────────────────────────────────────────────────

/// Check whether `path` matches a skip-path `pattern`.
///
/// Patterns ending with `/**` match the prefix itself and all sub-paths.
/// Everything else is an exact prefix match (path == pattern or
/// path starts with `pattern/`).
fn path_matches(pattern: &str, path: &str) -> bool {
    let prefix = pattern.strip_suffix("/**").unwrap_or(pattern);
    path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(ttl_secs: u64) -> CacheConfig {
        CacheConfig {
            store: "memory".into(),
            max_size_mb: None,
            ttl_secs: Some(ttl_secs),
            vary_headers: None,
            skip_paths: None,
            skip_if_cookie: None,
            methods: None,
        }
    }

    // ── build_cache_key ───────────────────────────────────────────────────────

    #[test]
    fn cache_key_without_query() {
        // Should not panic and should produce a key.
        let _ = build_cache_key("example.com", "https", "/api/data", None);
    }

    #[test]
    fn cache_key_with_empty_query() {
        let _ = build_cache_key("example.com", "https", "/api/data", Some(""));
    }

    #[test]
    fn cache_key_with_query() {
        let _ = build_cache_key("example.com", "https", "/search", Some("q=hello"));
    }

    // ── should_cache_request ──────────────────────────────────────────────────

    #[test]
    fn get_is_cached_by_default() {
        assert!(should_cache_request(&cfg(60), "GET", false, "/api"));
    }

    #[test]
    fn head_is_cached_by_default() {
        assert!(should_cache_request(&cfg(60), "HEAD", false, "/api"));
    }

    #[test]
    fn post_is_not_cached_by_default() {
        assert!(!should_cache_request(&cfg(60), "POST", false, "/api"));
    }

    #[test]
    fn method_check_is_case_insensitive() {
        assert!(should_cache_request(&cfg(60), "get", false, "/"));
        assert!(should_cache_request(&cfg(60), "Get", false, "/"));
    }

    #[test]
    fn skip_if_cookie_blocks_when_cookie_present() {
        let mut c = cfg(60);
        c.skip_if_cookie = Some(true);
        assert!(!should_cache_request(&c, "GET", true, "/api"));
    }

    #[test]
    fn skip_if_cookie_allows_when_no_cookie() {
        let mut c = cfg(60);
        c.skip_if_cookie = Some(true);
        assert!(should_cache_request(&c, "GET", false, "/api"));
    }

    #[test]
    fn skip_if_cookie_false_ignores_cookie() {
        let mut c = cfg(60);
        c.skip_if_cookie = Some(false);
        assert!(should_cache_request(&c, "GET", true, "/api"));
    }

    #[test]
    fn skip_paths_exact_prefix_blocks() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth".into()]);
        assert!(!should_cache_request(&c, "GET", false, "/api/auth"));
        assert!(!should_cache_request(&c, "GET", false, "/api/auth/login"));
    }

    #[test]
    fn skip_paths_exact_prefix_does_not_block_other_paths() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth".into()]);
        assert!(should_cache_request(&c, "GET", false, "/api/data"));
        assert!(should_cache_request(&c, "GET", false, "/api"));
    }

    #[test]
    fn skip_paths_glob_blocks_prefix_and_subpaths() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth/**".into()]);
        assert!(!should_cache_request(&c, "GET", false, "/api/auth"));
        assert!(!should_cache_request(&c, "GET", false, "/api/auth/"));
        assert!(!should_cache_request(
            &c,
            "GET",
            false,
            "/api/auth/anything"
        ));
    }

    #[test]
    fn skip_paths_glob_does_not_block_different_prefix() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth/**".into()]);
        assert!(should_cache_request(&c, "GET", false, "/api/data"));
        assert!(should_cache_request(&c, "GET", false, "/api"));
    }

    #[test]
    fn custom_methods_allow_post() {
        let mut c = cfg(60);
        c.methods = Some(vec!["GET".into(), "POST".into()]);
        assert!(should_cache_request(&c, "POST", false, "/"));
    }

    #[test]
    fn custom_methods_block_head_when_not_listed() {
        let mut c = cfg(60);
        c.methods = Some(vec!["GET".into()]);
        assert!(!should_cache_request(&c, "HEAD", false, "/"));
    }

    // ── path_matches ──────────────────────────────────────────────────────────

    #[test]
    fn path_matches_exact() {
        assert!(path_matches("/api/auth", "/api/auth"));
        assert!(!path_matches("/api/auth", "/api"));
    }

    #[test]
    fn path_matches_sub_path() {
        assert!(path_matches("/api/auth", "/api/auth/login"));
        assert!(!path_matches("/api/auth", "/api/authorise"));
    }

    #[test]
    fn path_matches_glob_exact() {
        assert!(path_matches("/api/auth/**", "/api/auth"));
    }

    #[test]
    fn path_matches_glob_sub_path() {
        assert!(path_matches("/api/auth/**", "/api/auth/token"));
    }
}
