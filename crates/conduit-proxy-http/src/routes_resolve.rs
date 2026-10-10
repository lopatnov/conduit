//! `routes[]` array proxy-target resolution (issue #143, #412).
//!
//! A matched `routes[]` entry is resolved by the same engine as a `proxy` map
//! route ([`crate::resolve`]): the two mechanisms used to have separate
//! resolvers, and the `routes[]` one silently ignored `groups`, `hashKey`,
//! `sticky`, `backup`, `rewrite`, `mirror` and `upstreamTls` (#412). Sharing
//! the engine means a route option cannot work in one mechanism and be inert
//! in the other.
//!
//! The only things that differ are what a `routes[]` entry does not have: a
//! path-prefix key. It gets a synthetic, site-scoped key for counters and
//! runtime overrides (`<site>#routes[<i>]`), and the prefix `stripPrefix`
//! removes is the literal prefix of its `match.path` glob.

use crate::config::{ProxyRouteTarget, RouteConfig};
use crate::options::ProxyCtx;
use crate::outcome::ProxyResolution;
use crate::resolve;

/// Convert the matched `routes[index]` entry's `proxy` action to a [`ProxyResolution`].
pub(crate) fn resolve_route_target(
    index: usize,
    route: &RouteConfig,
    target: &ProxyRouteTarget,
    ctx: &ProxyCtx<'_>,
) -> ProxyResolution {
    // Site-scoped: the round-robin/least-conn counters are process-wide and keyed by this string,
    // so two sites' `routes[0]` must not share one.
    let route_key = format!("{}#routes[{index}]", ctx.site_label);
    let strip_base = route.r#match.path.as_deref().map(literal_prefix);
    resolve::resolve_target(&route_key, strip_base, target, ctx)
}

