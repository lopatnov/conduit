use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;

use crate::config::schema::{ProxyConfig, ProxyRouteTarget, ProxyTarget};

/// Parse "http://host:port/path" or "https://host:port/path" → "host:port".
///
/// Handles IPv6 literals correctly:
/// - `http://[::1]:8080/` → `[::1]:8080`
/// - `http://[::1]/`      → `[::1]:80`
pub fn url_to_host_port(url: &str) -> Option<String> {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host_port = without_scheme.split('/').next()?;
    if host_port.is_empty() {
        return None;
    }
    let default_port = if url.starts_with("https://") {
        "443"
    } else {
        "80"
    };
    if host_port.starts_with('[') {
        // IPv6 literal: [::1]:8080 (has port) or [::1] (no port).
        if host_port.contains("]:") {
            Some(host_port.to_string())
        } else {
            Some(format!("{host_port}:{default_port}"))
        }
    } else if host_port.contains(':') {
        // host:port — already includes explicit port.
        Some(host_port.to_string())
    } else {
        Some(format!("{host_port}:{default_port}"))
    }
}

pub fn url_is_tls(url: &str) -> bool {
    url.starts_with("https://")
}

/// Extract the bare hostname for SNI (no brackets, no port).
///
/// `https://[::1]:8443/` → `::1`
/// `https://example.com:443/` → `example.com`
pub fn url_host(url: &str) -> String {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host_port = without_scheme.split('/').next().unwrap_or("");
    if host_port.starts_with('[') {
        // IPv6 literal: strip brackets, ignore port.
        host_port
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or("")
            .to_string()
    } else {
        host_port.split(':').next().unwrap_or(host_port).to_string()
    }
}

/// Collect all target URLs from a route target (Url / RoundRobin / Full).
pub fn target_urls(route_target: &ProxyRouteTarget) -> Vec<String> {
    match route_target {
        ProxyRouteTarget::Url(url) => vec![url.clone()],
        ProxyRouteTarget::RoundRobin(urls) => urls.clone(),
        ProxyRouteTarget::Full(cfg) => cfg
            .targets
            .iter()
            .map(|t| match t {
                ProxyTarget::Simple(url) => url.clone(),
                ProxyTarget::Weighted(w) => w.url.clone(),
            })
            .collect(),
    }
}

/// Flatten all target URLs from a `ProxyConfig` (all routes, all targets).
pub fn target_urls_from_proxy(proxy: &ProxyConfig) -> Vec<String> {
    match proxy {
        ProxyConfig::Single(url) => vec![url.clone()],
        ProxyConfig::Routes(routes) => routes.values().flat_map(target_urls).collect(),
    }
}

/// Returns `true` if the route config has `stripPrefix: true`.
pub fn strip_prefix_enabled(route_target: &ProxyRouteTarget) -> bool {
    match route_target {
        ProxyRouteTarget::Full(cfg) => cfg.strip_prefix.unwrap_or(false),
        _ => false,
    }
}

/// Pick the next URL from `targets` using round-robin, keyed by `route_key`.
/// The counter lives in `counters` so state is shared across requests.
pub fn pick_round_robin(
    targets: &[String],
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
) -> Option<String> {
    if targets.is_empty() {
        return None;
    }
    if targets.len() == 1 {
        return Some(targets[0].clone());
    }
    let entry = counters
        .entry(route_key.to_owned())
        .or_insert_with(|| AtomicUsize::new(0));
    let idx = entry.fetch_add(1, Ordering::Relaxed) % targets.len();
    Some(targets[idx].clone())
}

