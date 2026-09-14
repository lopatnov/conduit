//! Route matching for the `routes` array in the root crate's `SiteConfig`
//! (issue #143 — moved here from the root crate's own `src/proxy/routes.rs`
//! because [`crate::config::RouteConfig`]/[`crate::config::MatchConfig`]
//! moved into this crate in this same PR; matching logic and its config
//! types travel together).
//!
//! Each [`crate::config::RouteConfig`] contains a
//! [`crate::config::MatchConfig`] (path glob, method list, header
//! predicates, query predicates) plus an action (`proxy` or `static`).
//! Routes are evaluated in declaration order; the first match wins.
//!
//! Proxy-target resolution itself lives in `crate::routes_resolve` — this
//! file owns matching only.

use std::sync::atomic::AtomicUsize;
use std::sync::OnceLock;

use dashmap::DashMap;
use regex::Regex;

use conduit_upstream::health::UpstreamRegistry;

use crate::config::{MatchConfig, RouteConfig};
use crate::outcome::ProxyResolution;
use crate::{resolve, routes_resolve};

// ── Public entry point ────────────────────────────────────────────────────────

/// Outcome of matching a single `routes[]` entry.
///
/// `pub` (not `pub(crate)`): the root crate's `router.rs` pattern-matches on
/// this cross-crate.
pub enum RouteMatch {
    /// `routes[index]` matched and has a `proxy` action.
    Proxy {
        /// Kept for symmetry with [`Self::NonProxy`] — the current sole
        /// caller (`router.rs::resolve_routes_array`) doesn't need it, since
        /// the rate-limit/priority stamp is already baked into `resolution`.
        #[allow(dead_code)]
        index: usize,
        /// Boxed: `ProxyResolution` is ~800+ bytes (embeds `ProxyReqState`,
        /// which carries several `Option<...>` config-sized fields) — far
        /// larger than the `NonProxy` variant's single `usize`, which would
        /// otherwise blow up every `RouteMatch` to the size of its largest
        /// variant (`clippy::large_enum_variant`).
        resolution: Box<ProxyResolution>,
    },
    /// `routes[index]` matched but has no `proxy` action — the caller
    /// resolves its `static` action (or falls back).
    NonProxy { index: usize },
}

/// Try to match the request against the site's `routes` list.
///
/// Returns the first matching [`RouteMatch`] or `None` when no route matches.
///
/// `pub` (not `pub(crate)`): called cross-crate from the root crate's
/// `router.rs::resolve_routes_array`.
#[allow(clippy::too_many_arguments)]
pub fn match_routes(
    routes: &[RouteConfig],
    path: &str,
    method: &str,
    req_headers: &http::HeaderMap,
    query: Option<&str>,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> Option<RouteMatch> {
    for (i, route) in routes.iter().enumerate() {
        if route_matches(&route.r#match, path, method, req_headers, query) {
            let Some(target) = &route.proxy else {
                return Some(RouteMatch::NonProxy { index: i });
            };
            let mut resolution =
                routes_resolve::resolve_route_target(target, path, counters, upstream_health);
            // Stamp the matched route's rate limit/priority (#360), same as
            // the `proxy` map path in `router.rs::resolve_legacy_proxy` —
            // applied regardless of which of `resolve_route_target`'s
            // internal outcomes (proxy/overloaded/unresolved) actually
            // returned. An index-based key (`routes[{i}]`) is used rather
            // than the match pattern itself: two `routes[]` entries can
            // legally share a path glob and differ only by method/header,
            // and would otherwise wrongly share a rate-limit bucket.
            let route_key = format!("routes[{i}]");
            let (route_rate_limit, route_priority) =
                resolve::route_limits_from_target(target, &route_key);
            resolution.state.route_rate_limit = route_rate_limit;
            resolution.state.route_priority = route_priority;
            return Some(RouteMatch::Proxy {
                index: i,
                resolution: Box::new(resolution),
            });
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
        let registry = conduit_upstream::health::UpstreamRegistry::new();
        let result = match_routes(
            &[],
            "/api/v1",
            "GET",
            &http::HeaderMap::new(),
            None,
            &counters,
            &registry,
        );
        assert!(result.is_none());
    }

    #[test]
    fn match_routes_no_match_returns_none() {
        use crate::config::{ProxyRouteTarget, RouteConfig};
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = conduit_upstream::health::UpstreamRegistry::new();
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
        );
        assert!(result.is_none());
    }

    #[test]
    fn match_routes_first_match_wins() {
        use crate::config::{ProxyRouteTarget, RouteConfig};
        use crate::outcome::ProxyOutcome;
        let counters: DashMap<String, std::sync::atomic::AtomicUsize> = DashMap::new();
        let registry = conduit_upstream::health::UpstreamRegistry::new();
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
        );
        let route_match = result.expect("must match");
        let RouteMatch::Proxy { resolution, .. } = route_match else {
            panic!("expected a Proxy match");
        };
        // First route should win — addr must contain port 4000.
        match resolution.outcome {
            ProxyOutcome::Upstream(pu) => {
                assert!(
                    pu.addr.contains("4000"),
                    "expected first:4000, got {}",
                    pu.addr
                )
            }
            other => panic!("expected Upstream outcome, got {other:?}"),
        }
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
