//! Proxy-map (`site.proxy` as a `Routes` map) target resolution (issue #143,
//! PR A2 of a 3-PR plan) — split out of `router.rs`'s former
//! `resolve_proxy_routes_for_target` (~250 lines) into a flat orchestrator
//! plus named helper functions, same style as PR #91/#92's
//! `request_phase.rs`/`logging_phase.rs` split. Every helper below returns
//! [`ProxyResolution`] instead of `Option<RouteResolution>` — see
//! `routing::outcome`'s module doc for why.

use crate::config::schema::{ProxyRouteTarget, RateLimitConfig, StickyConfig};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::routing::groups::resolve_grouped;
use crate::proxy::routing::options::{ProxyCtx, RouteOptions};
use crate::proxy::routing::outcome::{self, ProxyResolution, ProxyUpstream};
use crate::proxy::routing::peer_pick::{build_pool, pick_peer_with_retry};
use crate::proxy::routing::state::{ProxyReqState, RouteRateLimit};
use crate::proxy::routing::sticky;
use crate::proxy::{router, upstream};

/// Match a request against the `proxy` map's routes and stamp the matched
/// route's rate limit/priority onto the resulting resolution.
///
/// The stamp is applied **after** [`resolve_target`] returns, regardless of
/// which outcome it produced (`Upstream`/`Overloaded`/`Unresolved`) — a
/// request that matches a route but resolves to an overloaded/unresolved
/// outcome must still carry that route's rate limit (#360, #415): the
/// previous design (a second, post-routing path matcher) applied
/// unconditionally to every outcome, so replicating that here means
/// evaluating `route_limits_from_target` once up front and stamping it onto
/// whatever [`ProxyResolution`] comes back.
///
/// **Issue #415 fix**: before this PR, the inner resolution function
/// returned `Option<RouteResolution>`, and a `?` on its `None` result here
/// discarded the already-computed stamp — a proxy-map route with `rateLimit`
/// configured whose target failed to resolve (e.g. malformed URL) fell
/// through to fallback/static carrying NO rate-limit stamp. Now that the
/// inner function always returns a `ProxyResolution` (never a bare `None`),
/// there is no `?` to short-circuit past — the stamp below applies
/// unconditionally, including to `ProxyOutcome::Unresolved`.
pub(crate) fn resolve_proxy_routes(
    routes: &indexmap::IndexMap<String, ProxyRouteTarget>,
    ctx: &ProxyCtx<'_>,
) -> Option<ProxyResolution> {
    let (route_key, route_target) = find_route(routes, ctx.path)?;
    let (route_rate_limit, route_priority) = route_limits_from_target(route_target, route_key);

    let mut resolution = resolve_target(route_key, route_target, ctx);
    resolution.state.route_rate_limit = route_rate_limit;
    resolution.state.route_priority = route_priority;
    Some(resolution)
}

/// Extract the per-route rate limit / priority from a matched `ProxyRouteTarget`.
///
/// Returns `(None, None)` for the `Url`/`RoundRobin` shorthand variants —
/// only `Full(ProxyRouteConfig)` carries `rateLimit`/`priority`. Shared
/// between the legacy `proxy` map path (`resolve_proxy_routes` above) and the
/// `routes[]` array path (`routes.rs::match_routes`) so both matchers stamp
/// the resolution the exact same way (#360).
pub(crate) fn route_limits_from_target(
    target: &ProxyRouteTarget,
    route_key: &str,
) -> (Option<RouteRateLimit>, Option<u8>) {
    let ProxyRouteTarget::Full(cfg) = target else {
        return (None, None);
    };
    let rate_limit: Option<RateLimitConfig> = cfg.rate_limit.clone();
    let rate_limit = rate_limit.map(|config| RouteRateLimit {
        config,
        route_key: route_key.to_owned(),
    });
    (rate_limit, cfg.priority)
}