/// The part of a path glob that is the same for every request it matches, cut back to a whole
/// segment: `/api/**` -> `/api/`, `/api/us*` -> `/api/`, `/health` -> `/health`.
fn literal_prefix(glob: &str) -> &str {
    let end = glob.find(['*', '?', '[']).unwrap_or(glob.len());
    let literal = &glob[..end];
    if end == glob.len() {
        literal
    } else {
        literal.rfind('/').map_or("", |slash| &literal[..=slash])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use dashmap::DashMap;

    use crate::config::ProxyRouteConfig;
    use crate::outcome::ProxyOutcome;
    use conduit_upstream::health::UpstreamRegistry;
    use conduit_upstream::{LoadBalanceStrategy, ProxyTarget};

    /// Resolve `target` as `routes[0]` of a site matching exactly `path`, for a client at
    /// `203.0.113.1` with no headers. Shim over the production entry point so the
    /// strategy/health/capacity/retry tests below read as they did before the resolver was shared.
    fn resolve_route_target(
        target: &ProxyRouteTarget,
        path: &str,
        counters: &DashMap<String, AtomicUsize>,
        registry: &UpstreamRegistry,
    ) -> ProxyResolution {
        resolve_for_client(target, path, "203.0.113.1", counters, registry)
    }

    fn resolve_for_client(
        target: &ProxyRouteTarget,
        path: &str,
        client_ip: &str,
        counters: &DashMap<String, AtomicUsize>,
        registry: &UpstreamRegistry,
    ) -> ProxyResolution {
        let headers = http::HeaderMap::new();
        let ctx = ProxyCtx {
            path,
            client_ip,
            req_headers: &headers,
            counters,
            upstream_health: registry,
            site_label: "test:80",
        };
        let route = RouteConfig {
            r#match: crate::config::MatchConfig {
                path: Some(path.to_owned()),
                ..Default::default()
            },
            proxy: Some(target.clone()),
            ..Default::default()
        };
        resolve_route_target_for(0, &route, target, &ctx)
    }

    use super::resolve_route_target as resolve_route_target_for;

    fn upstream_addr(resolution: &ProxyResolution) -> String {
        match &resolution.outcome {
            ProxyOutcome::Upstream(pu) => pu.addr.clone(),
            other => panic!("expected Upstream outcome, got {other:?}"),
        }
    }

    // ── resolve_route_target (formerly `route_to_result`) ─────────────────────
    //
    // Static-action handling (formerly also part of `route_to_result`) moved
    // to `router.rs::resolve_non_proxy_route` (issue #143) — its coverage
    // moved with it, see `router.rs`'s own test module
    // (`resolve_non_proxy_route_serves_static_file`/`..._no_static_falls_back`).

    #[test]
    fn resolve_route_target_url_proxy() {
        let target = ProxyRouteTarget::Url("http://backend:4000".to_string());
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_invalid_url_gives_unresolved() {
        // "http://" has an empty host segment — url_to_host_port returns None.
        let target = ProxyRouteTarget::Url("http://".to_string());
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Unresolved));
    }

    #[test]
    fn resolve_route_target_round_robin_proxy() {
        let target = ProxyRouteTarget::RoundRobin(vec![
            "http://b1:4000".to_string(),
            "http://b2:4001".to_string(),
        ]);
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_round_robin_rotates() {
        let target = ProxyRouteTarget::RoundRobin(vec![
            "http://b1:4000".to_string(),
            "http://b2:4001".to_string(),
        ]);
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();

        let r1 = resolve_route_target(&target, "/api", &counters, &registry);
        let r2 = resolve_route_target(&target, "/api", &counters, &registry);

        // Round-robin must select different upstreams on consecutive calls.
        let a1 = upstream_addr(&r1);
        let a2 = upstream_addr(&r2);
        assert_ne!(a1, a2, "round-robin must rotate across upstreams");
    }

    #[test]
    fn resolve_route_target_full_proxy_round_robin() {
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Simple("http://b1:4000".to_string()),
                ProxyTarget::Simple("http://b2:4001".to_string()),
            ],
            strategy: Some(LoadBalanceStrategy::RoundRobin),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_full_proxy_random_strategy() {
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
            strategy: Some(LoadBalanceStrategy::Random),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_full_proxy_least_conn_strategy() {
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
            strategy: Some(LoadBalanceStrategy::LeastConn),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_full_proxy_ip_hash_strategy() {
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
            strategy: Some(LoadBalanceStrategy::IpHash),
            hash_key: Some("ip".to_string()),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api/users", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_full_proxy_weighted_round_robin() {
        use conduit_upstream::WeightedTarget;
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
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
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_full_proxy_consistent_hash() {
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
            strategy: Some(LoadBalanceStrategy::ConsistentHash),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api/items/42", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_full_proxy_all_unhealthy_fails_open() {
        use conduit_upstream::health::UpstreamEntry;

        let registry = UpstreamRegistry::new();
        // Mark the only upstream as unhealthy.
        registry.statuses.insert(
            "http://b1:4000".to_string(),
            UpstreamEntry {
                healthy: false,
                ..Default::default()
            },
        );

        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://b1:4000".to_string())],
            strategy: Some(LoadBalanceStrategy::RoundRobin),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        // Fail-open: when all upstreams are unhealthy the router falls back to
        // the full (unhealthy) pool rather than refusing traffic.
        assert!(
            matches!(resolution.outcome, ProxyOutcome::Upstream(_)),
            "fail-open must still pick from the full upstream pool, got {:?}",
            resolution.outcome
        );
    }

    #[test]
    fn resolve_route_target_full_proxy_with_strip_prefix_and_retry() {
        use crate::config::RetryConfig;
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
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
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api/users", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
        assert!(
            resolution.state.retry.is_some(),
            "expected retry state to be populated"
        );
    }

    /// Regression test for #217: `retry.urls` must come from the
    /// health-filtered candidate list, not the raw config target list — a
    /// retry must never be able to rotate into a peer already known
    /// unhealthy. Reverting the fix (`urls: all_urls.clone()`) would make
    /// this test fail, since `all_urls` still contains the unhealthy target.
    #[test]
    fn resolve_route_target_retry_urls_exclude_unhealthy_targets() {
        use crate::config::RetryConfig;
        use conduit_upstream::health::UpstreamEntry;

        let registry = UpstreamRegistry::new();
        // Mark b2 unhealthy; b1 stays healthy (default).
        registry.statuses.insert(
            "http://b2:4000".to_string(),
            UpstreamEntry {
                healthy: false,
                ..Default::default()
            },
        );

        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Simple("http://b1:4000".to_string()),
                ProxyTarget::Simple("http://b2:4000".to_string()),
            ],
            strategy: Some(LoadBalanceStrategy::RoundRobin),
            retry: Some(RetryConfig {
                attempts: 2,
                conditions: vec!["connection_error".to_string()],
                backoff_ms: Some(50),
                backoff_jitter: None,
                budget_percent: None,
            }),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let resolution = resolve_route_target(&target, "/api/users", &counters, &registry);
        let retry = resolution
            .state
            .retry
            .expect("retry state must be populated");
        assert_eq!(
            retry.urls,
            vec!["http://b1:4000".to_string()],
            "retry candidate list must exclude the unhealthy target"
        );
    }

    /// Regression test for #367: `retry.urls[0]` must always equal the peer
    /// actually chosen by the route's strategy (`proxy_upstream_url`), not
    /// just the head of the unrotated candidate list. Drives round-robin
    /// selection across several requests on a route with two healthy
    /// targets and `retry` configured — reverting the anchoring fix (using
    /// the unrotated `ramp.filter_candidates(...)` list directly) would make
    /// every request's `retry.urls[0]` come back as `http://b1:4000`
    /// regardless of which peer round-robin actually picked for that
    /// request, since `chosen_url` alternates but the candidate list never
    /// rotates on its own.
    #[test]
    fn resolve_route_target_retry_urls_anchored_to_chosen_peer() {
        use crate::config::RetryConfig;

        let registry = UpstreamRegistry::new();
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Simple("http://b1:4000".to_string()),
                ProxyTarget::Simple("http://b2:4000".to_string()),
            ],
            strategy: Some(LoadBalanceStrategy::RoundRobin),
            retry: Some(RetryConfig {
                attempts: 2,
                conditions: vec!["connection_error".to_string()],
                backoff_ms: Some(50),
                backoff_jitter: None,
                budget_percent: None,
            }),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();

        let mut saw_b1_first = false;
        let mut saw_b2_first = false;
        for _ in 0..4 {
            let resolution = resolve_route_target(&target, "/api/users", &counters, &registry);
            let chosen = resolution
                .state
                .proxy_upstream_url
                .clone()
                .expect("proxy_upstream_url must be populated");
            let retry = resolution
                .state
                .retry
                .expect("retry state must be populated");
            assert_eq!(
                retry.urls.first(),
                Some(&chosen),
                "retry.urls[0] must equal the peer round-robin actually chose"
            );
            match chosen.as_str() {
                "http://b1:4000" => saw_b1_first = true,
                "http://b2:4000" => saw_b2_first = true,
                other => panic!("unexpected chosen upstream: {other}"),
            }
        }
        // Round-robin must have actually alternated across 4 requests over
        // 2 peers — otherwise this test would trivially pass even with the
        // pre-fix bug (both would be b1 every time).
        assert!(
            saw_b1_first && saw_b2_first,
            "expected round-robin to pick both peers across 4 requests"
        );
    }

    #[test]
    fn resolve_route_target_full_proxy_least_response_time() {
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Simple("http://b1:4000".to_string()),
                ProxyTarget::Simple("http://b2:4001".to_string()),
            ],
            strategy: Some(LoadBalanceStrategy::LeastResponseTime),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Upstream(_)));
    }

    #[test]
    fn resolve_route_target_round_robin_invalid_url_gives_unresolved() {
        // "http://" has an empty host — url_to_proxy_upstream returns None → Unresolved.
        let target = ProxyRouteTarget::RoundRobin(vec!["http://".to_string()]);
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Unresolved));
    }

    #[test]
    fn resolve_route_target_full_proxy_empty_targets_gives_unresolved() {
        // Empty targets list → filter_healthy returns empty → Unresolved.
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![],
            strategy: Some(LoadBalanceStrategy::RoundRobin),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Unresolved));
    }

    #[test]
    fn resolve_route_target_full_proxy_least_conn_invalid_url_gives_unresolved() {
        // LeastConn + invalid URL (bare "http://"): the URL cannot be resolved
        // to a host:port, so resolve_route_target returns Unresolved.
        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://".to_string())],
            strategy: Some(LoadBalanceStrategy::LeastConn),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Unresolved));
        // Verify the connection counter was not left incremented.
        // (LeastConn increments the counter before picking the peer; on parse
        // failure it must decrement before returning Unresolved.)
        let conn_count = counters
            .get("http://")
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0);
        assert_eq!(
            conn_count, 0,
            "conn counter must be zero after invalid-URL fallback"
        );
    }

    // ── circuit breaker capacity enforcement (#156) ───────────────────────────

    #[test]
    fn resolve_route_target_full_proxy_capacity_exhausted_gives_overloaded() {
        // Before #156, this config path had zero circuit-breaker code at all —
        // maxConnectionsPerUpstream was silently ignored. Also asserts the
        // 503 shape: capacity exhaustion must NOT reuse the Unresolved outcome.
        use conduit_upstream::UpstreamHealthCheck;

        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://only:4000".to_string())],
            health_check: Some(UpstreamHealthCheck {
                max_connections_per_upstream: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        registry.conn_inc("http://only:4000"); // saturate the only target

        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(
            matches!(resolution.outcome, ProxyOutcome::Overloaded),
            "capacity-exhausted routes[] route must return Overloaded (503), got {:?}",
            resolution.outcome
        );
    }

    #[test]
    fn resolve_route_target_full_proxy_circuit_tracking_sets_upstream_conn_slot() {
        // RoundRobin (not least-conn) + a configured cap: circuit_tracking
        // must acquire a conn_count slot so the capacity check has real data
        // to filter on for subsequent requests.
        use conduit_upstream::UpstreamHealthCheck;

        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://only:4000".to_string())],
            health_check: Some(UpstreamHealthCheck {
                max_connections_per_upstream: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();

        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(
            resolution.state.upstream_conn_slot,
            "round-robin with maxConnectionsPerUpstream set must acquire a conn_count slot"
        );
        assert_eq!(
            registry.conn_load("http://only:4000"),
            1,
            "circuit_tracking must have called conn_inc"
        );
    }

    #[test]
    fn resolve_route_target_full_proxy_malformed_url_with_capacity_configured_leaks_no_slot() {
        // LeastConn + an invalid URL + a cap also configured: the malformed-URL
        // release path must still fire, and circuit_tracking's conn_inc must
        // never have run (it happens only after a successful URL parse), so
        // conn_load ends at exactly zero either way.
        use conduit_upstream::UpstreamHealthCheck;

        let target = ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://".to_string())],
            strategy: Some(LoadBalanceStrategy::LeastConn),
            health_check: Some(UpstreamHealthCheck {
                max_connections_per_upstream: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        }));
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();

        let resolution = resolve_route_target(&target, "/api", &counters, &registry);
        assert!(matches!(resolution.outcome, ProxyOutcome::Unresolved));
        assert_eq!(
            registry.conn_load("http://"),
            0,
            "malformed-URL release must leave no leaked slot even with a cap configured"
        );
    }

    // ── #436: slow-start ramp vs. the retry list on the routes[] path ────────
    //
    // Two peers: "a" has just recovered (mid-ramp, fraction 0.0 over a 1h
    // window) and "b" is fully ramped -- so the ramp filter can never fail
    // open, and exactly "a" goes missing from a ramp-filtered list.

    const RAMP_A: &str = "http://a:4000";
    const RAMP_B: &str = "http://b:4000";

    fn ramp_retry_target(strategy: LoadBalanceStrategy) -> ProxyRouteTarget {
        use crate::config::RetryConfig;
        use conduit_upstream::UpstreamHealthCheck;

        ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Simple(RAMP_A.to_string()),
                ProxyTarget::Simple(RAMP_B.to_string()),
            ],
            strategy: Some(strategy),
            // Hash the path: `path_hashing_to_b` picks a route path for the primary peer.
            hash_key: Some("url".to_owned()),
            health_check: Some(UpstreamHealthCheck {
                slow_start_secs: Some(3600),
                ..Default::default()
            }),
            retry: Some(RetryConfig {
                attempts: 3,
                conditions: vec!["5xx".to_string()],
                backoff_ms: None,
                backoff_jitter: None,
                budget_percent: None,
            }),
            ..Default::default()
        }))
    }

    fn registry_with_a_mid_ramp() -> UpstreamRegistry {
        let registry = UpstreamRegistry::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        registry
            .statuses
            .entry(RAMP_A.to_string())
            .or_default()
            .recovery_time_secs = Some(now);
        registry
    }

    /// A route path whose hash lands on index 1 ("b") of the two-peer ring.
    /// Deliberate: `build_retry_state` re-inserts `chosen_url` at the front of
    /// the retry list whenever the ramp-filtered candidates lack it, so if the
    /// primary pick were "a" itself, "a" would appear in `retry.urls`
    /// regardless of whether the exemption ran. With "b" as the primary pick,
    /// "a" can only be there because the exemption kept it.
    fn path_hashing_to_b() -> String {
        (0..256)
            .map(|i| format!("/p{i}"))
            .find(|p| conduit_upstream::targets::fnv1a_hash(p) % 2 == 1)
            .expect("some path in 0..256 hashes to index 1 of a 2-peer ring")
    }

    #[test]
    fn hash_strategy_retry_list_keeps_a_mid_ramp_peer() {
        // Before #436 the routes[] retry list was ramp-filtered
        // unconditionally: a mid-ramp peer was fully eligible for the
        // *primary* pick under a hash strategy (pick_bounded exempts it) but
        // was silently dropped from retry attempts 1+ -- the exact
        // inconsistency #375 closed for the `proxy`-map path.
        let path = path_hashing_to_b();
        for strategy in [
            LoadBalanceStrategy::IpHash,
            LoadBalanceStrategy::ConsistentHash,
        ] {
            let target = ramp_retry_target(strategy.clone());
            let counters: DashMap<String, AtomicUsize> = DashMap::new();
            let registry = registry_with_a_mid_ramp();

            let resolution = resolve_route_target(&target, &path, &counters, &registry);
            let retry = resolution
                .state
                .retry
                .unwrap_or_else(|| panic!("{strategy:?}: retry must be configured"));
            assert_eq!(
                retry.urls.first().map(String::as_str),
                Some(RAMP_B),
                "{strategy:?}: sanity check -- the primary pick must be \"b\", not \
                 \"a\", or this test cannot discriminate the bug it guards"
            );
            assert_eq!(
                retry.urls,
                vec![RAMP_B.to_string(), RAMP_A.to_string()],
                "{strategy:?}: the mid-ramp peer must stay in the retry list"
            );
        }
    }

    #[test]
    fn non_hash_strategy_retry_list_still_drops_a_mid_ramp_peer() {
        // Control for the test above: the exemption must be exactly "hash
        // strategies", not "any route". Round-robin keeps ramp-filtering the
        // retry list (#157), so an over-broad fix that stopped filtering
        // everywhere would pass the hash test but fail here.
        let target = ramp_retry_target(LoadBalanceStrategy::RoundRobin);
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = registry_with_a_mid_ramp();

        let resolution = resolve_route_target(&target, "/anything", &counters, &registry);
        let retry = resolution
            .state
            .retry
            .expect("retry must be configured for this route");
        assert_eq!(
            retry.urls,
            vec![RAMP_B.to_string()],
            "a ramp-filtered retry list must exclude the mid-ramp peer"
        );
    }

    // ── parity with the `proxy` map (issue #412) ──────────────────────────────

    fn full(cfg: ProxyRouteConfig) -> ProxyRouteTarget {
        ProxyRouteTarget::Full(Box::new(cfg))
    }

    fn simple(urls: &[&str]) -> Vec<ProxyTarget> {
        urls.iter()
            .map(|u| ProxyTarget::Simple((*u).to_owned()))
            .collect()
    }

    fn resolve_with_headers(
        target: &ProxyRouteTarget,
        path: &str,
        client_ip: &str,
        headers: &http::HeaderMap,
        registry: &UpstreamRegistry,
    ) -> ProxyResolution {
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let ctx = ProxyCtx {
            path,
            client_ip,
            req_headers: headers,
            counters: &counters,
            upstream_health: registry,
            site_label: "test:80",
        };
        let route = RouteConfig {
            r#match: crate::config::MatchConfig {
                path: Some(path.to_owned()),
                ..Default::default()
            },
            proxy: Some(target.clone()),
            ..Default::default()
        };
        super::resolve_route_target(0, &route, target, &ctx)
    }

    #[test]
    fn groups_only_route_resolves_to_a_group_member() {
        // Before #412 a groups-only `routes[]` entry had an empty target list and resolved to
        // nothing, so the route silently did not work at all.
        let target = full(ProxyRouteConfig {
            groups: Some(vec![conduit_upstream::UpstreamGroup {
                name: "blue".to_owned(),
                targets: simple(&["http://blue1:4000"]),
                strategy: None,
            }]),
            ..Default::default()
        });
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &UpstreamRegistry::new());
        assert_eq!(upstream_addr(&resolution), "blue1:4000");
    }

    #[test]
    fn hash_key_header_pins_the_client_regardless_of_ip() {
        let target = full(ProxyRouteConfig {
            targets: simple(&["http://a:4000", "http://b:4000", "http://c:4000"]),
            strategy: Some(LoadBalanceStrategy::IpHash),
            hash_key: Some("header:x-tenant".to_owned()),
            ..Default::default()
        });
        let registry = UpstreamRegistry::new();
        let mut headers = http::HeaderMap::new();
        headers.insert("x-tenant", "acme".parse().unwrap());
        let picks: std::collections::HashSet<String> = (1..=40)
            .map(|i| {
                let ip = format!("198.51.100.{i}");
                upstream_addr(&resolve_with_headers(
                    &target, "/api", &ip, &headers, &registry,
                ))
            })
            .collect();
        assert_eq!(
            picks.len(),
            1,
            "the header, not the client IP, is the hash input: {picks:?}"
        );
    }

    #[test]
    fn default_hash_key_is_the_client_ip_not_the_path() {
        // Control for the test above, and the old behaviour this replaces: `routes[]` used to hash
        // the request path, so every client asking for one path landed on one peer.
        let target = full(ProxyRouteConfig {
            targets: simple(&["http://a:4000", "http://b:4000", "http://c:4000"]),
            strategy: Some(LoadBalanceStrategy::IpHash),
            ..Default::default()
        });
        let registry = UpstreamRegistry::new();
        let headers = http::HeaderMap::new();
        let picks: std::collections::HashSet<String> = (1..=40)
            .map(|i| {
                let ip = format!("198.51.100.{i}");
                upstream_addr(&resolve_with_headers(
                    &target, "/api", &ip, &headers, &registry,
                ))
            })
            .collect();
        assert!(
            picks.len() > 1,
            "different client IPs must spread over the peers: {picks:?}"
        );
    }

    #[test]
    fn backup_takes_over_when_every_primary_is_unhealthy() {
        let target = full(ProxyRouteConfig {
            targets: simple(&["http://primary:4000"]),
            backup: Some("http://backup:4000".to_owned()),
            ..Default::default()
        });
        let registry = UpstreamRegistry::new();
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        assert_eq!(
            upstream_addr(&resolve_route_target(&target, "/api", &counters, &registry)),
            "primary:4000"
        );
        registry
            .statuses
            .entry("http://primary:4000".to_owned())
            .or_default()
            .healthy = false;
        assert_eq!(
            upstream_addr(&resolve_route_target(&target, "/api", &counters, &registry)),
            "backup:4000"
        );
    }

    #[test]
    fn rewrite_mirror_and_upstream_tls_reach_the_upstream() {
        let target = full(ProxyRouteConfig {
            targets: simple(&["https://secure:4443"]),
            rewrite: Some(vec![crate::config::RewriteRule {
                from: "^/v1/(.*)".to_owned(),
                to: "/$1".to_owned(),
            }]),
            mirror: Some("http://shadow:4000".to_owned()),
            upstream_tls: Some(conduit_upstream::UpstreamTlsConfig {
                verify: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        });
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let resolution =
            resolve_route_target(&target, "/v1/x", &counters, &UpstreamRegistry::new());
        let ProxyOutcome::Upstream(pu) = resolution.outcome else {
            panic!("expected an upstream");
        };
        assert_eq!(pu.rewrite.as_ref().map(Vec::len), Some(1));
        assert_eq!(pu.mirror_url.as_deref(), Some("http://shadow:4000"));
        assert_eq!(pu.upstream_tls.and_then(|t| t.verify), Some(false));
    }

    #[test]
    fn sticky_route_sets_a_session_cookie() {
        let target = full(ProxyRouteConfig {
            targets: simple(&["http://a:4000", "http://b:4000"]),
            sticky: Some(crate::config::StickyConfig {
                cookie: "srv".to_owned(),
                secret: Some("s3cret".to_owned()),
                strict: None,
            }),
            ..Default::default()
        });
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let resolution = resolve_route_target(&target, "/api", &counters, &UpstreamRegistry::new());
        let (name, _value) = resolution
            .state
            .sticky_set_cookie
            .expect("a sticky route must hand out its cookie");
        assert_eq!(name, "srv");
    }

    #[test]
    fn strip_prefix_removes_the_literal_prefix_of_the_match_glob() {
        // Before #412 the *whole request path* was stripped, so `/api/users` reached the upstream as `/`.
        let target = full(ProxyRouteConfig {
            targets: simple(&["http://a:4000"]),
            strip_prefix: Some(true),
            ..Default::default()
        });
        let headers = http::HeaderMap::new();
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let ctx = ProxyCtx {
            path: "/api/users",
            client_ip: "203.0.113.1",
            req_headers: &headers,
            counters: &counters,
            upstream_health: &registry,
            site_label: "test:80",
        };
        let route = RouteConfig {
            r#match: crate::config::MatchConfig {
                path: Some("/api/**".to_owned()),
                ..Default::default()
            },
            proxy: Some(target.clone()),
            ..Default::default()
        };
        let ProxyOutcome::Upstream(pu) =
            super::resolve_route_target(0, &route, &target, &ctx).outcome
        else {
            panic!("expected an upstream");
        };
        assert_eq!(pu.strip_prefix.as_deref(), Some("/api"));
    }

    #[test]
    fn counters_are_scoped_to_the_site_and_the_entry() {
        // Two sites (or two entries) must not share one round-robin position.
        let target = full(ProxyRouteConfig {
            targets: simple(&["http://a:4000", "http://b:4000"]),
            ..Default::default()
        });
        let counters: DashMap<String, AtomicUsize> = DashMap::new();
        let registry = UpstreamRegistry::new();
        let headers = http::HeaderMap::new();
        let route = RouteConfig::default();
        for site in ["one:80", "two:80"] {
            let ctx = ProxyCtx {
                path: "/x",
                client_ip: "203.0.113.1",
                req_headers: &headers,
                counters: &counters,
                upstream_health: &registry,
                site_label: site,
            };
            // First pick of each site starts at the first target, not wherever the other site left off.
            assert_eq!(
                upstream_addr(&super::resolve_route_target(0, &route, &target, &ctx)),
                "a:4000",
                "{site} must start its own rotation"
            );
        }
        assert!(counters
            .iter()
            .any(|e| e.key().starts_with("one:80#routes[0]")));
        let _ = Ordering::Relaxed;
    }

    #[test]
    fn literal_prefix_cuts_the_glob_back_to_a_whole_segment() {
        assert_eq!(literal_prefix("/api/**"), "/api/");
        assert_eq!(literal_prefix("/api/us*"), "/api/");
        assert_eq!(literal_prefix("/health"), "/health");
        assert_eq!(literal_prefix("/**"), "/");
        assert_eq!(literal_prefix("*"), "");
    }
}
