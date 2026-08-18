//! Route matching for the `routes` array in [`SiteConfig`].
//!
//! Each [`RouteConfig`] contains a [`MatchConfig`] (path glob, method list,
//! header predicates, query predicates) plus an action (`proxy` or `static`).
//! Routes are evaluated in declaration order; the first match wins.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use regex::Regex;

use crate::config::schema::{
    LoadBalanceStrategy, MatchConfig, ProxyRouteTarget, RouteConfig, StaticOptions,
};
use crate::proxy::ctx::{LocalHandler, RetryState, UpstreamTarget};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::{router, upstream};

/// Result type shared with the main router.
type RouteResult = router::RouteResultAlias;

// ── Public entry point ────────────────────────────────────────────────────────

/// Try to match the request against the site's `routes` list.
///
/// Returns the first matching [`UpstreamTarget`] (wrapped in a `RouteResult`)
/// or `None` when no route matches.
#[allow(clippy::too_many_arguments)]
pub fn match_routes(
    routes: &[RouteConfig],
    path: &str,
    method: &str,
    req_headers: &http::HeaderMap,
    query: Option<&str>,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
    static_options: Option<&StaticOptions>,
) -> Option<RouteResult> {
    for route in routes {
        if route_matches(&route.r#match, path, method, req_headers, query) {
            return Some(route_to_result(
                route,
                path,
                counters,
                upstream_health,
                static_options,
            ));
        }
    }
    None
}

// ── Match evaluation ──────────────────────────────────────────────────────────

/// Returns `true` when all criteria in `m` are satisfied by the request.
fn route_matches(
    m: &MatchConfig,
    path: &str,
    method: &str,
    req_headers: &http::HeaderMap,
    query: Option<&str>,
) -> bool {
    // 1. Path glob.
    if let Some(pat) = &m.path {
        if !glob_match(pat, path) {
            return false;
        }
    }
    // 2. Method.
    if let Some(methods) = &m.method {
        if !methods.iter().any(|m| m.eq_ignore_ascii_case(method)) {
            return false;
        }
    }
    // 3. Request headers.
    if let Some(hdr) = &m.headers {
        if !headers_match(hdr, req_headers) {
            return false;
        }
    }
    // 4. Query parameters.
    if let Some(qry) = &m.query {
        if !query_params_match(qry, query) {
            return false;
        }
    }
    // 5. Cookies.
    if let Some(cookies) = &m.cookies {
        let cookie_header = req_headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !cookies_match(cookies, cookie_header) {
            return false;
        }
    }
    true
}

/// Return `true` when every header predicate in `predicates` is satisfied.
fn headers_match(
    predicates: &indexmap::IndexMap<String, String>,
    req_headers: &http::HeaderMap,
) -> bool {
    for (name, pattern) in predicates {
        let value = req_headers
            .get(name.as_str())
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !regex_match(pattern, value) {
            return false;
        }
    }
    true
}

/// Return `true` when every query-parameter predicate in `predicates` is satisfied.
fn query_params_match(
    predicates: &indexmap::IndexMap<String, String>,
    query: Option<&str>,
) -> bool {
    let qs = query.unwrap_or("");
    for (param, pattern) in predicates {
        let value = query_param_value(qs, param).unwrap_or("");
        if !regex_match(pattern, value) {
            return false;
        }
    }
    true
}

/// Return `true` when every cookie predicate in `predicates` is satisfied.
///
/// Parses the `Cookie` header value (e.g. `"a=1; b=2"`) and matches each
/// named cookie against the given pattern using the same regex semantics as
/// header and query matching.
fn cookies_match(predicates: &indexmap::IndexMap<String, String>, cookie_header: &str) -> bool {
    for (name, pattern) in predicates {
        let value = cookie_value(cookie_header, name).unwrap_or("");
        if !regex_match(pattern, value) {
            return false;
        }
    }
    true
}

/// Return the value of cookie `name` from a `Cookie` header value string.
///
/// The `Cookie` header format is `name1=val1; name2=val2; …`.
fn cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            if k.trim() == name {
                return Some(v.trim());
            }
        }
    }
    None
}

// ── Action dispatch ───────────────────────────────────────────────────────────