/// Resolve a single `proxy` map route's `Full` target — strategy dispatch,
/// health filtering, circuit-breaker capacity (#156), sticky-session pinning
/// (#220, #366), slow-start ramp (#157), and retry-state construction
/// (#367, #374, #375, #216 part 2). Every early-return path below produces a
/// [`ProxyResolution`] rather than a bare `None`, so #415's fix
/// (`resolve_proxy_routes` above) has a real stamp to apply regardless of
/// which phase bailed out.
fn resolve_target(
    route_key: &str,
    route_target: &ProxyRouteTarget,
    ctx: &ProxyCtx<'_>,
) -> ProxyResolution {
    // ── Two-level (grouped) routing ─────────────────────────────────────────
    // When the route config has `groups`, bypass flat-target logic and
    // resolve via pick_group → pick_within_group.
    let opts = RouteOptions::from_target(route_target);

    if let ProxyRouteTarget::Full(cfg) = route_target {
        if let Some(groups) = &cfg.groups {
            return resolve_grouped(cfg.group_strategy.as_ref(), groups, route_key, ctx, &opts);
        }
    }

    let (all_urls, all_weighted_base) = effective_targets(route_target, route_key, ctx);

    // Failover: when a backup URL is configured and all primary upstreams
    // are unhealthy, route to the backup instead.
    if let Some(resolution) = resolve_backup(&all_urls, opts.backup, ctx.upstream_health) {
        return resolution;
    }

    let pool = match build_pool(&all_urls, all_weighted_base, &opts, route_key, ctx) {
        Ok(pool) => pool,
        Err(resolution) => return *resolution,
    };

    let peer = match pick_peer_with_retry(&pool, &opts, &all_urls, route_key, ctx) {
        Ok(peer) => peer,
        Err(resolution) => return *resolution,
    };

    let strip = opts
        .strip_prefix
        .then(|| route_key.trim_end_matches('/').to_string());

    // url_to_proxy_upstream may return None for a malformed URL. If
    // least-conn already incremented the inflight counter, build_proxy_upstream
    // releases it — the logging() hook won't run on this request. Parsing
    // BEFORE the circuit_tracking conn_inc below (see it) means a malformed
    // URL can never leak a circuit-tracking slot nothing will release.
    let Some(upstream) = build_proxy_upstream(
        &peer.chosen_url,
        strip,
        &opts,
        peer.is_least_conn,
        ctx.upstream_health,
    ) else {
        return ProxyResolution::unresolved(ProxyReqState::default());
    };

    let (proxy_upstream_url, upstream_conn_slot) = track_conn_slot(
        opts.max_conns_per_upstream,
        peer.is_least_conn,
        &peer.chosen_url,
        ctx.upstream_health,
    );

    let sticky_set_cookie = finalize_sticky_cookie(
        opts.sticky,
        peer.pinned.as_deref(),
        peer.honored_pin,
        peer.fail_open,
        &pool.healthy_urls,
        &peer.chosen_url,
    );

    ProxyResolution::upstream(
        upstream,
        ProxyReqState {
            retry: peer.retry_state,
            proxy_timeout: opts.timeout.cloned(),
            proxy_pool: opts.pool.cloned(),
            proxy_http2: opts.http2,
            proxy_upstream_url,
            upstream_conn_slot,
            proxy_cache_cfg: opts.cache.cloned(),
            passive_unhealthy_status: opts.unhealthy_status.to_vec(),
            passive_unhealthy_latency_ms: opts.unhealthy_latency_ms,
            websocket_allowed: opts.websocket,
            sticky_set_cookie,
            // Stamped by the `resolve_proxy_routes` wrapper above, regardless
            // of which of this function's return paths produced the result
            // (#360, #415).
            ..Default::default()
        },
    )
}

