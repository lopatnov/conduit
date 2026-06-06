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
                // Normalize to lowercase so that "Accept-Language" and
                // "accept-language" in the config produce the same cache key.
                // http::HeaderMap::get() is already case-insensitive, so the
                // value lookup is correct either way; the normalization here
                // ensures the key string representation is consistent.
                let name_lc = name.to_ascii_lowercase();
                let val = headers
                    .get(name_lc.as_str())
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                parts.push(format!("{name_lc}={val}"));
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
/// - Request carries an `Authorization` header — per RFC 7234 § 3.2, caching
///   authenticated responses is unsafe by default (the private response for
///   user A must not be served to user B).  This prevents cache poisoning
///   attacks where an authenticated response leaks to other users.
/// - The request path matches one of the `skipPaths` patterns.
pub fn should_cache_request(
    cfg: &CacheConfig,
    method: &str,
    has_cookie: bool,
    has_authorization: bool,
    path: &str,
) -> bool {
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

    // RFC 7234 § 3.2: never cache requests that carry an Authorization header.
    // Authenticated responses are user-specific and must not be shared via cache.
    if has_authorization {
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
///
/// When `staleWhileRevalidateSecs` or `staleIfErrorSecs` are configured
/// (or present in the upstream `Cache-Control` response header), Pingora's
/// stale-while-revalidate / stale-if-error logic is activated automatically.
pub fn response_cacheable(cfg: &CacheConfig, resp: &ResponseHeader) -> RespCacheable {
    // Only cache successful responses.
    if resp.status.as_u16() != 200 {
        return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache);
    }

    let ttl_secs = cfg.ttl_secs.unwrap_or(0);
    if ttl_secs == 0 {
        return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache);
    }

    // RFC 7234 § 3: Never cache responses that set session cookies.
    if resp.headers.contains_key("set-cookie") {
        return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache);
    }

    // RFC 7234 § 3 — respect Cache-Control directives from the upstream response.
    // A well-behaved proxy MUST obey these even when the operator configures a TTL.
    //
    // • no-store: MUST NOT store the response in any cache.
    // • private: shared caches (like Conduit) MUST NOT cache this response.
    // • no-cache: may store but must revalidate before each use; we treat it as
    //   uncacheable since we don't implement per-request revalidation.
    //
    // Pattern: nginx proxy_cache_bypass, RFC 7234 § 3.
    if let Some(cc) = resp
        .headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
    {
        let cc_lower = cc.to_ascii_lowercase();
        if cc_lower.contains("no-store")
            || cc_lower.contains("private")
            || cc_lower.contains("no-cache")
        {
            return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache);
        }
    }

    // RFC 7234 § 5.2.2.9: s-maxage overrides max-age for shared caches.
    // Respect the upstream's preference over our config TTL.
    // nginx proxy_cache also honours s-maxage.
    let effective_ttl = {
        match parse_cc_directive_opt(resp, "s-maxage") {
            Some(0) => return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache), // s-maxage=0 → don't cache
            Some(s) => s as u64, // use upstream's s-maxage value
            None => ttl_secs,    // not present → use configured TTL
        }
    };
    if effective_ttl == 0 {
        return RespCacheable::Uncacheable(NoCacheReason::OriginNotCache);
    }

    // Prefer explicit config values; fall back to upstream Cache-Control header.
    let swr = cfg
        .stale_while_revalidate_secs
        .unwrap_or_else(|| parse_cc_directive(resp, "stale-while-revalidate"));
    let sie = cfg
        .stale_if_error_secs
        .unwrap_or_else(|| parse_cc_directive(resp, "stale-if-error"));

    let now = SystemTime::now();
    let fresh_until = now + Duration::from_secs(effective_ttl);
    let meta = CacheMeta::new(fresh_until, now, swr, sie, resp.clone());
    RespCacheable::Cacheable(meta)
}

