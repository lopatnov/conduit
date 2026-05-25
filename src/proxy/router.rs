use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::config::schema::{
    AppConfig, ConnectionPoolConfig, LoadBalanceStrategy, ProxyConfig, ProxyRouteTarget,
    ProxyTimeout, RetryConfig, SiteConfig, StaticConfig,
};
use crate::proxy::ctx::{LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::upstream;

/// Resolved routing result: all per-route data needed to populate `RequestCtx`.
///
/// Fields: (upstream, retry, timeout, pool, http2, upstream_url_for_least_conn)
type RouteResult = (
    UpstreamTarget,
    Option<RetryState>,
    Option<ProxyTimeout>,
    Option<ConnectionPoolConfig>,
    bool,           // proxy_http2
    Option<String>, // upstream URL selected (for least-conn decrement)
);

pub fn route_request(
    config: &AppConfig,
    host: &str,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> RequestCtx {
    let site_idx = find_site_idx(config, host).unwrap_or(0);
    let site = config.sites.get(site_idx);

    let (upstream, retry, proxy_timeout, proxy_pool, proxy_http2, proxy_upstream_url) =
        if is_health_path(site, path) {
            (
                UpstreamTarget::Local(LocalHandler::Health),
                None,
                None,
                None,
                false,
                None,
            )
        } else if let Some(token) = metrics_token(site, path) {
            (
                UpstreamTarget::Local(LocalHandler::Metrics { token }),
                None,
                None,
                None,
                false,
                None,
            )
        } else if let Some(site) = site {
            route_site(site, path, counters, upstream_health)
        } else {
            (
                UpstreamTarget::Local(LocalHandler::Fallback),
                None,
                None,
                None,
                false,
                None,
            )
        };

    RequestCtx::new(
        site_idx,
        upstream,
        retry,
        proxy_timeout,
        proxy_pool,
        proxy_http2,
        proxy_upstream_url,
    )
}

fn route_site(
    site: &SiteConfig,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> RouteResult {
    // Proxy routes take priority over static files.
    if let Some(proxy_cfg) = &site.proxy {
        if let Some(result) = resolve_proxy(proxy_cfg, path, counters, upstream_health) {
            return result;
        }
    }

    // Static files (only reached if no proxy route matched).
    if let Some(static_cfg) = &site.static_files {
        let options = Arc::new(site.static_options.clone().unwrap_or_default());
        let (roots, strip_prefix) = resolve_static_roots(static_cfg, path);
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
            );
        }
    }

    (
        UpstreamTarget::Local(LocalHandler::Fallback),
        None,
        None,
        None,
        false,
        None,
    )
}

fn resolve_proxy(
    config: &ProxyConfig,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> Option<RouteResult> {
    match config {
        ProxyConfig::Single(url) => Some((
            url_to_proxy_upstream(url, None)?,
            None,
            None,
            None,
            false,
            None,
        )),
        ProxyConfig::Routes(routes) => {
            let (route_key, route_target) = find_route(routes, path)?;
            let all_urls = upstream::target_urls(route_target);

            let (retry_cfg, proxy_timeout, proxy_pool, strategy, proxy_http2) = match route_target {
                ProxyRouteTarget::Full(cfg) => (
                    cfg.retry.as_ref(),
                    cfg.timeout.clone(),
                    cfg.pool.clone(),
                    cfg.strategy.as_ref(),
                    cfg.http2.unwrap_or(false),
                ),
                _ => (None, None, None, None, false),
            };

            // Filter to healthy upstreams; if all are down keep all (fail-open).
            let healthy = upstream_health.filter_healthy(&all_urls);
            let urls: Vec<String> = healthy.into_iter().cloned().collect();

            let (chosen_url, retry_state, is_least_conn) = pick_url_by_strategy(
                urls,
                route_key,
                counters,
                retry_cfg,
                strategy,
                upstream_health,
            )?;

            let strip = if upstream::strip_prefix_enabled(route_target) {
                Some(route_key.trim_end_matches('/').to_string())
            } else {
                None
            };

            // Only store the upstream URL when using least-conn so the logging()
            // hook can decrement the per-upstream inflight counter.
            let proxy_upstream_url = if is_least_conn {
                Some(chosen_url.clone())
            } else {
                None
            };

            Some((
                url_to_proxy_upstream(&chosen_url, strip)?,
                retry_state,
                proxy_timeout,
                proxy_pool,
                proxy_http2,
                proxy_upstream_url,
            ))
        }
    }
}

/// Pick a URL and optional retry state according to the configured strategy.
///
/// Returns `(url, retry_state, is_least_conn)`.  `is_least_conn` is `true`
/// when the inflight counter on `upstream_health` has already been incremented
/// so the caller knows to store the URL for later decrement.
fn pick_url_by_strategy(
    urls: Vec<String>,
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
    retry_cfg: Option<&RetryConfig>,
    strategy: Option<&LoadBalanceStrategy>,
    upstream_health: &UpstreamRegistry,
) -> Option<(String, Option<RetryState>, bool)> {
    // With retry configured, always use round-robin rotation regardless of strategy.
    if let Some(retry) = retry_cfg {
        let (url, state) = pick_with_retry(urls, route_key, counters, retry)?;
        return Some((url, Some(state), false));
    }

    match strategy.unwrap_or(&LoadBalanceStrategy::RoundRobin) {
        LoadBalanceStrategy::Random => {
            let url = upstream::pick_random(&urls, route_key, counters)?;
            Some((url, None, false))
        }
        LoadBalanceStrategy::LeastConn => {
            let url = upstream_health.pick_least_conn(&urls)?;
            Some((url, None, true))
        }
        // RoundRobin is the default; WeightedRoundRobin, LeastResponseTime,
        // IpHash, ConsistentHash fall back to RoundRobin (Phase 2.5b).
        _ => {
            let url = upstream::pick_round_robin(&urls, route_key, counters)?;
            Some((url, None, false))
        }
    }
}

/// Convert a target URL + optional strip prefix into an `UpstreamTarget::Proxy`.
fn url_to_proxy_upstream(url: &str, strip_prefix: Option<String>) -> Option<UpstreamTarget> {
    let addr = upstream::url_to_host_port(url)?;
    let tls = upstream::url_is_tls(url);
    let sni = if tls {
        upstream::url_host(url)
    } else {
        String::new()
    };
    Some(UpstreamTarget::Proxy {
        addr,
        tls,
        sni,
        strip_prefix,
    })
}

/// Pick a starting URL and build retry state, rotating the URL list so that
/// `upstream_peer()` can walk it on each attempt.
fn pick_with_retry(
    urls: Vec<String>,
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
    retry: &RetryConfig,
) -> Option<(String, RetryState)> {
    let start_idx = if urls.len() > 1 {
        let entry = counters
            .entry(route_key.to_owned())
            .or_insert_with(|| AtomicUsize::new(0));
        entry.fetch_add(1, Ordering::Relaxed) % urls.len()
    } else {
        0
    };
    let rotated: Vec<String> = urls[start_idx..]
        .iter()
        .chain(urls[..start_idx].iter())
        .cloned()
        .collect();
    let first = rotated.first()?.clone();
    let state = RetryState {
        urls: rotated,
        attempt: 0,
        max_attempts: retry.attempts as usize,
        conditions: retry.conditions.clone(),
        backoff_ms: retry.backoff_ms,
    };
    Some((first, state))
}

fn find_route<'a>(
    routes: &'a indexmap::IndexMap<String, crate::config::schema::ProxyRouteTarget>,
    path: &str,
) -> Option<(&'a str, &'a crate::config::schema::ProxyRouteTarget)> {
    let mut best: Option<(&str, &crate::config::schema::ProxyRouteTarget)> = None;
    for (prefix, target) in routes {
        let norm = prefix.trim_end_matches('/');
        let matches = if norm.is_empty() {
            true
        } else {
            path == norm || path.starts_with(&format!("{norm}/"))
        };
        if matches {
            let cur_len = norm.len();
            let best_len = best.map_or(0, |(b, _)| b.trim_end_matches('/').len());
            if cur_len >= best_len {
                best = Some((prefix.as_str(), target));
            }
        }
    }
    best
}

fn resolve_static_roots(cfg: &StaticConfig, path: &str) -> (Vec<PathBuf>, Option<String>) {
    match cfg {
        StaticConfig::Single(s) => (vec![PathBuf::from(s)], None),
        StaticConfig::Multi(v) => (v.iter().map(PathBuf::from).collect(), None),
        StaticConfig::Mapped(m) => match find_best_mapped_prefix(m, path) {
            Some((pfx, root)) => (vec![PathBuf::from(root)], Some(pfx.to_string())),
            None => (vec![], None),
        },
    }
}

/// Find the longest prefix in a mapped static config that matches `path`.
fn find_best_mapped_prefix<'a>(
    m: &'a indexmap::IndexMap<String, String>,
    path: &str,
) -> Option<(&'a str, &'a str)> {
    let mut best: Option<(&str, &str)> = None;
    for (prefix, root) in m {
        let norm = prefix.trim_end_matches('/');
        let matches = norm.is_empty() || path == norm || path.starts_with(&format!("{norm}/"));
        if matches {
            let len = norm.len();
            if best.is_none_or(|(b, _)| len > b.trim_end_matches('/').len()) {
                best = Some((prefix.as_str(), root.as_str()));
            }
        }
    }
    best
}