/// Health-filtered, capacity-checked candidate pool for one routing decision.
/// When maxConnectionsPerUpstream is set and the strategy is NOT
/// least-conn (which already tracks conn_count), increment the counter
/// manually here so the circuit breaker sees accurate load. Decremented
/// by logging() via proxy_upstream_url, gated on upstream_conn_slot.
fn track_conn_slot(
    max_conns_per_upstream: Option<u64>,
    is_least_conn: bool,
    chosen_url: &str,
    upstream_health: &UpstreamRegistry,
) -> (Option<String>, bool) {
    let circuit_tracking = max_conns_per_upstream.is_some() && !is_least_conn;
    if circuit_tracking {
        upstream_health.conn_inc(chosen_url);
    }
    // proxy_upstream_url is populated unconditionally (#155) so passive-health
    // attribution works for every strategy; upstream_conn_slot separately
    // tracks whether this request actually holds a conn_count slot to release.
    let proxy_upstream_url = Some(chosen_url.to_owned());
    let upstream_conn_slot = is_least_conn || circuit_tracking;
    (proxy_upstream_url, upstream_conn_slot)
}

/// HMAC sticky: sign the chosen upstream URL and schedule a Set-Cookie
/// injection on the response side.
///
/// Skip re-signing when this pick was a capacity relocation away from a
/// still-healthy pinned peer (#156 review finding): re-signing the cookie
/// to the relocated fallback would permanently migrate the session to it,
/// since the next request's hash input is derived from the *pinned URL
/// string itself* (see `selection_hash_val`), not the client's original
/// identity — the fallback would then stay "pinned to itself" even after
/// the originally-preferred peer frees capacity. Leaving the existing
/// cookie untouched means the next request retries the original pin, so
/// sticky sessions genuinely self-heal once capacity is available again,
/// matching the same self-healing property already tested for plain
/// (non-sticky) hash routing.
/// A *genuine* relocation: we had an exact pin, could not honor it, and
/// the pin is still healthy — i.e. it is merely at capacity right now and
/// will be serviceable again shortly.
///
/// Before #220 this condition was `pinned != chosen_url && healthy`,
/// which also matched the (then-usual) case of the hash simply landing on
/// a different peer than the pin — so on ~3 of every 4 sticky requests it
/// wrongly concluded "capacity relocation" and suppressed re-signing,
/// masking the real bug. Now that an honorable pin is always honored,
/// `pinned && !honored` can only mean unhealthy-or-saturated, and the
/// `healthy` check cleanly separates the two: saturated → keep the cookie
/// (self-heal), gone → re-sign onto wherever the strategy relocated us.
///
/// `!fail_open` (#374): during a total-pool outage, `healthy_urls` is a
/// fail-open passthrough containing *every* peer, including one that's
/// actually down -- so `healthy_urls.contains(pin)` can't tell "pin is
/// genuinely healthy but over capacity" apart from "nothing is healthy,
/// pin included, we're just trying anyway." When `fail_open` is true we
/// already know for certain the pin isn't really healthy (fail-open only
/// triggers when *zero* peers pass the real check), so don't classify
/// this as a self-healing capacity relocation -- re-sign the cookie onto
/// wherever we actually landed instead of quietly promising a comeback
/// that isn't backed by a real health signal.
fn finalize_sticky_cookie(
    sticky_cfg: Option<&StickyConfig>,
    pinned: Option<&str>,
    honored_pin: bool,
    fail_open: bool,
    healthy_urls: &[String],
    chosen_url: &str,
) -> Option<(String, String)> {
    let sticky_relocated =
        pinned.is_some_and(|p| !honored_pin && !fail_open && healthy_urls.iter().any(|h| h == p));
    if sticky_relocated {
        None
    } else {
        sticky::make_sticky_cookie(sticky_cfg, chosen_url)
    }
}

/// Runtime-override-aware target list. When the operator has issued
/// `conduit upstreams add/remove/weight`, those targets replace the
/// config-file targets for this route.
fn effective_targets(
    route_target: &ProxyRouteTarget,
    route_key: &str,
    ctx: &ProxyCtx<'_>,
) -> (Vec<String>, Vec<(String, u32)>) {
    let Some(ov) = ctx
        .upstream_health
        .get_override_targets(ctx.site_label, route_key)
    else {
        return (
            upstream::target_urls(route_target),
            upstream::weighted_targets(route_target),
        );
    };
    let urls = ov.iter().map(|(u, _)| u.clone()).collect();
    (urls, ov)
}