/// Parse a `Cache-Control` directive that has an integer value.
/// Returns `Some(n)` when the directive is present (0 is valid), `None` when absent.
fn parse_cc_directive_opt(resp: &ResponseHeader, directive: &str) -> Option<u32> {
    let cc = resp
        .headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())?;
    // Scan comma-separated directives.
    for part in cc.split(',') {
        let part = part.trim();
        if let Some(value_str) = part
            .strip_prefix(directive)
            .and_then(|s| s.strip_prefix('='))
        {
            if let Ok(v) = value_str.trim().parse::<u32>() {
                return Some(v);
            }
        }
    }
    None
}

/// Parse a `Cache-Control` directive value like `stale-while-revalidate=300`.
/// Returns 0 when absent or unparseable.
fn parse_cc_directive(resp: &ResponseHeader, directive: &str) -> u32 {
    let cc = resp
        .headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    for part in cc.split(',') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix(directive) {
            let val = val.trim_start_matches('=').trim();
            if let Ok(n) = val.parse::<u32>() {
                return n;
            }
        }
    }
    0
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

// ── Early refresh helpers ─────────────────────────────────────────────────────

/// Return `true` when a cached entry should be proactively refreshed.
///
/// `remaining_secs` is how many seconds of TTL are left on the cached entry.
/// `early_window_secs` is `cache.earlyRefreshSecs` from the route config.
///
/// The check is a simple comparison: if the remaining TTL is within the
/// configured window, a background refresh is warranted.
pub(crate) fn should_early_refresh(remaining_secs: u64, early_window_secs: u32) -> bool {
    early_window_secs > 0 && remaining_secs < early_window_secs as u64
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
            stale_while_revalidate_secs: None,
            stale_if_error_secs: None,
            early_refresh_secs: None,
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

    #[test]
    fn cache_key_vary_header_case_normalized() {
        // "Accept-Language" and "accept-language" in the config must produce
        // the same cache key — the lookup is case-insensitive but the old code
        // put the unnormalized name in the key string, causing misses.
        let mut h = http::HeaderMap::new();
        h.insert("accept-language", "en".parse().unwrap());

        let vary_upper = vec!["Accept-Language".to_string()];
        let vary_lower = vec!["accept-language".to_string()];

        let k_upper = build_cache_key("h.com", "https", "/", None, Some(&vary_upper), Some(&h));
        let k_lower = build_cache_key("h.com", "https", "/", None, Some(&vary_lower), Some(&h));

        assert_eq!(
            k_upper.to_compact().primary,
            k_lower.to_compact().primary,
            "vary header name case must be normalized in the cache key"
        );
    }

    // ── should_cache_request ──────────────────────────────────────────────────

    #[test]
    fn get_is_cached_by_default() {
        assert!(should_cache_request(&cfg(60), "GET", false, false, "/api"));
    }

    #[test]
    fn head_is_cached_by_default() {
        assert!(should_cache_request(&cfg(60), "HEAD", false, false, "/api"));
    }

    #[test]
    fn post_is_not_cached_by_default() {
        assert!(!should_cache_request(
            &cfg(60),
            "POST",
            false,
            false,
            "/api"
        ));
    }

    #[test]
    fn method_check_is_case_insensitive() {
        assert!(should_cache_request(&cfg(60), "get", false, false, "/"));
        assert!(should_cache_request(&cfg(60), "Get", false, false, "/"));
    }

    #[test]
    fn skip_if_cookie_blocks_when_cookie_present() {
        let mut c = cfg(60);
        c.skip_if_cookie = Some(true);
        assert!(!should_cache_request(&c, "GET", true, false, "/api"));
    }

    #[test]
    fn skip_if_cookie_allows_when_no_cookie() {
        let mut c = cfg(60);
        c.skip_if_cookie = Some(true);
        assert!(should_cache_request(&c, "GET", false, false, "/api"));
    }

    #[test]
    fn skip_if_cookie_false_ignores_cookie() {
        let mut c = cfg(60);
        c.skip_if_cookie = Some(false);
        assert!(should_cache_request(&c, "GET", true, false, "/api"));
    }

    #[test]
    fn skip_paths_exact_prefix_blocks() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth".into()]);
        assert!(!should_cache_request(&c, "GET", false, false, "/api/auth"));
        assert!(!should_cache_request(
            &c,
            "GET",
            false,
            false,
            "/api/auth/login"
        ));
    }

    #[test]
    fn skip_paths_exact_prefix_does_not_block_other_paths() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth".into()]);
        assert!(should_cache_request(&c, "GET", false, false, "/api/data"));
        assert!(should_cache_request(&c, "GET", false, false, "/api"));
    }

    #[test]
    fn skip_paths_glob_blocks_prefix_and_subpaths() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth/**".into()]);
        assert!(!should_cache_request(&c, "GET", false, false, "/api/auth"));
        assert!(!should_cache_request(&c, "GET", false, false, "/api/auth/"));
        assert!(!should_cache_request(
            &c,
            "GET",
            false,
            false,
            "/api/auth/anything"
        ));
    }

    #[test]
    fn skip_paths_glob_does_not_block_different_prefix() {
        let mut c = cfg(60);
        c.skip_paths = Some(vec!["/api/auth/**".into()]);
        assert!(should_cache_request(&c, "GET", false, false, "/api/data"));
        assert!(should_cache_request(&c, "GET", false, false, "/api"));
    }

    #[test]
    fn custom_methods_allow_post() {
        let mut c = cfg(60);
        c.methods = Some(vec!["GET".into(), "POST".into()]);
        assert!(should_cache_request(&c, "POST", false, false, "/"));
    }

    #[test]
    fn custom_methods_block_head_when_not_listed() {
        let mut c = cfg(60);
        c.methods = Some(vec!["GET".into()]);
        assert!(!should_cache_request(&c, "HEAD", false, false, "/"));
    }

    // ── Authorization blocks caching (RFC 7234 § 3.2) ────────────────────────

    #[test]
    fn authorized_request_is_never_cached() {
        // Even a plain GET with Authorization must be bypassed to prevent one
        // user's private response from being served to another.
        assert!(
            !should_cache_request(&cfg(60), "GET", false, true, "/api"),
            "GET + Authorization must not be cached"
        );
    }

    #[test]
    fn authorized_request_is_never_cached_regardless_of_cookie_setting() {
        let mut c = cfg(60);
        c.skip_if_cookie = Some(false); // cookies explicitly allowed
        assert!(
            !should_cache_request(&c, "GET", false, true, "/api"),
            "Authorization must block caching even when skipIfCookie is false"
        );
    }

    #[test]
    fn no_authorization_header_is_cached_normally() {
        assert!(
            should_cache_request(&cfg(60), "GET", false, false, "/api"),
            "request without Authorization should be cacheable"
        );
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

    // ── response_cacheable: Set-Cookie ────────────────────────────────────────

    #[test]
    fn set_cookie_response_is_not_cached() {
        use pingora_http::ResponseHeader;
        let mut resp = ResponseHeader::build(200, None).unwrap();
        resp.insert_header("set-cookie", "session=abc; HttpOnly")
            .unwrap();
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "response with Set-Cookie must not be cached"
        );
    }

    #[test]
    fn response_without_set_cookie_is_cached() {
        use pingora_http::ResponseHeader;
        let resp = ResponseHeader::build(200, None).unwrap();
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Cacheable(_)
            ),
            "response without Set-Cookie should be cacheable"
        );
    }

    // ── RFC 7234 Cache-Control compliance ─────────────────────────────────────

    fn resp_with_cc(cc: &str) -> pingora_http::ResponseHeader {
        let mut r = pingora_http::ResponseHeader::build(200, None).unwrap();
        r.insert_header("cache-control", cc).unwrap();
        r
    }

    #[test]
    fn no_store_prevents_caching() {
        let resp = resp_with_cc("no-store");
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "Cache-Control: no-store must not be cached"
        );
    }

    #[test]
    fn private_prevents_caching() {
        let resp = resp_with_cc("private");
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "Cache-Control: private must not be cached by shared proxy"
        );
    }

    #[test]
    fn no_cache_prevents_caching() {
        let resp = resp_with_cc("no-cache");
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "Cache-Control: no-cache must not be stored by proxy without revalidation"
        );
    }

    #[test]
    fn s_maxage_overrides_configured_ttl() {
        // RFC 7234 § 5.2.2.9: s-maxage takes precedence for shared caches.
        let resp = resp_with_cc("s-maxage=120");
        let cacheable = response_cacheable(&cfg(60), &resp);
        assert!(
            matches!(cacheable, RespCacheable::Cacheable(_)),
            "valid s-maxage should be cacheable"
        );
        // The freshness duration should be 120s (s-maxage) not 60s (config ttl).
        if let RespCacheable::Cacheable(meta) = cacheable {
            let fresh_secs = meta
                .fresh_until()
                .duration_since(std::time::SystemTime::now())
                .unwrap_or_default()
                .as_secs();
            assert!(
                fresh_secs >= 115 && fresh_secs <= 125,
                "s-maxage=120 must override config ttl=60; got ~{fresh_secs}s"
            );
        }
    }

    #[test]
    fn s_maxage_zero_prevents_caching() {
        let resp = resp_with_cc("s-maxage=0");
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "s-maxage=0 must not be cached"
        );
    }

    #[test]
    fn response_with_set_cookie_not_cached() {
        let mut resp = pingora_http::ResponseHeader::build(200, None).unwrap();
        resp.insert_header("set-cookie", "session=abc; HttpOnly")
            .unwrap();
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "response with Set-Cookie must not be cached"
        );
    }

    #[test]
    fn response_with_no_store_not_cached() {
        let resp = resp_with_cc("no-store");
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "no-store must prevent caching"
        );
    }

    #[test]
    fn response_with_private_not_cached() {
        let resp = resp_with_cc("private");
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "private must prevent caching"
        );
    }

    #[test]
    fn response_with_no_cache_directive_not_cached() {
        let resp = resp_with_cc("no-cache");
        assert!(
            matches!(
                response_cacheable(&cfg(60), &resp),
                RespCacheable::Uncacheable(_)
            ),
            "no-cache directive must prevent caching"
        );
    }

    // ── should_early_refresh ──────────────────────────────────────────────────
    //
    // These tests verify the TTL-window decision function that `response_filter`
    // uses to decide whether to schedule a background cache refresh.

    #[test]
    fn early_refresh_not_configured_is_false() {
        // earlyRefreshSecs = 0 (unset) → never refresh early.
        assert!(!should_early_refresh(5, 0));
    }

    #[test]
    fn early_refresh_within_window_is_true() {
        // 8 seconds remain, window is 10 s → refresh.
        assert!(should_early_refresh(8, 10));
    }

    #[test]
    fn early_refresh_at_window_boundary_is_true() {
        // 9 seconds remain, window is 10 s → refresh (strict <).
        assert!(should_early_refresh(9, 10));
    }

    #[test]
    fn early_refresh_exactly_at_window_is_false() {
        // remaining == window → not yet in the early-refresh zone.
        assert!(!should_early_refresh(10, 10));
    }

    #[test]
    fn early_refresh_well_within_ttl_is_false() {
        // Plenty of TTL left → no early refresh.
        assert!(!should_early_refresh(3600, 10));
    }

    #[test]
    fn early_refresh_zero_remaining_with_window_is_true() {
        // Already expired — still triggers (stale-if-error can serve stale).
        assert!(should_early_refresh(0, 10));
    }

    #[test]
    fn early_refresh_schema_field_serializes() {
        let mut c = cfg(60);
        c.early_refresh_secs = Some(15);
        let json = serde_json::to_string(&c).expect("must serialize");
        assert!(json.contains("earlyRefreshSecs"), "field name must be camelCase");
        let roundtrip: CacheConfig =
            serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(roundtrip.early_refresh_secs, Some(15));
    }
}