/// Convert a matched [`RouteConfig`] into a [`RouteResult`].
///
/// Priority: `proxy` beats `static`.  If neither is set the route is a
/// no-op (unlikely in practice) and returns the fallback.
fn route_to_result(
    route: &RouteConfig,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
    static_options: Option<&StaticOptions>,
) -> RouteResult {
    // ── Proxy action ─────────────────────────────────────────────────────────
    if let Some(proxy_target) = &route.proxy {
        return proxy_target_to_result(proxy_target, path, counters, upstream_health);
    }

    // ── Static action ─────────────────────────────────────────────────────────
    if let Some(static_cfg) = &route.static_files {
        let options = Arc::new(static_options.cloned().unwrap_or_default());
        let (roots, strip_prefix) = router::resolve_static_roots(static_cfg, path);
        if !roots.is_empty() {
            return router::RouteResolution::local(UpstreamTarget::Local(
                LocalHandler::StaticFile {
                    roots,
                    options,
                    strip_prefix,
                },
            ));
        }
    }

    // No action configured — fall through to fallback.
    fallback_result()
}

/// Convert a [`ProxyRouteTarget`] to a [`RouteResult`].
fn proxy_target_to_result(
    target: &ProxyRouteTarget,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> RouteResult {
    match target {
        ProxyRouteTarget::Url(url) => url_target_to_result(url),
        ProxyRouteTarget::RoundRobin(urls) => round_robin_target_to_result(urls, counters),
        ProxyRouteTarget::Full(cfg) => full_cfg_to_result(cfg, path, counters, upstream_health),
    }
}

/// Handle the `Full` form of a proxy route target (strategy selection, health filtering, etc.).
fn full_cfg_to_result(
    cfg: &crate::config::schema::ProxyRouteConfig,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> RouteResult {
    let all_urls: Vec<String> = cfg
        .targets
        .iter()
        .map(|t| match t {
            crate::config::schema::ProxyTarget::Simple(u) => u.clone(),
            crate::config::schema::ProxyTarget::Weighted(w) => w.url.clone(),
        })
        .collect();
    let all_weighted: Vec<(String, u32)> = cfg
        .targets
        .iter()
        .map(|t| match t {
            crate::config::schema::ProxyTarget::Simple(u) => (u.clone(), 1),
            crate::config::schema::ProxyTarget::Weighted(w) => (w.url.clone(), w.weight),
        })
        .collect();

    // Filter to healthy upstreams; fail-open when all are down.
    let healthy = upstream_health.filter_healthy(&all_urls);
    let urls: Vec<String> = healthy.iter().cloned().cloned().collect();
    let weighted: Vec<(String, u32)> = all_weighted
        .iter()
        .filter(|(u, _)| urls.contains(u))
        .cloned()
        .collect();

    if urls.is_empty() {
        return fallback_result();
    }

    // Use path as the hash input since client IP is not available at
    // route-match time (the routes array doesn't carry it through).
    let hash_val = upstream::fnv1a_hash(path);

    // Pick URL using the configured strategy.
    let strategy = cfg
        .strategy
        .as_ref()
        .unwrap_or(&LoadBalanceStrategy::RoundRobin);
    let route_key = path; // stable key for round-robin counters
    let pick_result = pick_by_strategy(
        strategy,
        &urls,
        &weighted,
        hash_val,
        route_key,
        counters,
        upstream_health,
    );

    let Some((chosen_url, is_least_conn)) = pick_result else {
        return fallback_result();
    };

    let strip = cfg.strip_prefix.unwrap_or(false).then(|| path.to_string());

    let upstream = match router::url_to_proxy_upstream(&chosen_url, strip) {
        Some(u) => u,
        None => {
            if is_least_conn {
                upstream_health.conn_dec(&chosen_url);
            }
            return fallback_result();
        }
    };

    let retry = cfg.retry.as_ref().map(|r| RetryState {
        urls: all_urls.clone(),
        attempt: 0,
        max_attempts: r.attempts as usize,
        conditions: r.conditions.clone(),
        backoff_ms: r.backoff_ms,
        backoff_jitter: r.backoff_jitter.unwrap_or(false),
        budget_percent: r.budget_percent,
        is_retrying: false,
    });

    // proxy_upstream_url is populated unconditionally (#155) so passive-health
    // attribution works for every strategy; upstream_conn_slot tracks whether
    // this request actually holds the conn_count slot least-conn acquired.
    let proxy_upstream_url = Some(chosen_url.clone());

    router::RouteResolution {
        upstream,
        retry,
        proxy_timeout: cfg.timeout.clone(),
        proxy_pool: cfg.pool.clone(),
        proxy_http2: cfg.http2.unwrap_or(false),
        proxy_upstream_url,
        upstream_conn_slot: is_least_conn,
        proxy_cache_cfg: cfg.cache.clone(),
        passive_unhealthy_status: cfg
            .health_check
            .as_ref()
            .and_then(|hc| hc.unhealthy_status.clone())
            .unwrap_or_default(),
        passive_unhealthy_latency_ms: cfg
            .health_check
            .as_ref()
            .and_then(|hc| hc.unhealthy_latency_ms),
        websocket_allowed: cfg.websocket.unwrap_or(false),
        sticky_set_cookie: None, // routes.rs path: sticky is handled in router.rs
    }
}

/// Select an upstream URL from `urls` using the given strategy.
///
/// Returns `Some((url, is_least_conn))` or `None` if no peer is available.
fn pick_by_strategy(
    strategy: &LoadBalanceStrategy,
    urls: &[String],
    weighted: &[(String, u32)],
    hash_val: u64,
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> Option<(String, bool)> {
    match strategy {
        LoadBalanceStrategy::Random => {
            upstream::pick_random(urls, route_key, counters).map(|u| (u, false))
        }
        LoadBalanceStrategy::LeastConn => upstream_health.pick_least_conn(urls).map(|u| (u, true)),
        LoadBalanceStrategy::WeightedRoundRobin => {
            upstream::pick_weighted_round_robin(weighted, route_key, counters).map(|u| (u, false))
        }
        LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash => {
            upstream::pick_by_hash(urls, hash_val).map(|u| (u, false))
        }
        LoadBalanceStrategy::LeastResponseTime => {
            upstream::pick_least_response_time(urls, upstream_health, route_key, counters)
                .map(|u| (u, false))
        }
        LoadBalanceStrategy::RoundRobin => {
            upstream::pick_round_robin(urls, route_key, counters).map(|u| (u, false))
        }
        LoadBalanceStrategy::P2c => crate::proxy::strategy::from_config(&LoadBalanceStrategy::P2c)
            .pick(
                urls,
                weighted,
                route_key,
                hash_val,
                counters,
                upstream_health,
            ),
    }
}

/// Convert a single-URL `ProxyRouteTarget::Url` to a [`RouteResult`].
fn url_target_to_result(url: &str) -> RouteResult {
    match router::url_to_proxy_upstream(url, None) {
        Some(upstream) => router::RouteResolution::local(upstream),
        None => fallback_result(),
    }
}

/// Rotate through `urls` round-robin and convert the chosen URL to a [`RouteResult`].
fn round_robin_target_to_result(
    urls: &[String],
    counters: &DashMap<String, AtomicUsize>,
) -> RouteResult {
    let key = urls.join(",");
    let counter = counters.entry(key).or_insert_with(|| AtomicUsize::new(0));
    let idx = counter.fetch_add(1, Ordering::Relaxed) % urls.len();
    match router::url_to_proxy_upstream(&urls[idx], None) {
        Some(upstream) => router::RouteResolution::local(upstream),
        None => fallback_result(),
    }
}

/// Convenience: return the canonical "fall through to fallback" result.
#[inline]
fn fallback_result() -> RouteResult {
    router::RouteResolution::local(UpstreamTarget::Local(LocalHandler::Fallback))
}

// ── Glob path matching ────────────────────────────────────────────────────────

/// Return `true` when `path` matches the glob `pattern`.
///
/// Pattern syntax:
/// - `**` matches any sequence of characters including `/`.
/// - `*` matches any sequence of characters within a single path segment (no `/`).
/// - `?` matches any single non-`/` character.
/// - All other characters match literally.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), path.as_bytes())
}

fn glob_match_inner(pat: &[u8], s: &[u8]) -> bool {
    match (pat, s) {
        // Both exhausted — success.
        ([], []) => true,
        // Pattern exhausted but string still has characters.
        ([], _) => false,
        // `**` — try matching 0 or more characters (including `/`).
        ([b'*', b'*', rest @ ..], _) => glob_double_star(rest, s),
        // `*` — match one or more characters within a single path segment (no `/`).
        ([b'*', rest @ ..], [_, ..]) => glob_single_star(rest, s),
        // `*` against empty string — never matches.
        ([b'*', ..], []) => false,
        // `?` — match any single non-`/` character.
        ([b'?', rest_p @ ..], [c, rest_s @ ..]) if *c != b'/' => glob_match_inner(rest_p, rest_s),
        ([b'?', ..], _) => false,
        // Literal character match.
        ([pc, rest_p @ ..], [sc, rest_s @ ..]) if pc == sc => glob_match_inner(rest_p, rest_s),
        _ => false,
    }
}

/// `**` handler: matches zero or more characters including `/`.
fn glob_double_star(rest: &[u8], s: &[u8]) -> bool {
    // `**` at the end of the pattern matches everything.
    if rest.is_empty() {
        return true;
    }
    // Otherwise try matching at each position in `s`.
    for i in 0..=s.len() {
        if glob_match_inner(rest, &s[i..]) {
            return true;
        }
    }
    false
}

/// `*` handler: matches one or more characters within a single path segment (no `/`).
fn glob_single_star(rest: &[u8], s: &[u8]) -> bool {
    for i in 1..=s.len() {
        // Don't let `*` cross a `/`.
        if s[i - 1] == b'/' {
            break;
        }
        if glob_match_inner(rest, &s[i..]) {
            return true;
        }
    }
    false
}

// ── Regex value matching ──────────────────────────────────────────────────────

/// Global cache of compiled, anchored regexes used in route header/query matching.
///
/// Patterns are keyed by the raw string; the value is the compiled [`Regex`]
/// wrapped in an anchored `^(?:…)$` form.  Invalid patterns are not stored so
/// they fall back to exact-string comparison without re-attempting compilation.
///
/// Maximum number of compiled regular expressions to keep in the cache.
///
/// Patterns come from the admin-controlled config, so in practice there are
/// only a few dozen at most.  The cap is a defence-in-depth safety net against
/// an unbounded DashMap growth in pathological configs (e.g. thousands of
/// header-match patterns across many config reloads).
const MAX_REGEX_CACHE: usize = 2_048;

/// Return a compiled, anchored version of `pattern`, reusing a cached copy
/// when available.
fn get_anchored_regex(pattern: &str) -> Option<Regex> {
    static CACHE: OnceLock<DashMap<String, Regex>> = OnceLock::new();
    let cache = CACHE.get_or_init(DashMap::new);
    if let Some(re) = cache.get(pattern) {
        return Some(re.clone());
    }
    // Don't grow the cache past the safety cap — fall back to compile-each-time
    // for new patterns once full.  This keeps memory bounded without silently
    // dropping existing cached patterns.
    let re = Regex::new(&format!("^(?:{pattern})$")).ok()?;
    if cache.len() < MAX_REGEX_CACHE {
        cache.insert(pattern.to_owned(), re.clone());
    }
    Some(re)
}

/// Return `true` when `value` matches `pattern`.
///
/// The pattern is first tried as a full-string regex match.  If the regex is
/// invalid it falls back to exact-string comparison (so plain header values
/// like `"Bearer"` work without escaping).
fn regex_match(pattern: &str, value: &str) -> bool {
    match get_anchored_regex(pattern) {
        Some(re) => re.is_match(value),
        None => value == pattern, // malformed pattern → exact match
    }
}

// ── Query string parsing ──────────────────────────────────────────────────────

/// Return the first value for `key` in the URL query string `qs`.
fn query_param_value<'a>(qs: &'a str, key: &str) -> Option<&'a str> {
    for part in qs.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            if k == key {
                return Some(v);
            }
        } else if part == key {
            return Some("");
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::LoadBalanceStrategy;
    use indexmap::IndexMap;

    // ── glob_match ────────────────────────────────────────────────────────────

    #[test]
    fn glob_exact_match() {
        assert!(glob_match("/health", "/health"));
        assert!(!glob_match("/health", "/healthz"));
    }

    #[test]
    fn glob_star_single_segment() {
        assert!(glob_match("/blog/*", "/blog/hello"));
        assert!(!glob_match("/blog/*", "/blog/hello/world")); // crosses `/`
        assert!(!glob_match("/blog/*", "/blog/")); // empty segment
    }

    #[test]
    fn glob_double_star_any_depth() {
        assert!(glob_match("/api/**", "/api/"));
        assert!(glob_match("/api/**", "/api/v1/users"));
        assert!(glob_match("/api/**", "/api/v1/users/42/settings"));
        assert!(!glob_match("/api/**", "/other/v1"));
    }

    #[test]
    fn glob_double_star_at_end() {
        assert!(glob_match("/**", "/"));
        assert!(glob_match("/**", "/any/path/here"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("/v?", "/v1"));
        assert!(glob_match("/v?", "/v2"));
        assert!(!glob_match("/v?", "/v12")); // two chars, not one
        assert!(!glob_match("/v?", "/v/")); // `/` not matched by `?`
    }

    #[test]
    fn glob_no_pattern_chars() {
        assert!(glob_match("/static/style.css", "/static/style.css"));
        assert!(!glob_match("/static/style.css", "/static/other.css"));
    }

    // ── regex_match ───────────────────────────────────────────────────────────

    #[test]
    fn regex_exact_value() {
        assert!(regex_match("Bearer", "Bearer"));
        assert!(!regex_match("Bearer", "Basic something"));
    }

    #[test]
    fn regex_pattern_match() {
        assert!(regex_match("Bearer .+", "Bearer token123"));
        assert!(!regex_match("Bearer .+", "Basic user:pass"));
    }

    #[test]
    fn regex_invalid_falls_back_to_exact() {
        // `[invalid` is an invalid regex — falls back to exact comparison.
        assert!(regex_match("[invalid", "[invalid")); // exact match succeeds
        assert!(!regex_match("[invalid", "other"));
    }

    // ── query_param_value ────────────────────────────────────────────────────

    #[test]
    fn query_param_found() {
        assert_eq!(query_param_value("foo=bar&baz=qux", "foo"), Some("bar"));
        assert_eq!(query_param_value("foo=bar&baz=qux", "baz"), Some("qux"));
    }

    #[test]
    fn query_param_not_found() {
        assert_eq!(query_param_value("foo=bar", "missing"), None);
    }

    #[test]
    fn query_param_empty_value() {
        assert_eq!(query_param_value("flag", "flag"), Some(""));
    }

    #[test]
    fn query_param_empty_string() {
        assert_eq!(query_param_value("", "foo"), None);
    }

    // ── route_matches ─────────────────────────────────────────────────────────

    #[test]
    fn route_matches_path_only() {
        let m = MatchConfig {
            path: Some("/api/**".to_string()),
            ..Default::default()
        };
        assert!(route_matches(
            &m,
            "/api/v1/users",
            "GET",
            &http::HeaderMap::new(),
            None
        ));
        assert!(!route_matches(
            &m,
            "/other",
            "GET",
            &http::HeaderMap::new(),
            None
        ));
    }

    #[test]
    fn route_matches_method_filter() {
        let m = MatchConfig {
            method: Some(vec!["POST".to_string(), "PUT".to_string()]),
            ..Default::default()
        };
        assert!(route_matches(
            &m,
            "/any",
            "POST",
            &http::HeaderMap::new(),
            None
        ));
        assert!(route_matches(
            &m,
            "/any",
            "put",
            &http::HeaderMap::new(),
            None
        )); // case-insensitive
        assert!(!route_matches(
            &m,
            "/any",
            "GET",
            &http::HeaderMap::new(),
            None
        ));
    }

    #[test]
    fn route_matches_header_predicate() {
        let mut headers_map = IndexMap::new();
        headers_map.insert("x-api-version".to_string(), "v2".to_string());
        let m = MatchConfig {
            headers: Some(headers_map),
            ..Default::default()
        };

        let mut req_headers = http::HeaderMap::new();
        req_headers.insert("x-api-version", http::HeaderValue::from_static("v2"));
        assert!(route_matches(&m, "/any", "GET", &req_headers, None));

        let empty_headers = http::HeaderMap::new();
        assert!(!route_matches(&m, "/any", "GET", &empty_headers, None));
    }

    #[test]
    fn route_matches_query_predicate() {
        let mut query_map = IndexMap::new();
        query_map.insert("version".to_string(), "2".to_string());
        let m = MatchConfig {
            query: Some(query_map),
            ..Default::default()
        };
        assert!(route_matches(
            &m,
            "/any",
            "GET",
            &http::HeaderMap::new(),
            Some("version=2&other=x")
        ));
        assert!(!route_matches(
            &m,
            "/any",
            "GET",
            &http::HeaderMap::new(),
            Some("version=1")
        ));
    }

    #[test]
    fn route_matches_no_criteria_matches_everything() {
        let m = MatchConfig::default();
        assert!(route_matches(&m, "/", "GET", &http::HeaderMap::new(), None));
        assert!(route_matches(
            &m,
            "/any/path",
            "DELETE",
            &http::HeaderMap::new(),
            Some("x=1")
        ));
    }

    #[test]
    fn route_matches_combined_path_and_method() {
        let m = MatchConfig {
            path: Some("/api/**".to_string()),
            method: Some(vec!["POST".to_string()]),
            ..Default::default()
        };
        assert!(route_matches(
            &m,
            "/api/v1",
            "POST",
            &http::HeaderMap::new(),
            None
        ));
        assert!(!route_matches(
            &m,
            "/api/v1",
            "GET",
            &http::HeaderMap::new(),
            None
        ));
        assert!(!route_matches(
            &m,
            "/other",
            "POST",
            &http::HeaderMap::new(),
            None
        ));
    }

    // ── match_routes ──────────────────────────────────────────────────────────

    #[test]
    fn match_routes_empty_list_returns_none() {
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let result = match_routes(
            &[],
            "/api/v1",
            "GET",
            &http::HeaderMap::new(),
            None,
            &counters,
            &registry,
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn match_routes_no_match_returns_none() {
        use crate::config::schema::{ProxyRouteTarget, RouteConfig};
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let routes = vec![RouteConfig {
            r#match: MatchConfig {
                path: Some("/api/**".to_string()),
                ..Default::default()
            },
            proxy: Some(ProxyRouteTarget::Url("http://api:4000".to_string())),
            static_files: None,
        }];
        let result = match_routes(
            &routes,
            "/other",
            "GET",
            &http::HeaderMap::new(),
            None,
            &counters,
            &registry,
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn match_routes_first_match_wins() {
        use crate::config::schema::{ProxyRouteTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let routes = vec![
            RouteConfig {
                r#match: MatchConfig {
                    path: Some("/api/**".to_string()),
                    ..Default::default()
                },
                proxy: Some(ProxyRouteTarget::Url("http://first:4000".to_string())),
                static_files: None,
            },
            RouteConfig {
                r#match: MatchConfig {
                    path: Some("/**".to_string()),
                    ..Default::default()
                },
                proxy: Some(ProxyRouteTarget::Url("http://second:5000".to_string())),
                static_files: None,
            },
        ];
        let result = match_routes(
            &routes,
            "/api/users",
            "GET",
            &http::HeaderMap::new(),
            None,
            &counters,
            &registry,
            None,
        );
        assert!(result.is_some());
        let target = result.unwrap().upstream;
        // First route should win — addr must contain port 4000.
        if let UpstreamTarget::Proxy { addr, .. } = target {
            assert!(addr.contains("4000"), "expected first:4000, got {addr}");
        } else {
            panic!("expected Proxy target");
        }
    }

    // ── route_to_result / proxy_target_to_result ──────────────────────────────

    #[test]
    fn route_to_result_url_proxy() {
        use crate::config::schema::{ProxyRouteTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Url("http://backend:4000".to_string())),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_invalid_url_gives_fallback() {
        use crate::config::schema::{ProxyRouteTarget, RouteConfig};
        use crate::proxy::ctx::{LocalHandler, UpstreamTarget};
        // "http://" has an empty host segment — url_to_host_port returns None.
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Url("http://".to_string())),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(
            target,
            UpstreamTarget::Local(LocalHandler::Fallback)
        ));
    }

    #[test]
    fn route_to_result_round_robin_proxy() {
        use crate::config::schema::{ProxyRouteTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::RoundRobin(vec![
                "http://b1:4000".to_string(),
                "http://b2:4001".to_string(),
            ])),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_round_robin_rotates() {
        use crate::config::schema::{ProxyRouteTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::RoundRobin(vec![
                "http://b1:4000".to_string(),
                "http://b2:4001".to_string(),
            ])),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();

        let t1 = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        let t2 = route_to_result(&route, "/api", &counters, &registry, None).upstream;

        // Round-robin must select different upstreams on consecutive calls.
        let a1 = match t1 {
            UpstreamTarget::Proxy { ref addr, .. } => addr.clone(),
            _ => panic!("expected Proxy target for first pick"),
        };
        let a2 = match t2 {
            UpstreamTarget::Proxy { ref addr, .. } => addr.clone(),
            _ => panic!("expected Proxy target for second pick"),
        };
        assert_ne!(a1, a2, "round-robin must rotate across upstreams");
    }

    #[test]
    fn route_to_result_no_proxy_no_static_gives_fallback() {
        use crate::config::schema::RouteConfig;
        use crate::proxy::ctx::{LocalHandler, UpstreamTarget};
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: None,
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(
            target,
            UpstreamTarget::Local(LocalHandler::Fallback)
        ));
    }

    #[test]
    fn route_to_result_full_proxy_round_robin() {
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![
                    ProxyTarget::Simple("http://b1:4000".to_string()),
                    ProxyTarget::Simple("http://b2:4001".to_string()),
                ],
                strategy: Some(LoadBalanceStrategy::RoundRobin),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_full_proxy_random_strategy() {
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
                strategy: Some(LoadBalanceStrategy::Random),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_full_proxy_least_conn_strategy() {
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
                strategy: Some(LoadBalanceStrategy::LeastConn),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_full_proxy_ip_hash_strategy() {
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
                strategy: Some(LoadBalanceStrategy::IpHash),
                hash_key: Some("ip".to_string()),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api/users", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_full_proxy_weighted_round_robin() {
        use crate::config::schema::{
            ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig, WeightedTarget,
        };
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![
                    ProxyTarget::Weighted(WeightedTarget {
                        url: "http://b1:4000".to_string(),
                        weight: 3,
                    }),
                    ProxyTarget::Weighted(WeightedTarget {
                        url: "http://b2:4001".to_string(),
                        weight: 1,
                    }),
                ],
                strategy: Some(LoadBalanceStrategy::WeightedRoundRobin),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_full_proxy_consistent_hash() {
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
                strategy: Some(LoadBalanceStrategy::ConsistentHash),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api/items/42", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_full_proxy_all_unhealthy_fails_open() {
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        use crate::proxy::health::UpstreamEntry;

        let registry = crate::proxy::health::UpstreamRegistry::new();
        // Mark the only upstream as unhealthy.
        registry.statuses.insert(
            "http://b1:4000".to_string(),
            UpstreamEntry {
                healthy: false,
                ..Default::default()
            },
        );

        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
                strategy: Some(LoadBalanceStrategy::RoundRobin),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        // Fail-open: when all upstreams are unhealthy the router falls back to
        // the full (unhealthy) pool rather than refusing traffic.
        assert!(
            matches!(target, UpstreamTarget::Proxy { .. }),
            "fail-open must still pick from the full upstream pool, got {target:?}"
        );
    }

    #[test]
    fn route_to_result_full_proxy_with_strip_prefix_and_retry() {
        use crate::config::schema::{
            ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RetryConfig, RouteConfig,
        };
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
                strategy: Some(LoadBalanceStrategy::RoundRobin),
                strip_prefix: Some(true),
                retry: Some(RetryConfig {
                    attempts: 2,
                    conditions: vec!["connection_error".to_string()],
                    backoff_ms: Some(50),
                    backoff_jitter: None,
                    budget_percent: None,
                }),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let _res = route_to_result(&route, "/api/users", &counters, &registry, None);
        let target = _res.upstream;
        let retry = _res.retry;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
        assert!(retry.is_some(), "expected retry state to be populated");
    }

    #[test]
    fn route_to_result_full_proxy_least_response_time() {
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::UpstreamTarget;
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![
                    ProxyTarget::Simple("http://b1:4000".to_string()),
                    ProxyTarget::Simple("http://b2:4001".to_string()),
                ],
                strategy: Some(LoadBalanceStrategy::LeastResponseTime),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(target, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_to_result_static_files() {
        use crate::config::schema::{RouteConfig, StaticConfig};
        use crate::proxy::ctx::{LocalHandler, UpstreamTarget};
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: None,
            static_files: Some(StaticConfig::Single("./dist".to_string())),
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/", &counters, &registry, None).upstream;
        assert!(matches!(
            target,
            UpstreamTarget::Local(LocalHandler::StaticFile { .. })
        ));
    }

    #[test]
    fn route_to_result_round_robin_invalid_url_gives_fallback() {
        // "http://" has an empty host — url_to_proxy_upstream returns None → Fallback.
        use crate::config::schema::{ProxyRouteTarget, RouteConfig};
        use crate::proxy::ctx::{LocalHandler, UpstreamTarget};
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::RoundRobin(vec!["http://".to_string()])),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(
            target,
            UpstreamTarget::Local(LocalHandler::Fallback)
        ));
    }

    #[test]
    fn route_to_result_full_proxy_empty_targets_gives_fallback() {
        // Empty targets list → filter_healthy returns empty → Fallback.
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, RouteConfig};
        use crate::proxy::ctx::{LocalHandler, UpstreamTarget};
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![],
                strategy: Some(LoadBalanceStrategy::RoundRobin),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(
            target,
            UpstreamTarget::Local(LocalHandler::Fallback)
        ));
    }

    #[test]
    fn route_to_result_full_proxy_least_conn_invalid_url_gives_fallback() {
        // LeastConn + invalid URL (bare "http://"): the URL cannot be resolved
        // to a host:port, so route_to_result returns Fallback.
        use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RouteConfig};
        use crate::proxy::ctx::{LocalHandler, UpstreamTarget};
        let route = RouteConfig {
            r#match: MatchConfig::default(),
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://".to_string())],
                strategy: Some(LoadBalanceStrategy::LeastConn),
                ..Default::default()
            }))),
            static_files: None,
        };
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = crate::proxy::health::UpstreamRegistry::new();
        let target = route_to_result(&route, "/api", &counters, &registry, None).upstream;
        assert!(matches!(
            target,
            UpstreamTarget::Local(LocalHandler::Fallback)
        ));
        // Verify the connection counter was not left incremented.
        // (LeastConn increments the counter before picking the peer; on parse
        // failure it must decrement before returning Fallback.)
        let conn_count = counters
            .get("http://")
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);
        assert_eq!(
            conn_count, 0,
            "conn counter must be zero after invalid-URL fallback"
        );
    }

    // ── cookie_value ──────────────────────────────────────────────────────────

    #[test]
    fn cookie_value_found() {
        assert_eq!(cookie_value("a=1; b=2; c=3", "b"), Some("2"));
    }

    #[test]
    fn cookie_value_first_cookie() {
        assert_eq!(cookie_value("session=abc; other=x", "session"), Some("abc"));
    }

    #[test]
    fn cookie_value_not_found() {
        assert_eq!(cookie_value("a=1; b=2", "missing"), None);
    }

    #[test]
    fn cookie_value_empty_header() {
        assert_eq!(cookie_value("", "any"), None);
    }

    // ── cookies_match ─────────────────────────────────────────────────────────

    #[test]
    fn cookies_match_exact() {
        let mut predicates = IndexMap::new();
        predicates.insert("beta".to_string(), "1".to_string());
        assert!(cookies_match(&predicates, "beta=1; session=abc"));
        assert!(!cookies_match(&predicates, "beta=0; session=abc"));
        assert!(!cookies_match(&predicates, "session=abc"));
    }

    #[test]
    fn cookies_match_regex() {
        let mut predicates = IndexMap::new();
        predicates.insert("experiment".to_string(), "blue|green".to_string());
        assert!(cookies_match(&predicates, "experiment=blue"));
        assert!(cookies_match(&predicates, "experiment=green"));
        assert!(!cookies_match(&predicates, "experiment=red"));
    }

    #[test]
    fn cookies_match_multiple_predicates() {
        let mut predicates = IndexMap::new();
        predicates.insert("beta".to_string(), "1".to_string());
        predicates.insert("group".to_string(), "A|B".to_string());
        assert!(cookies_match(&predicates, "beta=1; group=A"));
        assert!(cookies_match(&predicates, "group=B; beta=1; other=x"));
        assert!(!cookies_match(&predicates, "beta=1; group=C")); // group doesn't match
        assert!(!cookies_match(&predicates, "beta=0; group=A")); // beta doesn't match
    }

    // ── route_matches with cookies ────────────────────────────────────────────

    #[test]
    fn route_matches_cookie_predicate() {
        let mut cookies_map = IndexMap::new();
        cookies_map.insert("beta".to_string(), "1".to_string());
        let m = MatchConfig {
            cookies: Some(cookies_map),
            ..Default::default()
        };

        // Cookie present with correct value.
        let mut req_headers = http::HeaderMap::new();
        req_headers.insert(
            "cookie",
            http::HeaderValue::from_static("beta=1; session=abc"),
        );
        assert!(route_matches(&m, "/any", "GET", &req_headers, None));

        // Cookie absent.
        assert!(!route_matches(
            &m,
            "/any",
            "GET",
            &http::HeaderMap::new(),
            None
        ));

        // Cookie present with wrong value.
        let mut wrong_headers = http::HeaderMap::new();
        wrong_headers.insert("cookie", http::HeaderValue::from_static("beta=0"));
        assert!(!route_matches(&m, "/any", "GET", &wrong_headers, None));
    }

    // ── cookie_value extra cases ──────────────────────────────────────────────

    #[test]
    fn cookie_value_with_whitespace() {
        assert_eq!(cookie_value("  key = value ", "key"), Some("value"));
    }

    // ── query_param_value extra cases ─────────────────────────────────────────

    #[test]
    fn query_param_value_key_only_returns_empty() {
        // "flag" without "=value" is a boolean parameter.
        assert_eq!(query_param_value("flag&other=1", "flag"), Some(""));
    }

    #[test]
    fn query_param_value_multiple_with_same_key_returns_first() {
        // Only the first matching key is returned.
        assert_eq!(query_param_value("a=1&a=2", "a"), Some("1"));
    }

    // ── get_anchored_regex ────────────────────────────────────────────────────

    #[test]
    fn get_anchored_regex_valid_pattern() {
        let re = get_anchored_regex("v[12]");
        assert!(re.is_some(), "valid pattern must compile");
        let re = re.unwrap();
        assert!(re.is_match("v1"));
        assert!(re.is_match("v2"));
        assert!(!re.is_match("v3"));
    }

    #[test]
    fn get_anchored_regex_invalid_pattern_returns_none() {
        let re = get_anchored_regex("[invalid");
        assert!(re.is_none(), "invalid pattern must return None");
    }

    #[test]
    fn get_anchored_regex_anchors_full_string() {
        // Anchored regex must match only the full string, not substrings.
        let re = get_anchored_regex("v1").unwrap();
        assert!(re.is_match("v1"), "exact match must pass");
        assert!(!re.is_match("v10"), "prefix match must not pass (anchored)");
    }
}