/// Pick a URL from `targets` pseudo-randomly using the current nanosecond
/// timestamp XOR'd with a per-route counter.
///
/// This is intentionally lightweight (no external crate) and provides
/// sufficient distribution for load-balancing purposes.
pub fn pick_random(
    targets: &[String],
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
) -> Option<String> {
    if targets.is_empty() {
        return None;
    }
    if targets.len() == 1 {
        return Some(targets[0].clone());
    }
    // Mix nanosecond wall-clock time with a per-route counter to avoid
    // sequential correlations when multiple requests arrive in the same nanosecond.
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let counter = counters
        .entry(format!("{route_key}__rng"))
        .or_insert_with(|| AtomicUsize::new(0))
        .fetch_add(1, Ordering::Relaxed);
    let idx = (ns ^ counter.wrapping_mul(2_654_435_761)) % targets.len();
    Some(targets[idx].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── url_to_host_port ──────────────────────────────────────────────────────

    #[test]
    fn host_port_http_explicit() {
        assert_eq!(
            url_to_host_port("http://example.com:8080/path"),
            Some("example.com:8080".to_string())
        );
    }

    #[test]
    fn host_port_https_default_port() {
        assert_eq!(
            url_to_host_port("https://example.com/"),
            Some("example.com:443".to_string())
        );
    }

    #[test]
    fn host_port_http_default_port() {
        assert_eq!(
            url_to_host_port("http://example.com"),
            Some("example.com:80".to_string())
        );
    }

    #[test]
    fn host_port_ipv6_with_port() {
        assert_eq!(
            url_to_host_port("http://[::1]:8080/"),
            Some("[::1]:8080".to_string())
        );
    }

    #[test]
    fn host_port_ipv6_no_port() {
        assert_eq!(
            url_to_host_port("http://[::1]/"),
            Some("[::1]:80".to_string())
        );
    }

    #[test]
    fn host_port_empty_host_returns_none() {
        assert_eq!(url_to_host_port("http://"), None);
    }

    // ── url_is_tls ────────────────────────────────────────────────────────────

    #[test]
    fn tls_true_for_https() {
        assert!(url_is_tls("https://example.com/"));
    }

    #[test]
    fn tls_false_for_http() {
        assert!(!url_is_tls("http://example.com/"));
    }

    // ── url_host ──────────────────────────────────────────────────────────────

    #[test]
    fn host_strips_port() {
        assert_eq!(url_host("http://example.com:8080/"), "example.com");
    }

    #[test]
    fn host_ipv6_strips_brackets_and_port() {
        assert_eq!(url_host("https://[::1]:8443/"), "::1");
    }

    // ── target_urls ───────────────────────────────────────────────────────────

    #[test]
    fn target_urls_single_string() {
        let t = ProxyRouteTarget::Url("http://a:4000".to_string());
        assert_eq!(target_urls(&t), vec!["http://a:4000"]);
    }

    #[test]
    fn target_urls_round_robin_list() {
        let t = ProxyRouteTarget::RoundRobin(vec![
            "http://a:4000".to_string(),
            "http://b:4000".to_string(),
        ]);
        assert_eq!(target_urls(&t), vec!["http://a:4000", "http://b:4000"]);
    }

    #[test]
    fn target_urls_full_simple_targets() {
        use crate::config::schema::{ProxyRouteConfig, ProxyTarget};
        let cfg = ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Simple("http://a:4000".to_string()),
                ProxyTarget::Simple("http://b:4000".to_string()),
            ],
            ..Default::default()
        };
        assert_eq!(
            target_urls(&ProxyRouteTarget::Full(Box::new(cfg))),
            vec!["http://a:4000", "http://b:4000"]
        );
    }

    #[test]
    fn target_urls_full_weighted_targets() {
        use crate::config::schema::{ProxyRouteConfig, ProxyTarget, WeightedTarget};
        let cfg = ProxyRouteConfig {
            targets: vec![ProxyTarget::Weighted(WeightedTarget {
                url: "http://a:4000".to_string(),
                weight: 3,
            })],
            ..Default::default()
        };
        assert_eq!(
            target_urls(&ProxyRouteTarget::Full(Box::new(cfg))),
            vec!["http://a:4000"]
        );
    }

    // ── strip_prefix_enabled ──────────────────────────────────────────────────

    #[test]
    fn strip_prefix_false_for_url_target() {
        assert!(!strip_prefix_enabled(&ProxyRouteTarget::Url(
            "http://a:4000".to_string()
        )));
    }

    #[test]
    fn strip_prefix_true_when_configured() {
        use crate::config::schema::ProxyRouteConfig;
        let cfg = ProxyRouteConfig {
            strip_prefix: Some(true),
            ..Default::default()
        };
        assert!(strip_prefix_enabled(&ProxyRouteTarget::Full(Box::new(cfg))));
    }

    // ── pick_round_robin ──────────────────────────────────────────────────────

    #[test]
    fn round_robin_none_on_empty() {
        let counters = DashMap::new();
        assert!(pick_round_robin(&[], "r", &counters).is_none());
    }

    #[test]
    fn round_robin_single_target_always_returned() {
        let counters = DashMap::new();
        let targets = vec!["http://a:4000".to_string()];
        assert_eq!(
            pick_round_robin(&targets, "r", &counters),
            Some("http://a:4000".to_string())
        );
    }

    #[test]
    fn round_robin_cycles_targets() {
        let counters = DashMap::new();
        let targets = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let a = pick_round_robin(&targets, "r", &counters).unwrap();
        let b = pick_round_robin(&targets, "r", &counters).unwrap();
        let c = pick_round_robin(&targets, "r", &counters).unwrap();
        assert_ne!(a, b, "round-robin must alternate");
        assert_eq!(a, c, "third pick wraps back to first");
    }

    // ── pick_random ───────────────────────────────────────────────────────────

    #[test]
    fn random_none_on_empty() {
        let counters = DashMap::new();
        assert!(pick_random(&[], "r", &counters).is_none());
    }

    #[test]
    fn random_single_target_returned() {
        let counters = DashMap::new();
        let targets = vec!["http://a:4000".to_string()];
        assert_eq!(
            pick_random(&targets, "r", &counters),
            Some("http://a:4000".to_string())
        );
    }

    #[test]
    fn random_distributes_across_targets() {
        let counters = DashMap::new();
        let targets = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            seen.insert(pick_random(&targets, "r", &counters).unwrap());
        }
        assert_eq!(seen.len(), 2, "both targets should appear across 100 calls");
    }

    // ── target_urls_from_proxy ────────────────────────────────────────────────

    #[test]
    fn from_proxy_single_url() {
        let proxy = ProxyConfig::Single("http://a:4000".to_string());
        assert_eq!(target_urls_from_proxy(&proxy), vec!["http://a:4000"]);
    }

    #[test]
    fn from_proxy_routes_flattened() {
        use indexmap::IndexMap;
        let mut routes = IndexMap::new();
        routes.insert(
            "/api".to_string(),
            ProxyRouteTarget::Url("http://a:4000".to_string()),
        );
        routes.insert(
            "/web".to_string(),
            ProxyRouteTarget::RoundRobin(vec![
                "http://b:4000".to_string(),
                "http://c:4000".to_string(),
            ]),
        );
        let mut urls = target_urls_from_proxy(&ProxyConfig::Routes(routes));
        urls.sort();
        assert_eq!(
            urls,
            vec!["http://a:4000", "http://b:4000", "http://c:4000"]
        );
    }
}