/// Returns `Some(token)` when `path` matches the configured metrics endpoint.
/// `token` is `None` when the endpoint has no auth token.
fn metrics_token(site: Option<&SiteConfig>, path: &str) -> Option<Option<String>> {
    let site = site?;
    let metrics = site.metrics.as_ref()?;
    let bare = path.split('?').next().unwrap_or(path);
    let metrics_path = metrics.path.as_deref().unwrap_or("/__metrics__");
    if bare == metrics_path {
        Some(metrics.token.clone())
    } else {
        None
    }
}

fn is_health_path(site: Option<&SiteConfig>, path: &str) -> bool {
    let bare = path.split('?').next().unwrap_or(path);
    let default_path = "/__health__";
    if let Some(site) = site {
        if let Some(hc) = &site.health_check {
            use crate::config::schema::HealthCheckConfig;
            match hc {
                HealthCheckConfig::Enabled(false) => return false,
                HealthCheckConfig::Enabled(true) => return bare == default_path,
                HealthCheckConfig::Options(opts) => {
                    let p = opts.path.as_deref().unwrap_or(default_path);
                    return bare == p;
                }
            }
        }
    }
    bare == default_path
}

fn find_site_idx(config: &AppConfig, host: &str) -> Option<usize> {
    if config.sites.is_empty() {
        return None;
    }
    for (i, site) in config.sites.iter().enumerate() {
        if site.host.as_deref() == Some(host) {
            return Some(i);
        }
    }
    for (i, site) in config.sites.iter().enumerate() {
        if matches!(site.host.as_deref(), None | Some("*")) {
            return Some(i);
        }
    }
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        AppConfig, HealthCheckConfig, HealthCheckOptions, LoadBalanceStrategy, MetricsConfig,
        RetryConfig, SiteConfig,
    };
    use crate::proxy::health::UpstreamRegistry;

    // ── find_route ────────────────────────────────────────────────────────────

    #[test]
    fn find_route_longest_prefix_wins() {
        use crate::config::schema::ProxyRouteTarget;
        use indexmap::IndexMap;
        let mut routes = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Url("http://root:4000".to_string()),
        );
        routes.insert(
            "/api".to_string(),
            ProxyRouteTarget::Url("http://api:4000".to_string()),
        );
        routes.insert(
            "/api/v2".to_string(),
            ProxyRouteTarget::Url("http://apiv2:4000".to_string()),
        );
        let (key, _) = find_route(&routes, "/api/v2/users").unwrap();
        assert_eq!(key, "/api/v2");
    }

    #[test]
    fn find_route_root_catches_all() {
        use crate::config::schema::ProxyRouteTarget;
        use indexmap::IndexMap;
        let mut routes = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Url("http://root:4000".to_string()),
        );
        let (key, _) = find_route(&routes, "/anything/here").unwrap();
        assert_eq!(key, "/");
    }

    #[test]
    fn find_route_no_match_returns_none() {
        use crate::config::schema::ProxyRouteTarget;
        use indexmap::IndexMap;
        let mut routes = IndexMap::new();
        routes.insert(
            "/api".to_string(),
            ProxyRouteTarget::Url("http://api:4000".to_string()),
        );
        assert!(find_route(&routes, "/other").is_none());
    }

    // ── find_best_mapped_prefix ───────────────────────────────────────────────

    #[test]
    fn mapped_prefix_longest_wins() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/".to_string(), "./root".to_string());
        m.insert("/docs".to_string(), "./docs".to_string());
        let (pfx, root) = find_best_mapped_prefix(&m, "/docs/guide").unwrap();
        assert_eq!(pfx, "/docs");
        assert_eq!(root, "./docs");
    }

    #[test]
    fn mapped_prefix_no_match_returns_none() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/docs".to_string(), "./docs".to_string());
        assert!(find_best_mapped_prefix(&m, "/other").is_none());
    }

    // ── is_health_path ────────────────────────────────────────────────────────

    #[test]
    fn health_path_default_matches() {
        assert!(is_health_path(None, "/__health__"));
    }

    #[test]
    fn health_path_non_health_does_not_match() {
        assert!(!is_health_path(None, "/other"));
    }

    #[test]
    fn health_path_disabled_via_false() {
        let site = SiteConfig {
            health_check: Some(HealthCheckConfig::Enabled(false)),
            ..Default::default()
        };
        assert!(!is_health_path(Some(&site), "/__health__"));
    }

    #[test]
    fn health_path_enabled_via_true() {
        let site = SiteConfig {
            health_check: Some(HealthCheckConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(is_health_path(Some(&site), "/__health__"));
    }

    #[test]
    fn health_path_custom_path_configured() {
        let site = SiteConfig {
            health_check: Some(HealthCheckConfig::Options(HealthCheckOptions {
                path: Some("/health".to_string()),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(is_health_path(Some(&site), "/health"));
        assert!(!is_health_path(Some(&site), "/__health__"));
    }

    // ── metrics_token ─────────────────────────────────────────────────────────

    #[test]
    fn metrics_token_no_site_returns_none() {
        assert!(metrics_token(None, "/__metrics__").is_none());
    }

    #[test]
    fn metrics_token_default_path_no_auth() {
        let site = SiteConfig {
            metrics: Some(MetricsConfig {
                path: None,
                token: None,
            }),
            ..Default::default()
        };
        assert_eq!(metrics_token(Some(&site), "/__metrics__"), Some(None));
    }

    #[test]
    fn metrics_token_custom_path_with_token() {
        let site = SiteConfig {
            metrics: Some(MetricsConfig {
                path: Some("/m".to_string()),
                token: Some("secret".to_string()),
            }),
            ..Default::default()
        };
        assert_eq!(
            metrics_token(Some(&site), "/m"),
            Some(Some("secret".to_string()))
        );
        assert!(metrics_token(Some(&site), "/__metrics__").is_none());
    }

    // ── find_site_idx ─────────────────────────────────────────────────────────

    #[test]
    fn site_idx_empty_config_returns_none() {
        assert!(find_site_idx(&AppConfig::default(), "example.com").is_none());
    }

    #[test]
    fn site_idx_exact_host_match() {
        let config = AppConfig {
            sites: vec![
                SiteConfig {
                    host: Some("other.com".to_string()),
                    ..Default::default()
                },
                SiteConfig {
                    host: Some("example.com".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(find_site_idx(&config, "example.com"), Some(1));
    }

    #[test]
    fn site_idx_wildcard_fallback() {
        let config = AppConfig {
            sites: vec![
                SiteConfig {
                    host: Some("example.com".to_string()),
                    ..Default::default()
                },
                SiteConfig {
                    host: Some("*".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(find_site_idx(&config, "other.com"), Some(1));
    }

    // ── pick_with_retry ───────────────────────────────────────────────────────

    #[test]
    fn retry_single_url_builds_state() {
        let urls = vec!["http://a:4000".to_string()];
        let counters = DashMap::new();
        let retry = RetryConfig {
            attempts: 3,
            conditions: vec!["5xx".to_string()],
            backoff_ms: None,
        };
        let (url, state) = pick_with_retry(urls, "r", &counters, &retry).unwrap();
        assert_eq!(url, "http://a:4000");
        assert_eq!(state.max_attempts, 3);
        assert!(state.has_condition("5xx"));
        assert!(!state.has_condition("connection_error"));
    }

    #[test]
    fn retry_multiple_urls_rotates() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let counters = DashMap::new();
        let retry = RetryConfig {
            attempts: 2,
            conditions: vec!["connection_error".to_string()],
            backoff_ms: Some(50),
        };
        let (url, state) = pick_with_retry(urls.clone(), "r", &counters, &retry).unwrap();
        assert!(urls.contains(&url));
        assert_eq!(state.urls.len(), 2);
        assert_eq!(state.backoff_ms, Some(50));
    }

    #[test]
    fn retry_empty_urls_returns_none() {
        let counters = DashMap::new();
        let retry = RetryConfig {
            attempts: 3,
            conditions: vec![],
            backoff_ms: None,
        };
        assert!(pick_with_retry(vec![], "r", &counters, &retry).is_none());
    }

    // ── pick_url_by_strategy ──────────────────────────────────────────────────

    #[test]
    fn strategy_default_round_robin() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let (url, retry, is_lc) =
            pick_url_by_strategy(urls.clone(), "r", &counters, None, None, &reg).unwrap();
        assert!(urls.contains(&url));
        assert!(retry.is_none());
        assert!(!is_lc);
    }

    #[test]
    fn strategy_random_returns_valid_url() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let (url, _, is_lc) = pick_url_by_strategy(
            urls.clone(),
            "r",
            &counters,
            None,
            Some(&LoadBalanceStrategy::Random),
            &reg,
        )
        .unwrap();
        assert!(urls.contains(&url));
        assert!(!is_lc);
    }

    #[test]
    fn strategy_least_conn_increments_counter() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let (url, _, is_lc) = pick_url_by_strategy(
            urls.clone(),
            "r",
            &counters,
            None,
            Some(&LoadBalanceStrategy::LeastConn),
            &reg,
        )
        .unwrap();
        assert!(urls.contains(&url));
        assert!(is_lc, "least-conn must set the is_least_conn flag");
        assert_eq!(
            reg.conn_load(&url),
            1,
            "inflight counter must be incremented"
        );
    }

    #[test]
    fn strategy_with_retry_overrides_load_balancing() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let retry_cfg = RetryConfig {
            attempts: 3,
            conditions: vec!["5xx".to_string()],
            backoff_ms: None,
        };
        let (url, retry, is_lc) = pick_url_by_strategy(
            urls.clone(),
            "r",
            &counters,
            Some(&retry_cfg),
            Some(&LoadBalanceStrategy::LeastConn),
            &reg,
        )
        .unwrap();
        assert!(urls.contains(&url));
        assert!(retry.is_some(), "retry state must be built");
        assert!(!is_lc, "retry path never sets least-conn flag");
    }

    #[test]
    fn strategy_empty_urls_returns_none() {
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        assert!(pick_url_by_strategy(vec![], "r", &counters, None, None, &reg).is_none());
    }
}