/// `None` when failover does not apply (caller continues normal
/// load-balancing); `Some(resolution)` when it does — the resolution is
/// `Unresolved` rather than a bare fallthrough when the configured backup
/// URL itself is malformed, so #415's fix (a stamped rate-limit/priority
/// carried through fallthrough) covers this trigger too.
fn resolve_backup(
    all_urls: &[String],
    backup: Option<&str>,
    upstream_health: &UpstreamRegistry,
) -> Option<ProxyResolution> {
    let all_unhealthy =
        !all_urls.is_empty() && all_urls.iter().all(|u| !upstream_health.is_healthy(u));
    if !all_unhealthy {
        return None;
    }
    let backup = backup?;
    tracing::info!(backup = %backup, "all primary upstreams unhealthy — routing to backup");
    let resolution = match router::url_to_proxy_upstream(backup, None)
        .and_then(outcome::upstream_target_into_proxy_upstream)
    {
        Some(upstream) => ProxyResolution::upstream(upstream, ProxyReqState::default()),
        None => ProxyResolution::unresolved(ProxyReqState::default()),
    };
    Some(resolution)
}

/// Build the final `ProxyUpstream`, attaching per-route rewrite/mirror/
/// upstream-TLS settings. Releases the least-conn inflight slot when the URL
/// is malformed — `logging()` will not run for this request.
fn build_proxy_upstream(
    chosen_url: &str,
    strip: Option<String>,
    opts: &RouteOptions<'_>,
    is_least_conn: bool,
    upstream_health: &UpstreamRegistry,
) -> Option<ProxyUpstream> {
    let Some(target) = router::url_to_proxy_upstream(chosen_url, strip) else {
        if is_least_conn {
            upstream_health.conn_dec(chosen_url);
        }
        return None;
    };
    // `url_to_proxy_upstream` only ever returns the `Proxy` variant when
    // `Some` (see `routing::outcome`'s own doc comment) — this `expect` is
    // documentation of that invariant, not a real failure path.
    let base = outcome::upstream_target_into_proxy_upstream(target)
        .expect("url_to_proxy_upstream only ever returns the Proxy variant");
    Some(ProxyUpstream {
        rewrite: opts.rewrite.map(<[_]>::to_vec),
        mirror_url: opts.mirror.map(str::to_owned),
        upstream_tls: opts.upstream_tls.cloned(),
        ..base
    })
}

fn find_route<'a>(
    routes: &'a indexmap::IndexMap<String, ProxyRouteTarget>,
    path: &str,
) -> Option<(&'a str, &'a ProxyRouteTarget)> {
    let mut best: Option<(&str, &ProxyRouteTarget)> = None;
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

// `find_route_rate_limit`/`find_route_priority` (a second, post-routing path
// matcher scanning only `site.proxy`) were deleted for issue #360 — they
// could disagree with the routing decision itself: a site with both
// `routes[]` and a `proxy` map (a legal, documented backward-compatible
// shape) got the *non-selected* mechanism's rate limit applied to
// `routes[]`-served requests. Rate limit/priority are now stamped onto the
// `RouteResolution`/`RequestCtx` directly by whichever matcher actually
// matched (`route_limits_from_target` above, called from both
// `resolve_proxy_routes` here and `routes.rs::match_routes`), so enforcement
// reads `RequestCtx.proxy.route_rate_limit`/`route_priority` and can never disagree
// with routing. See the deleted functions' former test coverage, now ported
// to assert through `route_request(...)` instead, in `router.rs`'s own test
// module.

#[cfg(test)]
mod tests {
    use super::*;

    // ── find_route ────────────────────────────────────────────────────────────

    #[test]
    fn find_route_longest_prefix_wins() {
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
        use indexmap::IndexMap;
        let mut routes = IndexMap::new();
        routes.insert(
            "/api".to_string(),
            ProxyRouteTarget::Url("http://api:4000".to_string()),
        );
        assert!(find_route(&routes, "/other").is_none());
    }
}
