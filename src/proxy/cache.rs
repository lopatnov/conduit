//! Proxy-cache helpers for Phase 2.6.
//!
//! Provides:
//! - A shared `'static` in-memory storage singleton via [`cache_storage`].
//! - A deterministic [`build_cache_key`] function (host + scheme + path + query).
//! - [`should_cache_request`] — request-side policy (method, cookies, skip-paths).
//! - [`response_cacheable`] — response-side policy (status, TTL).

use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use pingora_cache::lock::{CacheKeyLockImpl, CacheLock};
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

// ── Cache lock (thundering herd prevention) ───────────────────────────────────

/// Global cache-key lock manager — prevents thundering herd on cache miss.
///
/// When multiple concurrent requests arrive for the same uncached key, only
/// the first one (the *writer*) fetches from upstream.  All others receive a
/// [`Locked::Read`] handle and wait until the writer finishes and stores the
/// response.  They then serve the cached copy without hitting the upstream at
/// all.
///
/// Timeout of 10 s means a reader waits at most 10 s for the writer before
/// giving up and making its own upstream request.
static CACHE_LOCK: OnceLock<CacheLock> = OnceLock::new();

/// Return a `'static` reference to the shared [`CacheKeyLockImpl`].
pub fn cache_lock() -> &'static CacheKeyLockImpl {
    CACHE_LOCK.get_or_init(|| CacheLock::new(Duration::from_secs(10)))
}

// ── Cache key ─────────────────────────────────────────────────────────────────

/// Build a deterministic [`CacheKey`] from the request coordinates.
///
/// The `namespace` is the `host` value so that different virtual-hosts with
/// the same path are stored independently.  The `primary` key is
/// `scheme:path` (with `?query` appended when present).
///
/// When `vary_headers` is provided, each header name is looked up in
/// `request_headers` and the `name=value` pairs are appended to the primary
/// key separated by `\0`.  This means `Accept-Language: en` and
/// `Accept-Language: fr` produce different cache entries for the same URL.
pub fn build_cache_key(
    host: &str,
    scheme: &str,
    path: &str,
    query: Option<&str>,
    vary_headers: Option<&[String]>,
    request_headers: Option<&http::HeaderMap>,
) -> CacheKey {
    let base = match query {
        Some(q) if !q.is_empty() => format!("{scheme}:{path}?{q}"),
        _ => format!("{scheme}:{path}"),
    };

    let primary = match (vary_headers, request_headers) {
        (Some(vary), Some(headers)) if !vary.is_empty() => {
            let mut parts = vec![base];
            for name in vary {
                let val = headers
                    .get(name.as_str())
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                parts.push(format!("{name}={val}"));
            }
            parts.join("\0")
        }
        _ => base,
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
        default_methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case(method))
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
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
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
        let _ = build_cache_key("example.com", "https", "/api/data", None, None, None);
    }

    #[test]
    fn cache_key_with_empty_query() {
        let _ = build_cache_key("example.com", "https", "/api/data", Some(""), None, None);
    }

    #[test]
    fn cache_key_with_query() {
        let _ = build_cache_key(
            "example.com",
            "https",
            "/search",
            Some("q=hello"),
            None,
            None,
        );
    }

    #[test]
    fn cache_key_vary_headers_with_no_request_headers_ignores_vary() {
        // vary supplied but request_headers is None → treated as base key only.
        let vary = vec!["accept-language".to_string()];
        let k1 = build_cache_key("h.com", "https", "/", None, Some(&vary), None);
        let k2 = build_cache_key("h.com", "https", "/", None, None, None);
        // Both produce the same key (vary cannot be applied without header values).
        assert_eq!(k1.to_compact().primary, k2.to_compact().primary);
    }

    #[test]
    fn cache_key_vary_headers_differentiates() {
        let mut h1 = http::HeaderMap::new();
        h1.insert("accept-language", "en".parse().unwrap());
        let mut h2 = http::HeaderMap::new();
        h2.insert("accept-language", "fr".parse().unwrap());

        let vary = vec!["accept-language".to_string()];
        let k1 = build_cache_key("h.com", "https", "/", None, Some(&vary), Some(&h1));
        let k2 = build_cache_key("h.com", "https", "/", None, Some(&vary), Some(&h2));
        // Different header values must produce different keys.
        assert_ne!(k1.to_compact().primary, k2.to_compact().primary);
    }

    #[test]
    fn cache_key_vary_headers_same_value_equal() {
        let mut h = http::HeaderMap::new();
        h.insert("accept-language", "en".parse().unwrap());
        let vary = vec!["accept-language".to_string()];
        let k1 = build_cache_key("h.com", "https", "/", None, Some(&vary), Some(&h));
        let k2 = build_cache_key("h.com", "https", "/", None, Some(&vary), Some(&h));
        // Same header value must produce the same key.
        assert_eq!(k1.to_compact().primary, k2.to_compact().primary);
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

    // ── cache_storage ─────────────────────────────────────────────────────────

    #[test]
    fn cache_storage_returns_static_reference() {
        let s1 = cache_storage();
        let s2 = cache_storage();
        // Both calls must return the same singleton address.
        assert!(std::ptr::eq(s1 as *const _, s2 as *const _));
    }

    // ── cache_lock ────────────────────────────────────────────────────────────

    #[test]
    fn cache_lock_returns_static_reference() {
        let l1 = cache_lock();
        let l2 = cache_lock();
        // Both calls must return the same singleton address.
        assert!(std::ptr::eq(
            l1 as *const CacheKeyLockImpl,
            l2 as *const CacheKeyLockImpl
        ));
    }

    #[test]
    fn cache_lock_first_caller_gets_write_permit() {
        use pingora_cache::lock::{LockStatus, Locked};
        // Use a unique key per test run (timestamp-based) to avoid dangling
        // lock state from previous runs in the same process (singleton cache).
        let unique = format!(
            "/test-write-permit-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        );
        let key = build_cache_key("lock-test.example", "https", &unique, None, None, None);
        let lock = cache_lock();
        let locked = lock.lock(&key, false);
        assert!(
            locked.is_write(),
            "first locker must receive a write permit"
        );
        // Release the permit so subsequent test runs don't see a dangling lock.
        if let Locked::Write(permit) = locked {
            lock.release(&key, permit, LockStatus::Done);
        }
    }

    // ── response_cacheable ────────────────────────────────────────────────────

    fn resp(status: u16) -> ResponseHeader {
        ResponseHeader::build(status, None).unwrap()
    }

    #[test]
    fn response_cacheable_200_with_ttl_is_cacheable() {
        let c = cfg(60);
        let r = resp(200);
        assert!(matches!(
            response_cacheable(&c, &r),
            RespCacheable::Cacheable(_)
        ));
    }

    #[test]
    fn response_cacheable_non200_is_uncacheable() {
        let c = cfg(60);
        for status in [201u16, 301, 404, 500] {
            let r = resp(status);
            assert!(
                matches!(response_cacheable(&c, &r), RespCacheable::Uncacheable(_)),
                "status {status} should be uncacheable"
            );
        }
    }

    #[test]
    fn response_cacheable_zero_ttl_is_uncacheable() {
        let c = cfg(0);
        let r = resp(200);
        assert!(matches!(
            response_cacheable(&c, &r),
            RespCacheable::Uncacheable(_)
        ));
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
