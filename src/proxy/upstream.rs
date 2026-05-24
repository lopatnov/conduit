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
