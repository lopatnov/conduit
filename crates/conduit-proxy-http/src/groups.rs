//! Two-level (grouped) upstream routing (issue #143) — moved out of the root
//! crate's `router.rs`'s `resolve_proxy_routes_for_target` in PR A2 (issue
//! #419), updated to return [`ProxyResolution`] instead of
//! `Option<RouteResolution>` (the `Unresolved`/`Overloaded` outcomes replace
//! the previous bare `None`/`Some(overloaded())` returns — every one of this
//! function's early-return paths used to propagate as a bare `None` that
//! discarded any already-computed state; `ProxyResolution` fixes that class
//! of bug the same way #415 fixed it for the flat-route path, see
//! `crate::resolve`), then moved into this crate in PR B (issue #143
//! itself).

use std::sync::atomic::AtomicUsize;

use dashmap::DashMap;

use conduit_upstream::health::UpstreamRegistry;
use conduit_upstream::{LoadBalanceStrategy, ProxyTarget, UpstreamGroup};

use crate::config::RetryConfig;
use crate::options::{ProxyCtx, RouteOptions};
use crate::outcome::{self, ProxyResolution, ProxyUpstream};
use crate::retry::pick_with_retry;
use crate::state::{ProxyReqState, RetryState};
use crate::{capacity, slow_start::Ramp};

/// Extra context required by hash-based and weighted strategies.
struct HashCtx<'a> {
    /// `(url, weight)` pairs for `WeightedRoundRobin`.
    weighted: &'a [(String, u32)],
    /// Precomputed FNV-1a hash of the appropriate key (client IP or request
    /// URL) for `IpHash` and `ConsistentHash`.
    hash_val: u64,
}

/// Pick a URL and optional retry state according to the configured strategy.
///
/// Returns `(url, retry_state, is_least_conn)`.  `is_least_conn` is `true`
/// when the inflight counter on `upstream_health` has already been incremented
/// so the caller knows to store the URL for later decrement.
///
/// Strategy dispatch is delegated to [`conduit_upstream::strategy`] — to add
/// a new load-balancing strategy, implement
/// [`conduit_upstream::strategy::LoadBalancingStrategy`] there and map it in
/// `strategy::from_config`. This function does not need to change.
fn pick_url_by_strategy(
    urls: &[String],
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
    retry_cfg: Option<&RetryConfig>,
    strategy: Option<&LoadBalanceStrategy>,
    upstream_health: &UpstreamRegistry,
    hash_ctx: &HashCtx<'_>,
) -> Option<(String, Option<RetryState>, bool)> {
    // With retry configured, always use round-robin rotation regardless of
    // strategy. Only ever reached from tests calling this function
    // directly with `retry_cfg: Some(_)` -- the sole production caller
    // (`resolve_grouped`'s outer group pick) always passes `None`, since
    // groups don't support retry in V1 (see `routes.rs`/this module's own
    // comments to that effect) -- so there is no real
    // `max_conns_per_upstream` to thread through here.
    if let Some(retry) = retry_cfg {
        let (url, state) = pick_with_retry(urls, route_key, counters, retry, None)?;
        return Some((url, Some(state), false));
    }

    let s = conduit_upstream::strategy::from_config(
        strategy.unwrap_or(&LoadBalanceStrategy::RoundRobin),
    );
    let (url, is_least_conn) = s.pick(
        urls,
        hash_ctx.weighted,
        route_key,
        hash_ctx.hash_val,
        counters,
        upstream_health,
    )?;
    Some((url, None, is_least_conn))
}

