//! `routes[]` array proxy-target resolution (issue #143, PR A2 of a 3-PR
//! plan) — split out of `routes.rs`'s former `full_cfg_to_result`
//! (~186 lines) the same way `routing::resolve` splits router.rs's
//! equivalent function, but WITHOUT sharing helpers with it: this path has
//! no sticky/backup/groups support, and uses the request path (not client
//! IP) as its hash input since client IP isn't threaded through route-array
//! matching — a real semantic difference from the `proxy`-map path, not an
//! oversight to unify (see `resolve_full_target`'s own hash-input comment).

use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;

use crate::config::schema::{ProxyRouteConfig, ProxyRouteTarget, ProxyTarget};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::routing::outcome::{self, ProxyResolution, ProxyUpstream};
use crate::proxy::routing::state::{ProxyReqState, RetryState};
use crate::proxy::slow_start::Ramp;
use crate::proxy::{capacity, router, upstream};

/// Convert a matched [`ProxyRouteTarget`] to a [`ProxyResolution`].
pub(crate) fn resolve_route_target(
    target: &ProxyRouteTarget,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> ProxyResolution {
    match target {
        ProxyRouteTarget::Url(url) => resolve_url_target(url),
        ProxyRouteTarget::RoundRobin(urls) => resolve_round_robin_target(urls, counters),
        ProxyRouteTarget::Full(cfg) => resolve_full_target(cfg, path, counters, upstream_health),
    }
}

/// Convert a single-URL `ProxyRouteTarget::Url` to a [`ProxyResolution`].
fn resolve_url_target(url: &str) -> ProxyResolution {
    match router::url_to_proxy_upstream(url, None)
        .and_then(outcome::upstream_target_into_proxy_upstream)
    {
        Some(upstream) => ProxyResolution::upstream(upstream, ProxyReqState::default()),
        None => ProxyResolution::unresolved(ProxyReqState::default()),
    }
}

/// Rotate through `urls` round-robin and convert the chosen URL to a [`ProxyResolution`].
fn resolve_round_robin_target(
    urls: &[String],
    counters: &DashMap<String, AtomicUsize>,
) -> ProxyResolution {
    let key = urls.join(",");
    let counter = counters.entry(key).or_insert_with(|| AtomicUsize::new(0));
    let idx = counter.fetch_add(1, Ordering::Relaxed) % urls.len();
    match router::url_to_proxy_upstream(&urls[idx], None)
        .and_then(outcome::upstream_target_into_proxy_upstream)
    {
        Some(upstream) => ProxyResolution::upstream(upstream, ProxyReqState::default()),
        None => ProxyResolution::unresolved(ProxyReqState::default()),
    }
}

/// Handle the `Full` form of a proxy route target (strategy selection, health
/// filtering, capacity, retry). No sticky/backup/groups support — see this
/// module's own doc comment for why this doesn't share phases with
/// `routing::resolve`'s equivalent.
fn resolve_full_target(
    cfg: &ProxyRouteConfig,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> ProxyResolution {
    let all_urls: Vec<String> = cfg
        .targets
        .iter()
        .map(|t| match t {
            ProxyTarget::Simple(u) => u.clone(),
            ProxyTarget::Weighted(w) => w.url.clone(),
        })
        .collect();
    let all_weighted: Vec<(String, u32)> = cfg
        .targets
        .iter()
        .map(|t| match t {
            ProxyTarget::Simple(u) => (u.clone(), 1),
            ProxyTarget::Weighted(w) => (w.url.clone(), w.weight),
        })
        .collect();

    // Filter to healthy upstreams; fail-open when all are down.
    let (healthy, _fail_open) = upstream_health.filter_healthy(&all_urls);
    let urls: Vec<String> = healthy.iter().cloned().cloned().collect();

    if urls.is_empty() {
        return ProxyResolution::unresolved(ProxyReqState::default());
    }

    // Circuit breaker: per-upstream capacity filtering (#156), shared with
    // the legacy `proxy` map and `groups` paths via `capacity::pick_bounded`.
    let max_conns = cfg
        .health_check
        .as_ref()
        .and_then(|h| h.max_connections_per_upstream);
    let slow_start_secs = cfg.health_check.as_ref().and_then(|h| h.slow_start_secs);
    let route_key = path; // stable key for round-robin counters
    let capacity = capacity::Capacity::evaluate(&urls, max_conns, route_key, upstream_health);
    if matches!(capacity, capacity::Capacity::Exhausted) {
        // Distinct from `ProxyOutcome::Unresolved`: capacity exhaustion is a
        // 503, not "no route matched" — matches the legacy `proxy` map path.
        return ProxyResolution::overloaded(ProxyReqState::default());
    }

    let weighted: Vec<(String, u32)> = all_weighted
        .iter()
        .filter(|(u, _)| urls.contains(u))
        .cloned()
        .collect();

    // Use path as the hash input since client IP is not available at
    // route-match time (the routes array doesn't carry it through).
    let hash_val = upstream::fnv1a_hash(path);
    let strategy = cfg.strategy.as_ref();

    // Slow start (#157): ramp traffic to a recently-recovered upstream.
    // `Ramp::new` is a true no-op when `slowStartSecs` is unset; hash-based
    // strategies are exempt for free via `pick_bounded`'s own early return.
    let ramp = Ramp::new(slow_start_secs, upstream_health);

    let input = capacity::BoundedPick {
        strategy,
        healthy: &urls,
        capacity: &capacity,
        weighted: &weighted,
        route_key,
        hash_val,
        counters,
        health: upstream_health,
        ramp: &ramp,
    };
    let Some((chosen_url, is_least_conn)) = capacity::pick_bounded(&input) else {
        return ProxyResolution::unresolved(ProxyReqState::default());
    };

    let strip = cfg.strip_prefix.unwrap_or(false).then(|| path.to_string());

    // url_to_proxy_upstream may return None for a malformed URL. Parse
    // BEFORE acquiring the circuit_tracking slot below (matches the
    // ordering invariant in router.rs's resolve_proxy_routes) so a
    // malformed-URL request can never leak a slot nothing will release.
    let Some(upstream) = build_target_upstream(&chosen_url, strip, is_least_conn, upstream_health)
    else {
        return ProxyResolution::unresolved(ProxyReqState::default());
    };

    // Same accounting shape as the legacy `proxy` map path (#155/#156): a
    // slot is only acquired when least-conn didn't already track it but a
    // cap is configured, so the capacity check above stays fed for every
    // strategy.
    let circuit_tracking = max_conns.is_some() && !is_least_conn;
    if circuit_tracking {
        upstream_health.conn_inc(&chosen_url);
    }

    let retry = build_retry_state(
        cfg,
        &capacity,
        &ramp,
        &urls,
        &chosen_url,
        is_least_conn,
        circuit_tracking,
        max_conns,
    );

    // proxy_upstream_url is populated unconditionally (#155) so passive-health
    // attribution works for every strategy; upstream_conn_slot tracks whether
    // this request actually holds a conn_count slot to release.
    let proxy_upstream_url = Some(chosen_url.clone());

    ProxyResolution::upstream(
        upstream,
        ProxyReqState {
            retry,
            proxy_timeout: cfg.timeout.clone(),
            proxy_pool: cfg.pool.clone(),
            proxy_http2: cfg.http2.unwrap_or(false),
            proxy_upstream_url,
            upstream_conn_slot: is_least_conn || circuit_tracking,
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
            ..Default::default()
        },
    )
}

/// Parse `chosen_url` into a [`ProxyUpstream`], releasing the least-conn
/// inflight slot if it's malformed — `logging()` will not run for this
/// request. No rewrite/mirror/upstream-TLS overlay here: unlike
/// `routing::resolve::build_proxy_upstream`, `routes[]` targets never carry
/// those fields (`ProxyRouteConfig` has no `rewrite`/`mirror`/`upstreamTls`
/// handling on this path today — preserved exactly, not a gap this PR fixes).
fn build_target_upstream(
    chosen_url: &str,
    strip: Option<String>,
    is_least_conn: bool,
    upstream_health: &UpstreamRegistry,
) -> Option<ProxyUpstream> {
    match router::url_to_proxy_upstream(chosen_url, strip) {
        Some(target) => outcome::upstream_target_into_proxy_upstream(target),
        None => {
            if is_least_conn {
                upstream_health.conn_dec(chosen_url);
            }
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_retry_state(
    cfg: &ProxyRouteConfig,
    capacity: &capacity::Capacity,
    ramp: &Ramp<'_>,
    urls: &[String],
    chosen_url: &str,
    is_least_conn: bool,
    circuit_tracking: bool,
    max_conns: Option<u64>,
) -> Option<RetryState> {
    // Fixed (#217): retry.urls now comes from the same health/capacity-
    // filtered candidate list used to pick chosen_url above, matching
    // router.rs's resolve_proxy_routes — a retry can no longer rotate into a
    // peer already known unhealthy or at its connection cap. `capacity` was
    // evaluated against `urls` (the health-filtered list) by the caller, and
    // the `Exhausted` case already returned before this point, so
    // `candidates()` is always `Some` here; `unwrap_or(&urls)` is just a
    // defensive fallback, not an expected path.
    cfg.retry.as_ref().map(|r| {
        // Slow start (#157): this retry list is its own routing decision that
        // never goes through `pick_bounded` -- without wrapping it here, a
        // route with `retry` configured would keep ignoring `slowStartSecs`
        // for its fallback rotation, mirroring the same gap fixed in
        // router.rs's `resolve_proxy_routes` retry-bypass branch.
        let mut retry_urls: Vec<String> = ramp
            .filter_candidates(capacity.candidates(urls).unwrap_or(urls))
            .into_owned();
        // Anchor attempt 1 to the peer actually chosen above (#367) — this
        // list used to be the unrotated candidate list, so attempt 1 always
        // connected to `retry_urls[0]` regardless of which peer `chosen_url`
        // (round-robin/least-conn/etc.) actually was, defeating round-robin
        // and misattributing conn_count/EWMA/access-log stats to the wrong
        // peer. Mirrors router.rs's `pick_with_retry`, which builds
        // `chosen_url` and `retry.urls` from the same rotation so the
        // invariant holds by construction; here the two are picked
        // separately (via `pick_bounded` vs. this list's own ramp/capacity
        // filter), so restore it explicitly instead. `chosen_url` can be
        // absent from this list for a hash-based strategy — exempt from
        // ramp filtering during its own pick_bounded pick, but not from this
        // separate list's filter (see #366's analogous gap in router.rs) —
        // in that case prepend it rather than leaving the invariant broken.
        match retry_urls.iter().position(|u| u == chosen_url) {
            Some(pos) => retry_urls.rotate_left(pos),
            None => retry_urls.insert(0, chosen_url.to_owned()),
        }
        RetryState {
            urls: retry_urls,
            attempt: 0,
            max_attempts: r.attempts as usize,
            conditions: r.conditions.clone(),
            backoff_ms: r.backoff_ms,
            backoff_jitter: r.backoff_jitter.unwrap_or(false),
            budget_percent: r.budget_percent,
            is_retrying: false,
            // #216 part 2: mirrors upstream_conn_slot's own formula a few
            // lines below (is_least_conn || circuit_tracking) -- unlike
            // router.rs's retry-bypass branch, this path goes through
            // pick_bounded, so is_least_conn can genuinely be true here.
            max_conns_per_upstream: max_conns,
            tracks_conn_slot: is_least_conn || circuit_tracking,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::LoadBalanceStrategy;
    use crate::proxy::routing::outcome::ProxyOutcome;

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
        use crate::config::schema::WeightedTarget;
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
        use crate::proxy::health::UpstreamEntry;

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
        use crate::config::schema::RetryConfig;
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
        use crate::config::schema::RetryConfig;
        use crate::proxy::health::UpstreamEntry;

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
        use crate::config::schema::RetryConfig;

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
        use crate::config::schema::UpstreamHealthCheck;

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
        use crate::config::schema::UpstreamHealthCheck;

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
        use crate::config::schema::UpstreamHealthCheck;

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
}
