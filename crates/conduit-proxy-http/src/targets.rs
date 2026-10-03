//! Upstream-target-list helpers that need [`crate::config::ProxyRouteTarget`]/
//! [`crate::config::ProxyConfig`] (issue #114/#143) — the three functions here
//! stayed behind in the root crate's `src/proxy/upstream.rs` since #142
//! specifically because moving them required `ProxyRouteTarget`/
//! `ProxyConfig` to move too (see `crates/conduit-upstream/src/lib.rs`'s own
//! doc comment for the "not extracted yet" note this PR makes stale). Now
//! that this crate owns those config types, the functions travel with them.
//!
//! `strip_prefix_enabled` (the fourth function `conduit-upstream`'s doc
//! comment named) did **not** move here — it had zero real production call
//! sites (confirmed via grep across `src/`/`crates/` before this PR: only
//! its own unit tests and two doc-comment mentions referenced it), so it was
//! deleted outright along with its tests rather than moved. See this PR's
//! own description for the full reasoning.

use crate::config::{ProxyConfig, ProxyRouteTarget};
use conduit_upstream::ProxyTarget;

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
        use crate::config::ProxyRouteConfig;
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
        use crate::config::ProxyRouteConfig;
        use conduit_upstream::WeightedTarget;
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
        use crate::config::ProxyRouteConfig;
        use conduit_upstream::WeightedTarget;
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
