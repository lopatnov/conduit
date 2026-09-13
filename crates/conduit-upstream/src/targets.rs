//! Upstream URL parsing and load-balancing "pick" algorithms.
//!
//! Moved out of the root crate's `src/proxy/upstream.rs` (issue #114/#142).
//! The four functions that used to live in this file operating on
//! `ProxyRouteTarget`/`ProxyConfig`/`ProxyTarget` (`target_urls`,
//! `weighted_targets`, `target_urls_from_proxy`, `strip_prefix_enabled`)
//! **stayed behind** in the root crate's own `src/proxy/upstream.rs` — those
//! types are still root-crate-only (`ProxyRouteTarget::Full` embeds
//! `ProxyRouteConfig`, a large struct itself embedding `CacheConfig`/
//! `RetryConfig`/`ConnectionPoolConfig`/etc., none of which belong in an
//! upstream-selection crate), so moving them here would force
//! `ProxyRouteConfig` to move too — a genuine circular dependency with
//! several other not-yet-extracted crates. Everything below operates on
//! plain `&[String]`/`&[(String, u32)]` slices instead and has no such
//! dependency.

use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;

/// Parse "http://host:port/path" or "https://host:port/path" → "host:port".
///
/// Handles IPv6 literals correctly:
/// - `http://[::1]:8080/` → `[::1]:8080`
/// - `http://[::1]/`      → `[::1]:80`
pub fn url_to_host_port(url: &str) -> Option<String> {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host_port = without_scheme.split(['/', '?', '#']).next()?;
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
    let host_port = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
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

/// Pick a URL using weighted round-robin.
///
/// Each entry in `targets` is `(url, weight)`.  The counter advances by 1 each
/// call; the winner is whichever bucket the counter falls into when the total
/// weight is used as the modulus.
///
/// Example: weights [3, 1] → target 0 selected for counter values 0, 1, 2;
/// target 1 selected for counter value 3; then repeats.
pub fn pick_weighted_round_robin(
    targets: &[(String, u32)],
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
) -> Option<String> {
    if targets.is_empty() {
        return None;
    }
    if targets.len() == 1 {
        return Some(targets[0].0.clone());
    }
    let total: usize = targets.iter().map(|(_, w)| *w as usize).sum();
    if total == 0 {
        return Some(targets[0].0.clone());
    }
    let entry = counters
        .entry(format!("{route_key}__wrr"))
        .or_insert_with(|| AtomicUsize::new(0));
    let slot = entry.fetch_add(1, Ordering::Relaxed) % total;
    let mut cumulative = 0usize;
    for (url, weight) in targets {
        cumulative += *weight as usize;
        if slot < cumulative {
            return Some(url.clone());
        }
    }
    Some(targets.last().unwrap().0.clone())
}

/// Pick a URL by mapping a precomputed hash value to a bucket index.
///
/// Used for `ip-hash` and `consistent-hash` strategies.  Both strategies hash
/// a key (client IP, request URL, etc.) outside this function and pass the
/// result in; this function just does the modulo mapping.
pub fn pick_by_hash(urls: &[String], hash_val: u64) -> Option<String> {
    if urls.is_empty() {
        return None;
    }
    Some(urls[(hash_val as usize) % urls.len()].clone())
}

/// Pick the URL with the lowest observed latency from the upstream registry.
///
/// If no probe data is available for any URL the function falls back to
/// round-robin so traffic is distributed evenly during warm-up.  URLs whose
/// latency is unknown (probe hasn't run yet) are ranked last (treated as
/// `u64::MAX`).
pub fn pick_least_response_time(
    urls: &[String],
    registry: &crate::health::UpstreamRegistry,
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
) -> Option<String> {
    let has_data = urls.iter().any(|u| {
        registry
            .statuses
            .get(u)
            .and_then(|e| e.latency_ms)
            .is_some()
    });
    if !has_data {
        return pick_round_robin(urls, route_key, counters);
    }
    urls.iter()
        .min_by_key(|u| {
            registry
                .statuses
                .get(*u)
                .and_then(|e| e.latency_ms)
                .unwrap_or(u64::MAX)
        })
        .cloned()
}

/// FNV-1a 64-bit hash of a string — used as the hash key for ip-hash and
/// consistent-hash load balancing.
pub fn fnv1a_hash(s: &str) -> u64 {
    const OFFSET: u64 = 14_695_981_039_346_656_037;
    const PRIME: u64 = 1_099_511_628_211;
    s.bytes()
        .fold(OFFSET, |h, b| h.wrapping_mul(PRIME) ^ b as u64)
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

    #[test]
    fn host_port_strips_query_string() {
        // URLs with query strings must not include '?' in the host:port.
        assert_eq!(
            url_to_host_port("http://backend:8080?health=1"),
            Some("backend:8080".to_string())
        );
    }

    #[test]
    fn host_port_strips_fragment() {
        assert_eq!(
            url_to_host_port("http://backend:8080#section"),
            Some("backend:8080".to_string())
        );
    }

    #[test]
    fn host_strips_query_string() {
        assert_eq!(url_host("http://example.com:8080?x=1"), "example.com");
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

    // ── pick_weighted_round_robin ─────────────────────────────────────────────

    #[test]
    fn wrr_none_on_empty() {
        let counters = DashMap::new();
        assert!(pick_weighted_round_robin(&[], "r", &counters).is_none());
    }

    #[test]
    fn wrr_single_target_always_returned() {
        let counters = DashMap::new();
        let targets = vec![("http://a:4000".to_string(), 2u32)];
        assert_eq!(
            pick_weighted_round_robin(&targets, "r", &counters),
            Some("http://a:4000".to_string())
        );
    }

    #[test]
    fn wrr_respects_weights_3_to_1() {
        let counters = DashMap::new();
        let targets = vec![
            ("http://a:4000".to_string(), 3u32),
            ("http://b:4000".to_string(), 1u32),
        ];
        // 4 calls: a, a, a, b (cycle length = 4)
        let results: Vec<_> = (0..4)
            .map(|_| pick_weighted_round_robin(&targets, "r", &counters).unwrap())
            .collect();
        let a_count = results
            .iter()
            .filter(|u| u.as_str() == "http://a:4000")
            .count();
        let b_count = results
            .iter()
            .filter(|u| u.as_str() == "http://b:4000")
            .count();
        assert_eq!(a_count, 3, "a should be selected 3 times per cycle");
        assert_eq!(b_count, 1, "b should be selected 1 time per cycle");
    }

    // ── pick_by_hash ──────────────────────────────────────────────────────────

    #[test]
    fn hash_none_on_empty() {
        assert!(pick_by_hash(&[], 42).is_none());
    }

    #[test]
    fn hash_single_target() {
        let urls = vec!["http://a:4000".to_string()];
        assert_eq!(pick_by_hash(&urls, 99), Some("http://a:4000".to_string()));
    }

    #[test]
    fn hash_deterministic_same_input() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        assert_eq!(pick_by_hash(&urls, 5), pick_by_hash(&urls, 5));
    }

    #[test]
    fn hash_distributes_across_buckets() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        // Even hash → a, odd hash → b (with 2 buckets)
        assert_eq!(pick_by_hash(&urls, 0), Some("http://a:4000".to_string()));
        assert_eq!(pick_by_hash(&urls, 1), Some("http://b:4000".to_string()));
    }

    // ── pick_least_response_time ──────────────────────────────────────────────

    #[test]
    fn lrt_falls_back_to_round_robin_without_data() {
        use crate::health::UpstreamRegistry;
        let registry = UpstreamRegistry::new();
        let counters = DashMap::new();
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        // No probe data → round-robin: first call picks index 0, second picks index 1
        let first = pick_least_response_time(&urls, &registry, "r", &counters).unwrap();
        let second = pick_least_response_time(&urls, &registry, "r", &counters).unwrap();
        assert_ne!(
            first, second,
            "without probe data, LRT falls back to round-robin"
        );
    }

    #[test]
    fn lrt_picks_lowest_latency() {
        use crate::health::{UpstreamEntry, UpstreamRegistry};
        let registry = UpstreamRegistry::new();
        let counters = DashMap::new();
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];

        registry.statuses.insert(
            "http://a:4000".to_string(),
            UpstreamEntry {
                latency_ms: Some(100),
                ..Default::default()
            },
        );
        registry.statuses.insert(
            "http://b:4000".to_string(),
            UpstreamEntry {
                latency_ms: Some(20),
                ..Default::default()
            },
        );

        let chosen = pick_least_response_time(&urls, &registry, "r", &counters).unwrap();
        assert_eq!(chosen, "http://b:4000", "b has lower latency");
    }

    // ── fnv1a_hash ────────────────────────────────────────────────────────────

    #[test]
    fn fnv1a_hash_deterministic() {
        assert_eq!(fnv1a_hash("1.2.3.4"), fnv1a_hash("1.2.3.4"));
    }

    #[test]
    fn fnv1a_hash_different_inputs_differ() {
        assert_ne!(fnv1a_hash("1.2.3.4"), fnv1a_hash("1.2.3.5"));
    }

    #[test]
    fn fnv1a_hash_empty_string() {
        // Empty string → offset basis constant, must not panic
        let _ = fnv1a_hash("");
    }

    // ── url_to_host_port edge cases ───────────────────────────────────────────

    #[test]
    fn url_to_host_port_empty_host_returns_none() {
        // URL with scheme but no host — e.g. "http://" with nothing after.
        assert!(url_to_host_port("http://").is_none());
    }

    #[test]
    fn url_to_host_port_ipv6_no_port_adds_default() {
        // IPv6 without port — default port should be added.
        let result = url_to_host_port("http://[::1]/path");
        assert_eq!(result, Some("[::1]:80".to_owned()));
    }

    #[test]
    fn url_to_host_port_ipv6_https_no_port_uses_443() {
        let result = url_to_host_port("https://[::1]/path");
        assert_eq!(result, Some("[::1]:443".to_owned()));
    }

    // ── url_host edge cases ───────────────────────────────────────────────────

    #[test]
    fn url_host_simple_no_port() {
        assert_eq!(url_host("http://example.com"), "example.com");
    }

    #[test]
    fn url_host_with_fragment() {
        assert_eq!(url_host("http://example.com:4000#section"), "example.com");
    }
}
