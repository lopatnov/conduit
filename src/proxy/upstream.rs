//! Upstream target resolution helpers that still need root-crate-only config
//! types, plus a facade re-export of everything `crates/conduit-upstream`
//! owns (issue #114/#142).
//!
//! Most of the original `src/proxy/upstream.rs` — URL parsing helpers and the
//! plain-slice load-balancing "pick" algorithms — moved verbatim into
//! `crates/conduit-upstream::targets`; see that module's re-export below.
//! The four functions in this file stay here because they operate on
//! `ProxyRouteTarget`/`ProxyConfig`/`ProxyTarget`, and `ProxyRouteTarget::Full`
//! embeds `ProxyRouteConfig` — a large struct itself embedding
//! `CacheConfig`/`RetryConfig`/`ConnectionPoolConfig`/`RateLimitConfig`/etc.,
//! none of which belong in (or are extracted into) an upstream-selection
//! crate. Moving these four functions into `conduit-upstream` would have
//! forced `ProxyRouteConfig` to move too — a genuine circular dependency
//! with several other not-yet-extracted crates — so they stayed in the root
//! crate instead. See `crates/conduit-upstream/src/lib.rs`'s own doc comment
//! for the full rationale.

pub use conduit_upstream::targets::*;

use crate::config::schema::{ProxyConfig, ProxyRouteTarget, ProxyTarget};

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

/// Collect `(url, weight)` pairs from a route target.
///
/// Simple URLs and round-robin lists get weight `1`.  Only `Full` targets with
/// `ProxyTarget::Weighted` entries carry explicit weights.
pub fn weighted_targets(route_target: &ProxyRouteTarget) -> Vec<(String, u32)> {
    match route_target {
        ProxyRouteTarget::Url(url) => vec![(url.clone(), 1)],
        ProxyRouteTarget::RoundRobin(urls) => urls.iter().map(|u| (u.clone(), 1)).collect(),
        ProxyRouteTarget::Full(cfg) => cfg
            .targets
            .iter()
            .map(|t| match t {
                ProxyTarget::Simple(url) => (url.clone(), 1),
                ProxyTarget::Weighted(w) => (w.url.clone(), w.weight.max(1)),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::config::schema::ProxyRouteConfig;
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
        use crate::config::schema::{ProxyRouteConfig, WeightedTarget};
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

    // ── weighted_targets ──────────────────────────────────────────────────────

    #[test]
    fn weighted_targets_url_gets_weight_one() {
        let t = ProxyRouteTarget::Url("http://a:4000".to_string());
        assert_eq!(weighted_targets(&t), vec![("http://a:4000".to_string(), 1)]);
    }

    #[test]
    fn weighted_targets_round_robin_all_weight_one() {
        let t = ProxyRouteTarget::RoundRobin(vec![
            "http://a:4000".to_string(),
            "http://b:4000".to_string(),
        ]);
        assert_eq!(
            weighted_targets(&t),
            vec![
                ("http://a:4000".to_string(), 1),
                ("http://b:4000".to_string(), 1),
            ]
        );
    }

    #[test]
    fn weighted_targets_full_preserves_explicit_weights() {
        use crate::config::schema::{ProxyRouteConfig, WeightedTarget};
        let cfg = ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Weighted(WeightedTarget {
                    url: "http://a:4000".to_string(),
                    weight: 3,
                }),
                ProxyTarget::Weighted(WeightedTarget {
                    url: "http://b:4000".to_string(),
                    weight: 1,
                }),
            ],
            ..Default::default()
        };
        assert_eq!(
            weighted_targets(&ProxyRouteTarget::Full(Box::new(cfg))),
            vec![
                ("http://a:4000".to_string(), 3),
                ("http://b:4000".to_string(), 1),
            ]
        );
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
