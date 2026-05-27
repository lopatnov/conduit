//! Route matching for the `routes` array in [`SiteConfig`].
//!
//! Each [`RouteConfig`] contains a [`MatchConfig`] (path glob, method list,
//! header predicates, query predicates) plus an action (`proxy` or `static`).
//! Routes are evaluated in declaration order; the first match wins.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
    if let Some(hdr_predicates) = &m.headers {
        for (name, pattern) in hdr_predicates {
            let value = req_headers
                .get(name.as_str())
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !regex_match(pattern, value) {
                return false;
            }
        }
    }

    // 4. Query parameters.
    if let Some(query_predicates) = &m.query {
        let qs = query.unwrap_or("");
        for (param, pattern) in query_predicates {
            let value = query_param_value(qs, param).unwrap_or("");
            if !regex_match(pattern, value) {
                return false;
            }
        }
    }

    true
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
            return (
                UpstreamTarget::Local(LocalHandler::StaticFile {
                    roots,
                    options,
                    strip_prefix,
                }),
                None,
                None,
                None,
                false,
                None,
                None,
            );
        }
    }

    // No action configured — fall through to fallback.
    (
        UpstreamTarget::Local(LocalHandler::Fallback),
        None,
        None,
        None,
        false,
        None,
        None,
    )
}

/// Convert a [`ProxyRouteTarget`] to a [`RouteResult`].
fn proxy_target_to_result(
    target: &ProxyRouteTarget,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> RouteResult {
    match target {
        ProxyRouteTarget::Url(url) => {
            let Some(upstream) = router::url_to_proxy_upstream(url, None) else {
                return (
                    UpstreamTarget::Local(LocalHandler::Fallback),
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                );
            };
            (upstream, None, None, None, false, None, None)
        }
        ProxyRouteTarget::RoundRobin(urls) => {
            // Rotate using round-robin counter shared with the main router.
            let idx = {
                let key = urls.join(",");
                let counter = counters.entry(key).or_insert_with(|| AtomicUsize::new(0));
                counter.fetch_add(1, Ordering::Relaxed) % urls.len()
            };
            let url = &urls[idx];
            let Some(upstream) = router::url_to_proxy_upstream(url, None) else {
                return (
                    UpstreamTarget::Local(LocalHandler::Fallback),
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                );
            };
            (upstream, None, None, None, false, None, None)
        }
        ProxyRouteTarget::Full(cfg) => {
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
                return (
                    UpstreamTarget::Local(LocalHandler::Fallback),
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                );
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
            let pick_result = match strategy {
                LoadBalanceStrategy::Random => {
                    upstream::pick_random(&urls, route_key, counters).map(|u| (u, false))
                }
                LoadBalanceStrategy::LeastConn => {
                    upstream_health.pick_least_conn(&urls).map(|u| (u, true))
                }
                LoadBalanceStrategy::WeightedRoundRobin => {
                    upstream::pick_weighted_round_robin(&weighted, route_key, counters)
                        .map(|u| (u, false))
                }
                LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash => {
                    upstream::pick_by_hash(&urls, hash_val).map(|u| (u, false))
                }
                LoadBalanceStrategy::LeastResponseTime => {
                    upstream::pick_least_response_time(&urls, upstream_health, route_key, counters)
                        .map(|u| (u, false))
                }
                LoadBalanceStrategy::RoundRobin => {
                    upstream::pick_round_robin(&urls, route_key, counters).map(|u| (u, false))
                }
            };

            let Some((chosen_url, is_least_conn)) = pick_result else {
                return (
                    UpstreamTarget::Local(LocalHandler::Fallback),
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                );
            };

            let strip = cfg.strip_prefix.unwrap_or(false).then(|| path.to_string());

            let upstream = match router::url_to_proxy_upstream(&chosen_url, strip) {
                Some(u) => u,
                None => {
                    if is_least_conn {
                        upstream_health.conn_dec(&chosen_url);
                    }
                    return (
                        UpstreamTarget::Local(LocalHandler::Fallback),
                        None,
                        None,
                        None,
                        false,
                        None,
                        None,
                    );
                }
            };

            let retry = cfg.retry.as_ref().map(|r| RetryState {
                urls: all_urls.clone(),
                attempt: 0,
                max_attempts: r.attempts as usize,
                conditions: r.conditions.clone(),
                backoff_ms: r.backoff_ms,
            });

            let upstream_url_for_lc = is_least_conn.then(|| chosen_url.clone());

            (
                upstream,
                retry,
                cfg.timeout.clone(),
                cfg.pool.clone(),
                cfg.http2.unwrap_or(false),
                upstream_url_for_lc,
                cfg.cache.clone(),
            )
        }
    }
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
        ([b'*', b'*', rest @ ..], _) => {
            // `**` at the end of the pattern matches the rest of the string.
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
        // `*` — match one or more characters within a single path segment (no `/`).
        // Requires at least one character — use `**` to match zero or more.
        ([b'*', rest @ ..], [_, ..]) => {
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

// ── Regex value matching ──────────────────────────────────────────────────────

/// Return `true` when `value` matches `pattern`.
///
/// The pattern is first tried as a full-string regex match.  If the regex is
/// invalid it falls back to exact-string comparison (so plain header values
/// like `"Bearer"` work without escaping).
fn regex_match(pattern: &str, value: &str) -> bool {
    match Regex::new(&format!("^(?:{pattern})$")) {
        Ok(re) => re.is_match(value),
        Err(_) => value == pattern, // malformed pattern → exact match
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
}