/// Two-level load balancing: pick a group via `group_strategy`, then pick a
/// target within the group using each group's own `strategy`.
///
/// Group selection keys:
/// - `hash_key = "ip"` → hash client IP across groups (sticky per client)
/// - `hash_key = "url"` → hash request path across groups
/// - Other strategies (round-robin, random, least-conn, …) work as usual.
pub(crate) fn resolve_grouped(
    group_strategy: Option<&LoadBalanceStrategy>,
    groups: &[UpstreamGroup],
    route_key: &str,
    ctx: &ProxyCtx<'_>,
    opts: &RouteOptions<'_>,
) -> ProxyResolution {
    if groups.is_empty() {
        return ProxyResolution::unresolved(ProxyReqState::default());
    }

    // Outer pick: choose which group handles this request. Group selection
    // itself is not capacity-aware — see the inner pick below for that.
    let group_key = format!("{route_key}__group");
    let hash_input = if opts.hash_key == "url" || ctx.client_ip.is_empty() {
        ctx.path
    } else {
        ctx.client_ip
    };
    let hash_val = conduit_upstream::targets::fnv1a_hash(hash_input);

    let group_names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
    let picked_name = {
        let hash_ctx = HashCtx {
            weighted: &[],
            hash_val,
        };
        pick_url_by_strategy(
            &group_names,
            &group_key,
            ctx.counters,
            None,
            group_strategy,
            ctx.upstream_health,
            &hash_ctx,
        )
        .map(|(name, _, _)| name)
    };
    let Some(picked_name) = picked_name else {
        return ProxyResolution::unresolved(ProxyReqState::default());
    };

    let Some(group) = groups.iter().find(|g| g.name == picked_name) else {
        return ProxyResolution::unresolved(ProxyReqState::default());
    };

    // Inner pick: choose a target within the selected group.
    let all_urls: Vec<String> = group
        .targets
        .iter()
        .map(|t| match t {
            ProxyTarget::Simple(u) => u.clone(),
            ProxyTarget::Weighted(w) => w.url.clone(),
        })
        .collect();
    let weighted: Vec<(String, u32)> = group
        .targets
        .iter()
        .map(|t| match t {
            ProxyTarget::Simple(u) => (u.clone(), 1u32),
            ProxyTarget::Weighted(w) => (w.url.clone(), w.weight),
        })
        .collect();

    let (healthy, _fail_open) = ctx.upstream_health.filter_healthy(&all_urls);
    let healthy_urls: Vec<String> = healthy.iter().cloned().cloned().collect();
    // WeightedRoundRobin reads `weighted`, not the healthy URL list — filter
    // it to health here (capacity-filtering happens inside `pick_bounded`).
    let weighted_healthy: Vec<(String, u32)> = weighted
        .into_iter()
        .filter(|(u, _)| healthy_urls.contains(u))
        .collect();

    // Circuit breaker: same per-upstream capacity filtering as the flat-route
    // path (#156). V1 semantic: all targets in the *selected* group at cap →
    // 503, even if a different group had room — group selection is usually
    // affinity-driven, so silently jumping groups would surprise more than
    // shedding does.
    let capacity = capacity::Capacity::evaluate(
        &healthy_urls,
        opts.max_conns_per_upstream,
        route_key,
        ctx.upstream_health,
    );
    if matches!(capacity, capacity::Capacity::Exhausted) {
        // Explicit, not a fallthrough — capacity exhaustion is a 503, not "no
        // route matched"; letting it become `Unresolved` here would fall all
        // the way through to a generic Fallback instead.
        return ProxyResolution::overloaded(ProxyReqState::default());
    }
    let inner_key = format!("{route_key}__group__{}", group.name);
    // Slow start (#157): group selection itself stays ramp-unaware (matches
    // the existing capacity-breaker semantic documented above -- V1 acts
    // within the selected group only), but the inner target pick honors it.
    let ramp = Ramp::new(opts.slow_start_secs, ctx.upstream_health);
    let inner_input = capacity::BoundedPick {
        strategy: group.strategy.as_ref(),
        healthy: &healthy_urls,
        capacity: &capacity,
        weighted: &weighted_healthy,
        route_key: &inner_key,
        hash_val,
        counters: ctx.counters,
        health: ctx.upstream_health,
        ramp: &ramp,
    };
    let Some((chosen_url, is_least_conn)) = capacity::pick_bounded(&inner_input) else {
        return ProxyResolution::unresolved(ProxyReqState::default());
    };
    let retry_state: Option<RetryState> = None; // groups don't support retry in V1

    // Parse BEFORE acquiring the circuit_tracking slot below — matches the
    // flat-route path's ordering (#156) so a malformed URL can never leak a
    // slot nothing will release.
    let strip = opts
        .strip_prefix
        .then(|| route_key.trim_end_matches('/').to_string());
    let upstream: ProxyUpstream = match outcome::url_to_proxy_upstream(&chosen_url, strip) {
        Some(base) => ProxyUpstream {
            rewrite: opts.rewrite.map(<[_]>::to_vec),
            mirror_url: None, // groups don't support mirror in V1
            upstream_tls: None,
            ..base
        },
        None => {
            if is_least_conn {
                ctx.upstream_health.conn_dec(&chosen_url);
            }
            return ProxyResolution::unresolved(ProxyReqState::default());
        }
    };

    // Same accounting shape as the flat-route path (#155/#156): a slot is
    // only acquired when least-conn didn't already track it but a cap is
    // configured, so the capacity check above stays fed for every strategy.
    // Placed after the successful parse — see the ordering note above.
    let circuit_tracking = opts.max_conns_per_upstream.is_some() && !is_least_conn;
    if circuit_tracking {
        ctx.upstream_health.conn_inc(&chosen_url);
    }

    // proxy_upstream_url is populated unconditionally (#155) so passive-health
    // attribution works for every strategy; upstream_conn_slot tracks whether
    // this request actually holds a conn_count slot to release.
    let proxy_upstream_url = Some(chosen_url.clone());
    ProxyResolution::upstream(
        upstream,
        ProxyReqState {
            retry: retry_state,
            proxy_timeout: opts.timeout.cloned(),
            proxy_pool: opts.pool.cloned(),
            proxy_http2: opts.http2,
            proxy_upstream_url,
            upstream_conn_slot: is_least_conn || circuit_tracking,
            proxy_cache_cfg: opts.cache.cloned(),
            passive_unhealthy_status: Vec::new(), // groups don't have per-route healthCheck
            passive_unhealthy_latency_ms: None,
            websocket_allowed: false, // groups don't support websocket config in V1
            sticky_set_cookie: None,  // groups don't support sticky in V1
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_hash<'a>() -> HashCtx<'a> {
        HashCtx {
            weighted: &[],
            hash_val: 0,
        }
    }

    #[test]
    fn strategy_default_round_robin() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let (url, retry, is_lc) =
            pick_url_by_strategy(&urls, "r", &counters, None, None, &reg, &no_hash()).unwrap();
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
            &urls,
            "r",
            &counters,
            None,
            Some(&LoadBalanceStrategy::Random),
            &reg,
            &no_hash(),
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
            &urls,
            "r",
            &counters,
            None,
            Some(&LoadBalanceStrategy::LeastConn),
            &reg,
            &no_hash(),
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
            budget_percent: None,
            backoff_jitter: None,
        };
        let (url, retry, is_lc) = pick_url_by_strategy(
            &urls,
            "r",
            &counters,
            Some(&retry_cfg),
            Some(&LoadBalanceStrategy::LeastConn),
            &reg,
            &no_hash(),
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
        assert!(pick_url_by_strategy(&[], "r", &counters, None, None, &reg, &no_hash()).is_none());
    }

    #[test]
    fn strategy_wrr_selects_by_weight() {
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let weighted = vec![
            ("http://a:4000".to_string(), 3u32),
            ("http://b:4000".to_string(), 1u32),
        ];
        let ctx = HashCtx {
            weighted: &weighted,
            hash_val: 0,
        };
        let results: Vec<_> = (0..4)
            .map(|_| {
                pick_url_by_strategy(
                    &urls,
                    "r",
                    &counters,
                    None,
                    Some(&LoadBalanceStrategy::WeightedRoundRobin),
                    &reg,
                    &ctx,
                )
                .unwrap()
                .0
            })
            .collect();
        let a_count = results
            .iter()
            .filter(|u| u.as_str() == "http://a:4000")
            .count();
        assert_eq!(a_count, 3, "a should win 3 out of 4 slots");
    }

    #[test]
    fn strategy_ip_hash_is_deterministic() {
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let hash_val = conduit_upstream::targets::fnv1a_hash("1.2.3.4");
        let ctx = HashCtx {
            weighted: &[],
            hash_val,
        };
        let first = pick_url_by_strategy(
            &urls,
            "r",
            &counters,
            None,
            Some(&LoadBalanceStrategy::IpHash),
            &reg,
            &ctx,
        )
        .unwrap()
        .0;
        let second = pick_url_by_strategy(
            &urls,
            "r",
            &counters,
            None,
            Some(&LoadBalanceStrategy::IpHash),
            &reg,
            &ctx,
        )
        .unwrap()
        .0;
        assert_eq!(
            first, second,
            "same IP hash must always select the same upstream"
        );
    }

    #[test]
    fn strategy_lrt_returns_valid_url() {
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let (url, _, is_lc) = pick_url_by_strategy(
            &urls,
            "r",
            &counters,
            None,
            Some(&LoadBalanceStrategy::LeastResponseTime),
            &reg,
            &no_hash(),
        )
        .unwrap();
        assert!(urls.contains(&url));
        assert!(!is_lc);
    }

    // ── pick_url_by_strategy direct tests ─────────────────────────────────────

    #[test]
    fn pick_url_by_strategy_round_robin_picks_url() {
        let urls = vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()];
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let hash_ctx = HashCtx {
            weighted: &[],
            hash_val: 0,
        };
        let result = pick_url_by_strategy(
            &urls,
            "route",
            &counters,
            None,
            Some(&LoadBalanceStrategy::RoundRobin),
            &reg,
            &hash_ctx,
        );
        assert!(result.is_some(), "must pick a URL");
        let (url, retry, _) = result.unwrap();
        assert!(
            url == "http://a:4000" || url == "http://b:4000",
            "must pick from list: {url}"
        );
        assert!(retry.is_none(), "no retry config → no retry state");
    }

    #[test]
    fn pick_url_by_strategy_empty_list_returns_none() {
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let hash_ctx = HashCtx {
            weighted: &[],
            hash_val: 0,
        };
        let result = pick_url_by_strategy(
            &[],
            "route",
            &counters,
            None,
            Some(&LoadBalanceStrategy::RoundRobin),
            &reg,
            &hash_ctx,
        );
        assert!(result.is_none(), "empty URL list must return None");
    }

    #[test]
    fn pick_url_by_strategy_with_retry_returns_retry_state() {
        let urls = vec!["http://a:4000".to_owned()];
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let hash_ctx = HashCtx {
            weighted: &[],
            hash_val: 0,
        };
        let retry = RetryConfig {
            attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            budget_percent: None,
            backoff_jitter: None,
        };
        let result = pick_url_by_strategy(
            &urls,
            "route",
            &counters,
            Some(&retry),
            None,
            &reg,
            &hash_ctx,
        );
        assert!(result.is_some());
        let (_, retry_state, _) = result.unwrap();
        assert!(
            retry_state.is_some(),
            "retry config must produce retry state"
        );
    }
}
