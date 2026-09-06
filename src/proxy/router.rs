use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "static")]
use std::sync::Arc;

use dashmap::DashMap;

use crate::config::schema::{
    AppConfig, CacheConfig, ConnectionPoolConfig, LoadBalanceStrategy, ProxyConfig,
    ProxyRouteTarget, ProxyTimeout, RetryConfig, RewriteRule, SiteConfig, StickyConfig,
    UpstreamGroup, UpstreamTlsConfig,
};
use crate::proxy::capacity;
use crate::proxy::ctx::{LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::slow_start::Ramp;
use crate::proxy::upstream;

/// Resolved routing result: all per-route data needed to populate `RequestCtx`.
///
/// Replaces the previous 7-element positional tuple.  Named fields make it
/// safe to add new fields (no silent positional-shift bugs) and dramatically
/// improve readability at call sites.
#[derive(Debug)]
pub struct RouteResolution {
    pub upstream: UpstreamTarget,
    /// Retry state (URLs + attempt counter) when `retry` is configured.
    pub retry: Option<RetryState>,
    /// Per-route connection timeouts.
    pub proxy_timeout: Option<ProxyTimeout>,
    /// Per-route connection-pool settings.
    pub proxy_pool: Option<ConnectionPoolConfig>,
    /// Negotiate HTTP/2 with the upstream when `true`.
    pub proxy_http2: bool,
    /// Selected upstream URL — `Some` for every proxy route so passive-health
    /// attribution (EWMA, Outlier Detection, per-peer stats) works regardless
    /// of load-balancing strategy. See `upstream_conn_slot` for whether a
    /// `conn_count` slot also needs releasing.
    pub proxy_upstream_url: Option<String>,
    /// `true` when a `conn_count` slot was acquired for `proxy_upstream_url`
    /// and must be released by `logging()` via `conn_dec`. `false` means the
    /// URL above is for attribution only — no slot to release.
    pub upstream_conn_slot: bool,
    /// Per-route cache config, if caching is enabled.
    pub proxy_cache_cfg: Option<CacheConfig>,
    /// Passive health: HTTP status codes that count as upstream failures.
    /// Populated from `healthCheck.unhealthyStatus`.
    pub passive_unhealthy_status: Vec<u16>,
    /// Passive health: latency threshold in ms above which response counts as failure.
    /// Populated from `healthCheck.unhealthyLatencyMs`.
    pub passive_unhealthy_latency_ms: Option<u64>,
    /// Whether WebSocket upgrades are permitted on this route.
    /// Populated from `proxy.*.websocket: true` in the route config.
    pub websocket_allowed: bool,
    /// HMAC-signed sticky cookie to inject in the response (`Set-Cookie`).
    ///
    /// `Some((name, value))` when `sticky.secret` is configured.  The
    /// `upstream_response_filter` in `service.rs` injects the header.
    pub sticky_set_cookie: Option<(String, String)>,
}

impl RouteResolution {
    /// Convenience constructor for local-handler routes that don't need
    /// any proxy-specific fields.
    pub fn local(upstream: UpstreamTarget) -> Self {
        Self {
            upstream,
            retry: None,
            proxy_timeout: None,
            proxy_pool: None,
            proxy_http2: false,
            proxy_upstream_url: None,
            upstream_conn_slot: false,
            proxy_cache_cfg: None,
            passive_unhealthy_status: Vec::new(),
            passive_unhealthy_latency_ms: None,
            websocket_allowed: false,
            sticky_set_cookie: None,
        }
    }
}

/// Backward-compatible type alias used in `src/proxy/routes.rs`.
pub type RouteResultAlias = RouteResolution;

type RouteResult = RouteResolution;

#[allow(clippy::too_many_arguments)]
pub fn route_request(
    config: &AppConfig,
    host: &str,
    path: &str,
    method: &str,
    req_headers: &http::HeaderMap,
    query: Option<&str>,
    client_ip: &str,
    server_port: u16,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
    upload_addr: Option<SocketAddr>,
) -> RequestCtx {
    let site_idx = find_site_idx(config, host, server_port).unwrap_or(0);
    let site = config.sites.get(site_idx);

    let res: RouteResolution = if let Some(token) = acme_challenge_token(path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::AcmeChallenge {
            token: token.to_owned(),
        }))
    } else if is_health_path(site, path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::Health))
    } else if let Some(token) = metrics_token(site, path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::Metrics { token }))
    } else if is_hot_reload_js_path(site, path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::HotReloadJs))
    } else if is_hot_reload_sse_path(site, path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::HotReloadSse))
    } else if let Some(site) = site {
        route_site(
            site,
            path,
            method,
            req_headers,
            query,
            client_ip,
            counters,
            upstream_health,
            upload_addr,
        )
    } else {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::Fallback))
    };

    let response_transform = site.and_then(|s| s.response_transform.clone());
    let mut ctx = RequestCtx::new(
        site_idx,
        res.upstream,
        res.retry,
        res.proxy_timeout,
        res.proxy_pool,
        res.proxy_http2,
        res.proxy_upstream_url,
        res.proxy_cache_cfg,
        response_transform,
    );
    // Populate passive health thresholds so logging() can apply them.
    ctx.passive_unhealthy_status = res.passive_unhealthy_status;
    ctx.passive_unhealthy_latency_ms = res.passive_unhealthy_latency_ms;
    ctx.websocket_allowed = res.websocket_allowed;
    ctx.sticky_set_cookie = res.sticky_set_cookie;
    ctx.upstream_conn_slot = res.upstream_conn_slot;
    ctx
}

#[allow(clippy::too_many_arguments)]
fn route_site(
    site: &SiteConfig,
    path: &str,
    method: &str,
    req_headers: &http::HeaderMap,
    query: Option<&str>,
    client_ip: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
    #[cfg_attr(not(feature = "upload"), allow(unused_variables))] upload_addr: Option<SocketAddr>,
) -> RouteResult {
    let site_label = crate::proxy::health::site_label(&site.host, site.port);

    #[cfg(feature = "upload")]
    if let Some(result) = match_upload_route(site, path, upload_addr) {
        return result;
    }
    if let Some(result) = match_routes_array(
        site,
        path,
        method,
        req_headers,
        query,
        counters,
        upstream_health,
    ) {
        return result;
    }
    if let Some(proxy_cfg) = &site.proxy {
        let proxy_ctx = ProxyCtx {
            path,
            client_ip,
            req_headers,
            counters,
            upstream_health,
            site_label: &site_label,
        };
        if let Some(result) = resolve_proxy(proxy_cfg, &proxy_ctx) {
            return result;
        }
    }
    match_static_or_fallback(site, path)
}

/// Check whether the request targets the configured upload prefix.
///
/// Upload path takes priority — it is a precise prefix configured by the
/// operator and must not be shadowed by a catch-all proxy route.
#[cfg(feature = "upload")]
fn match_upload_route(
    site: &SiteConfig,
    path: &str,
    upload_addr: Option<SocketAddr>,
) -> Option<RouteResult> {
    let (upload_cfg, addr) = site.upload.as_ref().zip(upload_addr)?;
    let upload_prefix = upload_cfg.path.trim_end_matches('/');
    let matches = path == upload_prefix || path.starts_with(&format!("{upload_prefix}/"));
    matches.then_some(RouteResolution::local(UpstreamTarget::Upload { addr }))
}

/// Match against the `routes` array (evaluated before legacy `proxy`/`static`).
///
/// Each `RouteConfig` has a `MatchConfig` (path glob, method, headers, query)
/// plus an action (proxy or static).  First match wins.
#[allow(clippy::too_many_arguments)]
fn match_routes_array(
    site: &SiteConfig,
    path: &str,
    method: &str,
    req_headers: &http::HeaderMap,
    query: Option<&str>,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> Option<RouteResult> {
    crate::proxy::routes::match_routes(
        site.routes.as_ref()?,
        path,
        method,
        req_headers,
        query,
        counters,
        upstream_health,
        site.static_options.as_ref(),
    )
}

/// Serve static files when configured, or fall through to the global fallback handler.
///
/// Without the `static` feature compiled in, `sites[].static` still parses
/// (`feature_warnings()` surfaces it) but never routes to a static-file
/// handler — every such request falls straight through to the plain
/// `Fallback` marker, matching the degradation shape of every other
/// `#[cfg]`-gated `LocalHandler` variant (see `HandlerKind::AcmeChallenge`'s
/// `#[cfg(not(feature = "acme"))]` arm in `request_phase.rs::build_handler`).
fn match_static_or_fallback(
    #[cfg_attr(not(feature = "static"), allow(unused_variables))] site: &SiteConfig,
    #[cfg_attr(not(feature = "static"), allow(unused_variables))] path: &str,
) -> RouteResult {
    #[cfg(feature = "static")]
    if let Some(static_cfg) = &site.static_files {
        let options = Arc::new(site.static_options.clone().unwrap_or_default());
        let (roots, strip_prefix) = resolve_static_roots(static_cfg, path);
        if !roots.is_empty() {
            return RouteResolution::local(UpstreamTarget::Local(LocalHandler::StaticFile {
                roots,
                options,
                strip_prefix,
            }));
        }
    }
    RouteResolution::local(UpstreamTarget::Local(LocalHandler::Fallback))
}

/// Inputs that stay constant while resolving one request's upstream.
struct ProxyCtx<'a> {
    path: &'a str,
    client_ip: &'a str,
    req_headers: &'a http::HeaderMap,
    counters: &'a DashMap<String, AtomicUsize>,
    upstream_health: &'a UpstreamRegistry,
    site_label: &'a str,
}

/// Per-route proxy settings, read once from `ProxyRouteTarget::Full`. All
/// fields borrow from the route config; shorthand targets (`Url` /
/// `RoundRobin`) get the documented defaults.
struct RouteOptions<'a> {
    retry: Option<&'a RetryConfig>,
    timeout: Option<&'a ProxyTimeout>,
    pool: Option<&'a ConnectionPoolConfig>,
    strategy: Option<&'a LoadBalanceStrategy>,
    http2: bool,
    hash_key: &'a str,
    cache: Option<&'a CacheConfig>,
    rewrite: Option<&'a [RewriteRule]>,
    mirror: Option<&'a str>,
    upstream_tls: Option<&'a UpstreamTlsConfig>,
    max_conns_per_upstream: Option<u64>,
    /// `healthCheck.slowStartSecs` (issue #157) — traffic ramp-up window
    /// after an upstream recovers. Ignored for hash-based strategies and
    /// sticky sessions; see `slow_start`'s module doc comment for why.
    slow_start_secs: Option<u64>,
    websocket: bool,
    unhealthy_status: &'a [u16],
    unhealthy_latency_ms: Option<u64>,
    backup: Option<&'a str>,
    sticky: Option<&'a StickyConfig>,
    strip_prefix: bool,
}

impl<'a> RouteOptions<'a> {
    fn from_target(target: &'a ProxyRouteTarget) -> Self {
        let ProxyRouteTarget::Full(cfg) = target else {
            return Self::shorthand();
        };
        let hc = cfg.health_check.as_ref();
        Self {
            retry: cfg.retry.as_ref(),
            timeout: cfg.timeout.as_ref(),
            pool: cfg.pool.as_ref(),
            strategy: cfg.strategy.as_ref(),
            http2: cfg.http2.unwrap_or(false),
            hash_key: cfg.hash_key.as_deref().unwrap_or("ip"),
            cache: cfg.cache.as_ref(),
            rewrite: cfg.rewrite.as_deref(),
            mirror: cfg.mirror.as_deref(),
            upstream_tls: cfg.upstream_tls.as_ref(),
            max_conns_per_upstream: hc.and_then(|h| h.max_connections_per_upstream),
            slow_start_secs: hc.and_then(|h| h.slow_start_secs),
            websocket: cfg.websocket.unwrap_or(false),
            unhealthy_status: hc
                .and_then(|h| h.unhealthy_status.as_deref())
                .unwrap_or(&[]),
            unhealthy_latency_ms: hc.and_then(|h| h.unhealthy_latency_ms),
            backup: cfg.backup.as_deref(),
            sticky: cfg.sticky.as_ref(),
            strip_prefix: cfg.strip_prefix.unwrap_or(false),
        }
    }

    fn shorthand() -> Self {
        Self {
            retry: None,
            timeout: None,
            pool: None,
            strategy: None,
            http2: false,
            hash_key: "ip",
            cache: None,
            rewrite: None,
            mirror: None,
            upstream_tls: None,
            max_conns_per_upstream: None,
            slow_start_secs: None,
            websocket: false,
            unhealthy_status: &[],
            unhealthy_latency_ms: None,
            backup: None,
            sticky: None,
            strip_prefix: false,
        }
    }
}

fn resolve_proxy(config: &ProxyConfig, ctx: &ProxyCtx<'_>) -> Option<RouteResult> {
    match config {
        ProxyConfig::Single(url) => Some(RouteResolution::local(url_to_proxy_upstream(url, None)?)),
        ProxyConfig::Routes(routes) => resolve_proxy_routes(routes, ctx),
    }
}

fn resolve_proxy_routes(
    routes: &indexmap::IndexMap<String, ProxyRouteTarget>,
    ctx: &ProxyCtx<'_>,
) -> Option<RouteResult> {
    let (route_key, route_target) = find_route(routes, ctx.path)?;

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
    if let Some(result) = resolve_backup(&all_urls, opts.backup, ctx.upstream_health) {
        return result;
    }

    // Filter to healthy upstreams; if all are down keep all (fail-open).
    let healthy = ctx.upstream_health.filter_healthy(&all_urls);
    let healthy_urls: Vec<String> = healthy.iter().cloned().cloned().collect();

    // Circuit breaker: per-upstream capacity filtering (#156). `Exhausted`
    // means every healthy peer is at its connection cap → 503.
    let capacity = capacity::Capacity::evaluate(
        &healthy_urls,
        opts.max_conns_per_upstream,
        route_key,
        ctx.upstream_health,
    );
    if matches!(capacity, capacity::Capacity::Exhausted) {
        return Some(overloaded());
    }

    // Build weighted list filtered to healthy targets. `pick_bounded` further
    // filters this to the admissible (under-capacity) subset internally.
    let weighted: Vec<(String, u32)> = all_weighted_base
        .into_iter()
        .filter(|(url, _)| healthy_urls.contains(url))
        .collect();

    // Sticky sessions: extract and optionally verify the session cookie.
    let sticky_override = match resolve_sticky(opts.sticky, &all_urls, ctx) {
        Sticky::Reject => return Some(overloaded()),
        Sticky::Key(key) => Some(key),
        Sticky::None => None,
    };

    // Priority: sticky cookie > hash_key config > client IP.
    let hash_val = selection_hash_val(
        sticky_override.as_deref(),
        opts.hash_key,
        ctx.path,
        ctx.client_ip,
    );
    // When sticky is active, override strategy to consistent-hash so the
    // cookie value is always used for backend selection.
    let strategy = effective_strategy(sticky_override.is_some(), opts.strategy);

    // Slow start (#157): ramp traffic to a recently-recovered upstream.
    // Constructed after `strategy` is resolved so hash/sticky routes (already
    // forced to `ConsistentHash` above) get the exemption for free -- see
    // `slow_start`'s module doc comment. `Ramp::new` is a true no-op when
    // `slow_start_secs` is unset.
    let ramp = Ramp::new(opts.slow_start_secs, ctx.upstream_health);

    // With retry configured, bypass the strategy entirely and rotate a
    // capacity-filtered candidate list (don't retry into a peer already
    // known to be saturated) — mirrors the pre-#156 retry-bypasses-strategy
    // behavior, now capacity-aware.
    let (chosen_url, retry_state, is_least_conn) = if let Some(retry) = opts.retry {
        let candidates = capacity.candidates(&healthy_urls)?;
        // This branch bypasses `pick_bounded` entirely, so without this wrap
        // `slowStartSecs` would stay a silent no-op on every retry-configured
        // route -- the same bug class #157 is about, recurring here.
        let candidates = ramp.filter_candidates(candidates);
        let (url, state) = pick_with_retry(&candidates, route_key, ctx.counters, retry)?;
        (url, Some(state), false)
    } else {
        let input = capacity::BoundedPick {
            strategy,
            healthy: &healthy_urls,
            capacity: &capacity,
            weighted: &weighted,
            route_key,
            hash_val,
            counters: ctx.counters,
            health: ctx.upstream_health,
            ramp: &ramp,
        };
        let (url, is_lc) = capacity::pick_bounded(&input)?;
        (url, None, is_lc)
    };

    let strip = opts
        .strip_prefix
        .then(|| route_key.trim_end_matches('/').to_string());

    // url_to_proxy_upstream may return None for a malformed URL. If
    // least-conn already incremented the inflight counter, build_proxy_upstream
    // releases it — the logging() hook won't run on this request. Parsing
    // BEFORE the circuit_tracking conn_inc below (see it) means a malformed
    // URL can never leak a circuit-tracking slot nothing will release.
    let upstream = build_proxy_upstream(
        &chosen_url,
        strip,
        &opts,
        is_least_conn,
        ctx.upstream_health,
    )?;

    // When maxConnectionsPerUpstream is set and the strategy is NOT
    // least-conn (which already tracks conn_count), increment the counter
    // manually here so the circuit breaker sees accurate load. Decremented
    // by logging() via proxy_upstream_url, gated on upstream_conn_slot.
    let circuit_tracking = opts.max_conns_per_upstream.is_some() && !is_least_conn;
    if circuit_tracking {
        ctx.upstream_health.conn_inc(&chosen_url);
    }
    // proxy_upstream_url is populated unconditionally (#155) so passive-health
    // attribution works for every strategy; upstream_conn_slot separately
    // tracks whether this request actually holds a conn_count slot to release.
    let proxy_upstream_url = Some(chosen_url.clone());
    let upstream_conn_slot = is_least_conn || circuit_tracking;

    // HMAC sticky: sign the chosen upstream URL and schedule a Set-Cookie
    // injection on the response side.
    //
    // Skip re-signing when this pick was a capacity relocation away from a
    // still-healthy pinned peer (#156 review finding): re-signing the cookie
    // to the relocated fallback would permanently migrate the session to it,
    // since the next request's hash input is derived from the *pinned URL
    // string itself* (see `selection_hash_val`), not the client's original
    // identity — the fallback would then stay "pinned to itself" even after
    // the originally-preferred peer frees capacity. Leaving the existing
    // cookie untouched means the next request retries the original pin, so
    // sticky sessions genuinely self-heal once capacity is available again,
    // matching the same self-healing property already tested for plain
    // (non-sticky) hash routing.
    let sticky_relocated = matches!(
        &sticky_override,
        Some(pinned) if pinned != &chosen_url && healthy_urls.contains(pinned)
    );
    let sticky_set_cookie = if sticky_relocated {
        None
    } else {
        make_sticky_cookie(opts.sticky, &chosen_url)
    };

    Some(RouteResolution {
        upstream,
        retry: retry_state,
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
    })
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
/// load-balancing); `Some(inner)` when it does apply — and `inner` may
/// itself be `None` if the configured backup URL is malformed, which must
/// propagate out of `resolve_proxy_routes` (falls through to
/// static/fallback), exactly as a normal unresolved route would.
fn resolve_backup(
    all_urls: &[String],
    backup: Option<&str>,
    upstream_health: &UpstreamRegistry,
) -> Option<Option<RouteResult>> {
    let all_unhealthy =
        !all_urls.is_empty() && all_urls.iter().all(|u| !upstream_health.is_healthy(u));
    if !all_unhealthy {
        return None;
    }
    let backup = backup?;
    tracing::info!(backup = %backup, "all primary upstreams unhealthy — routing to backup");
    Some(url_to_proxy_upstream(backup, None).map(RouteResolution::local))
}

/// Outcome of evaluating the sticky-session cookie.
enum Sticky {
    /// No sticky config, no cookie, or an unverifiable cookie in HMAC mode —
    /// use the configured load-balancing strategy.
    None,
    /// Consistent-hash key: a verified pinned upstream URL, or (legacy,
    /// no-secret mode) the raw cookie value.
    Key(String),
    /// `sticky.strict` and the pinned peer is unhealthy — refuse with 503.
    Reject,
}

/// When `sticky.secret` is set, verify the HMAC-SHA256 of each candidate
/// upstream URL to find the pinned backend. A forged or unmatched cookie
/// falls through to normal load-balancing (or returns `Reject` in strict
/// mode). Without `secret`, the raw cookie value is used as the
/// consistent-hash key (legacy behavior).
///
/// Security: a cookie that fails signature verification must NOT influence
/// routing — otherwise a client could forge/manipulate their cookie to
/// steer to specific upstreams. Raw cookie values are only used when no
/// secret is set (legacy, non-HMAC sticky).
fn resolve_sticky(
    sticky: Option<&StickyConfig>,
    all_urls: &[String],
    ctx: &ProxyCtx<'_>,
) -> Sticky {
    let Some(cfg) = sticky else {
        return Sticky::None;
    };
    let Some(cookie_val) = extract_cookie(ctx.req_headers, &cfg.cookie) else {
        return Sticky::None;
    };
    let Some(secret) = cfg.secret.as_deref() else {
        // No secret configured: use raw cookie as consistent-hash input.
        return Sticky::Key(cookie_val);
    };
    // Try to find the upstream whose HMAC matches the cookie.
    let Some(pinned) = all_urls
        .iter()
        .find(|u| hmac_verify_sticky(u, &cookie_val, secret))
    else {
        // HMAC mode but cookie failed verification: ignore — fall through
        // to the configured load-balancing strategy.
        return Sticky::None;
    };
    // Strict mode: if the client presented a signed cookie for a peer that
    // is now unhealthy, refuse the request rather than silently routing to
    // a different upstream (which would break session affinity).
    if cfg.strict.unwrap_or(false) && !ctx.upstream_health.is_healthy(pinned) {
        tracing::debug!(
            url = %pinned,
            "sticky strict mode: pinned upstream unhealthy — returning 503"
        );
        return Sticky::Reject;
    }
    Sticky::Key(pinned.clone())
}

/// Hash key for ip-hash / consistent-hash / sticky selection.
/// Priority: sticky cookie > `hashKey: "url"` (or empty client IP) > client IP.
fn selection_hash_val(
    sticky_override: Option<&str>,
    hash_key: &str,
    path: &str,
    client_ip: &str,
) -> u64 {
    let hash_input = if let Some(cookie_val) = sticky_override {
        cookie_val
    } else if hash_key == "url" || client_ip.is_empty() {
        path
    } else {
        client_ip
    };
    upstream::fnv1a_hash(hash_input)
}

/// Sticky sessions always select by consistent hash of the cookie value.
static STICKY_STRATEGY: LoadBalanceStrategy = LoadBalanceStrategy::ConsistentHash;

fn effective_strategy(
    sticky_active: bool,
    configured: Option<&LoadBalanceStrategy>,
) -> Option<&LoadBalanceStrategy> {
    if sticky_active {
        Some(&STICKY_STRATEGY)
    } else {
        configured
    }
}

/// Build the final `UpstreamTarget::Proxy`, attaching per-route rewrite /
/// mirror / upstream-TLS settings. Releases the least-conn inflight slot
/// when the URL is malformed — `logging()` will not run for this request.
fn build_proxy_upstream(
    chosen_url: &str,
    strip: Option<String>,
    opts: &RouteOptions<'_>,
    is_least_conn: bool,
    upstream_health: &UpstreamRegistry,
) -> Option<UpstreamTarget> {
    match url_to_proxy_upstream(chosen_url, strip) {
        Some(UpstreamTarget::Proxy {
            addr,
            tls,
            sni,
            strip_prefix,
            ..
        }) => Some(UpstreamTarget::Proxy {
            addr,
            tls,
            sni,
            strip_prefix,
            rewrite: opts.rewrite.map(<[_]>::to_vec),
            mirror_url: opts.mirror.map(str::to_owned),
            upstream_tls: opts.upstream_tls.cloned(),
        }),
        Some(other) => Some(other),
        None => {
            if is_least_conn {
                upstream_health.conn_dec(chosen_url);
            }
            None
        }
    }
}

/// HMAC-signed sticky cookie to set on the response, when `sticky.secret` is set.
fn make_sticky_cookie(sticky: Option<&StickyConfig>, chosen_url: &str) -> Option<(String, String)> {
    let cfg = sticky?;
    let secret = cfg.secret.as_deref()?;
    let signed = hmac_sign_sticky(chosen_url, secret);
    Some((cfg.cookie.clone(), signed))
}

/// 503 — circuit open / sticky strict reject.
fn overloaded() -> RouteResult {
    RouteResolution::local(UpstreamTarget::Local(LocalHandler::Overloaded))
}

/// Two-level load balancing: pick a group via `group_strategy`, then pick a
/// target within the group using each group's own `strategy`.
///
/// Group selection keys:
/// - `hash_key = "ip"` → hash client IP across groups (sticky per client)
/// - `hash_key = "url"` → hash request path across groups
/// - Other strategies (round-robin, random, least-conn, …) work as usual.
fn resolve_grouped(
    group_strategy: Option<&LoadBalanceStrategy>,
    groups: &[UpstreamGroup],
    route_key: &str,
    ctx: &ProxyCtx<'_>,
    opts: &RouteOptions<'_>,
) -> Option<RouteResult> {
    if groups.is_empty() {
        return None;
    }

    // Outer pick: choose which group handles this request. Group selection
    // itself is not capacity-aware — see the inner pick below for that.
    let group_key = format!("{route_key}__group");
    let hash_input = if opts.hash_key == "url" || ctx.client_ip.is_empty() {
        ctx.path
    } else {
        ctx.client_ip
    };
    let hash_val = upstream::fnv1a_hash(hash_input);

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
        .map(|(name, _, _)| name)?
    };

    let group = groups.iter().find(|g| g.name == picked_name)?;

    // Inner pick: choose a target within the selected group.
    let all_urls: Vec<String> = group
        .targets
        .iter()
        .map(|t| match t {
            crate::config::schema::ProxyTarget::Simple(u) => u.clone(),
            crate::config::schema::ProxyTarget::Weighted(w) => w.url.clone(),
        })
        .collect();
    let weighted: Vec<(String, u32)> = group
        .targets
        .iter()
        .map(|t| match t {
            crate::config::schema::ProxyTarget::Simple(u) => (u.clone(), 1u32),
            crate::config::schema::ProxyTarget::Weighted(w) => (w.url.clone(), w.weight),
        })
        .collect();

    let healthy = ctx.upstream_health.filter_healthy(&all_urls);
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
        // Explicit, not `?` — capacity exhaustion is a 503, not "no route
        // matched"; letting it propagate as `None` here would fall all the
        // way through to a generic Fallback instead.
        return Some(overloaded());
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
    let (chosen_url, is_least_conn) = capacity::pick_bounded(&inner_input)?;
    let retry_state: Option<RetryState> = None; // groups don't support retry in V1

    // Parse BEFORE acquiring the circuit_tracking slot below — matches the
    // flat-route path's ordering (#156) so a malformed URL can never leak a
    // slot nothing will release.
    let strip = opts
        .strip_prefix
        .then(|| route_key.trim_end_matches('/').to_string());
    let upstream = match url_to_proxy_upstream(&chosen_url, strip) {
        Some(UpstreamTarget::Proxy {
            addr,
            tls,
            sni,
            strip_prefix,
            ..
        }) => UpstreamTarget::Proxy {
            addr,
            tls,
            sni,
            strip_prefix,
            rewrite: opts.rewrite.map(<[_]>::to_vec),
            mirror_url: None, // groups don't support mirror in V1
            upstream_tls: None,
        },
        Some(other) => other,
        None => {
            if is_least_conn {
                ctx.upstream_health.conn_dec(&chosen_url);
            }
            return None;
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
    Some(RouteResolution {
        upstream,
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
    })
}

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
/// Strategy dispatch is delegated to [`crate::proxy::strategy`] — to add a new
/// load-balancing strategy, implement [`crate::proxy::strategy::LoadBalancingStrategy`]
/// there and map it in `strategy::from_config`. This function does not need to change.
fn pick_url_by_strategy(
    urls: &[String],
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
    retry_cfg: Option<&RetryConfig>,
    strategy: Option<&LoadBalanceStrategy>,
    upstream_health: &UpstreamRegistry,
    hash_ctx: &HashCtx<'_>,
) -> Option<(String, Option<RetryState>, bool)> {
    // With retry configured, always use round-robin rotation regardless of strategy.
    if let Some(retry) = retry_cfg {
        let (url, state) = pick_with_retry(urls, route_key, counters, retry)?;
        return Some((url, Some(state), false));
    }

    let s =
        crate::proxy::strategy::from_config(strategy.unwrap_or(&LoadBalanceStrategy::RoundRobin));
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

/// Convert a target URL + optional strip prefix into an `UpstreamTarget::Proxy`.
pub fn url_to_proxy_upstream(url: &str, strip_prefix: Option<String>) -> Option<UpstreamTarget> {
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
        rewrite: None,
        mirror_url: None,
        upstream_tls: None,
    })
}

/// Pick a starting URL and build retry state, rotating the URL list so that
/// `upstream_peer()` can walk it on each attempt.
fn pick_with_retry(
    urls: &[String],
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
        backoff_jitter: retry.backoff_jitter.unwrap_or(false),
        budget_percent: retry.budget_percent,
        is_retrying: false,
    };
    Some((first, state))
}

/// Extract the value of a named cookie from the `Cookie` request header.
///
/// Returns `None` when the cookie is absent or the header cannot be parsed.
fn extract_cookie(headers: &http::HeaderMap, name: &str) -> Option<String> {
    let cookie_hdr = headers.get("cookie")?.to_str().ok()?;
    for pair in cookie_hdr.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

// ── Sticky-session HMAC helpers ───────────────────────────────────────────────

/// Compute `HMAC-SHA256(upstream_url, secret)` and return it as URL-safe base64
/// (no padding).  Used for both signing response cookies and verifying requests.
pub(crate) fn hmac_sign_sticky(upstream_url: &str, secret: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(upstream_url.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// Return `true` when `cookie_value` is the valid HMAC of `upstream_url` with
/// the given `secret`.  Uses constant-time comparison to prevent timing attacks.
pub(crate) fn hmac_verify_sticky(upstream_url: &str, cookie_value: &str, secret: &str) -> bool {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use subtle::ConstantTimeEq as _;

    let expected = hmac_sign_sticky(upstream_url, secret);
    let Ok(expected_bytes) = URL_SAFE_NO_PAD.decode(&expected) else {
        return false;
    };
    let Ok(actual_bytes) = URL_SAFE_NO_PAD.decode(cookie_value) else {
        return false;
    };
    expected_bytes.len() == actual_bytes.len() && expected_bytes.ct_eq(&actual_bytes).into()
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

/// Find the per-route rate-limit config for the given request path.
///
/// Returns `(RateLimitConfig, route_key)` when the matched proxy route has a
/// `rateLimit` block.  The `route_key` is prepended to the bucket key so that
/// per-route buckets don't clash with site-level buckets.
///
/// Returns `None` when:
/// - The site has no `proxy` config
/// - The matched route has no `rateLimit` block
/// - The route is not a `Full(ProxyRouteConfig)` variant
pub fn find_route_rate_limit(
    site: &SiteConfig,
    path: &str,
) -> Option<(crate::config::schema::RateLimitConfig, String)> {
    use crate::config::schema::ProxyConfig;
    if let Some(ProxyConfig::Routes(routes)) = &site.proxy {
        if let Some((route_key, ProxyRouteTarget::Full(cfg))) = find_route(routes, path) {
            if let Some(rl) = &cfg.rate_limit {
                return Some((rl.clone(), route_key.to_owned()));
            }
        }
    }
    None
}

/// Return the effective priority for the matched route, if any.
///
/// Returns `None` when:
/// - The site has no `routes` proxy block
/// - No route matches `path`
/// - The matched route has no `priority` field
pub fn find_route_priority(site: &SiteConfig, path: &str) -> Option<u8> {
    use crate::config::schema::ProxyConfig;
    if let Some(ProxyConfig::Routes(routes)) = &site.proxy {
        if let Some((_, ProxyRouteTarget::Full(cfg))) = find_route(routes, path) {
            return cfg.priority;
        }
    }
    None
}

/// Parse an RFC 9218 `Priority:` header value and convert to Conduit's 0–100 scale.
///
/// RFC 9218 format: `u=<urgency>[,i]` where urgency is 0 (highest) to 7 (lowest).
/// Mapping: `priority = 100 - urgency * 14`
///
/// Returns `None` if the header is absent, malformed, or the urgency is out of range.
///
/// ```
/// # use conduit::proxy::router::parse_rfc9218_priority;
/// assert_eq!(parse_rfc9218_priority("u=0"), Some(100)); // highest urgency
/// assert_eq!(parse_rfc9218_priority("u=3"), Some(58));  // default urgency
/// assert_eq!(parse_rfc9218_priority("u=7"), Some(2));   // lowest urgency
/// assert_eq!(parse_rfc9218_priority("u=7,i"), Some(2)); // incremental flag ignored
/// ```
pub fn parse_rfc9218_priority(header: &str) -> Option<u8> {
    // Find the `u=<N>` token; other directives (e.g. `i`) are ignored.
    for token in header.split(',') {
        let token = token.trim();
        if let Some(rest) = token.strip_prefix("u=") {
            // Strip any structured-field parameters (e.g. `u=3;foo=bar` → "3").
            let val_str = rest.split(';').next().unwrap_or("").trim();
            if let Ok(urgency) = val_str.parse::<u8>() {
                if urgency <= 7 {
                    return Some(100u8.saturating_sub(urgency * 14));
                }
            }
        }
    }
    None
}

/// Extracted into `crates/conduit-static` (issue #114/#139) — this is a
/// facade re-export so `crate::proxy::router::resolve_static_roots` keeps
/// resolving to the same function at the same location for every existing
/// call site (`match_static_or_fallback` above, `routes.rs`'s
/// `route_to_result`). Its unit tests moved with it — see
/// `crates/conduit-static/src/roots.rs`.
#[cfg(feature = "static")]
pub use conduit_static::roots::resolve_static_roots;

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

/// Returns `true` when the path targets the SSE hot-reload endpoint
/// (`/__hot-reload__`) and the site has `hotReload` enabled.
///
/// Only matches when compiled with `--features hotreload` — without it, no
/// hot-reload handler exists to serve this path, so it must not win routing
/// precedence over the site's own `fallback`/`static`/`proxy` config (same
/// bug class as issue #341's ACME-challenge fix: previously matched
/// unconditionally whenever `hotReload` was configured, regardless of the
/// compiled feature — `HandlerKind::HotReloadSse`'s handler being `None`
/// without `hotreload` meant every request to this path would have fallen
/// through to Pingora's proxy path with no real upstream to select).
#[cfg(feature = "hotreload")]
fn is_hot_reload_sse_path(site: Option<&SiteConfig>, path: &str) -> bool {
    use crate::config::schema::HotReloadConfig;
    let Some(site) = site else { return false };
    let Some(hr) = &site.hot_reload else {
        return false;
    };
    if matches!(hr, HotReloadConfig::Enabled(false)) {
        return false;
    }
    let bare = path.split('?').next().unwrap_or(path);
    bare == "/__hot-reload__"
}

#[cfg(not(feature = "hotreload"))]
fn is_hot_reload_sse_path(_site: Option<&SiteConfig>, _path: &str) -> bool {
    false
}

/// Returns `true` when the path targets the hot-reload client JS file
/// (`/__hot-reload__/client.js`) and the site has `hotReload` enabled.
///
/// Only matches when compiled with `--features hotreload` — see
/// `is_hot_reload_sse_path`'s doc comment.
#[cfg(feature = "hotreload")]
fn is_hot_reload_js_path(site: Option<&SiteConfig>, path: &str) -> bool {
    use crate::config::schema::HotReloadConfig;
    let Some(site) = site else { return false };
    let Some(hr) = &site.hot_reload else {
        return false;
    };
    if matches!(hr, HotReloadConfig::Enabled(false)) {
        return false;
    }
    let bare = path.split('?').next().unwrap_or(path);
    bare == "/__hot-reload__/client.js"
}

#[cfg(not(feature = "hotreload"))]
fn is_hot_reload_js_path(_site: Option<&SiteConfig>, _path: &str) -> bool {
    false
}

/// If `path` starts with the ACME HTTP-01 challenge prefix, return the token
/// portion.  E.g. `/.well-known/acme-challenge/abc123` → `Some("abc123")`.
///
/// Only matches when compiled with `--features acme` — without it, no ACME
/// challenge handler exists to serve this path, so it must not win routing
/// precedence over the site's own `fallback`/`static`/`proxy` config
/// (issue #341: previously matched unconditionally regardless of the
/// feature, and `HandlerKind::AcmeChallenge`'s handler being `None` without
/// `acme` meant every request to this path fell through to Pingora's proxy
/// path with no real upstream to select, surfacing as a 502 instead of
/// whatever the site would otherwise have served).
#[cfg(feature = "acme")]
fn acme_challenge_token(path: &str) -> Option<&str> {
    path.strip_prefix("/.well-known/acme-challenge/")
}

#[cfg(not(feature = "acme"))]
fn acme_challenge_token(_path: &str) -> Option<&str> {
    None
}

fn find_site_idx(config: &AppConfig, host: &str, server_port: u16) -> Option<usize> {
    if config.sites.is_empty() {
        return None;
    }
    // 1st pass: sites with an explicit matching host.
    if let Some(idx) = find_host_match(&config.sites, host, server_port) {
        return Some(idx);
    }
    // 2nd pass: catch-all sites (no host configured, or host == "*").
    if let Some(idx) = find_wildcard_match(&config.sites, server_port) {
        return Some(idx);
    }
    Some(0)
}

/// Find the first site whose `host` equals `host`, preferring an exact port match.
fn find_host_match(
    sites: &[crate::config::schema::SiteConfig],
    host: &str,
    server_port: u16,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, site) in sites.iter().enumerate() {
        if site.host.as_deref() == Some(host) {
            if site.port == Some(server_port) {
                return Some(i); // exact host+port match
            }
            best.get_or_insert(i);
        }
    }
    best
}

/// Find the first catch-all site (no host or `host == "*"`), preferring port match.
fn find_wildcard_match(
    sites: &[crate::config::schema::SiteConfig],
    server_port: u16,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, site) in sites.iter().enumerate() {
        if matches!(site.host.as_deref(), None | Some("*")) {
            if site.port == Some(server_port) {
                return Some(i);
            }
            best.get_or_insert(i);
        }
    }
    best
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

    // find_best_mapped_prefix() (private helper of resolve_static_roots) and
    // its own unit tests moved to crates/conduit-static/src/roots.rs
    // (issue #114/#139) — equivalent coverage now lives in that module's
    // `static_roots_mapped_matches_prefix`/`static_roots_mapped_no_match_returns_empty`.

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
        assert!(find_site_idx(&AppConfig::default(), "example.com", 80).is_none());
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
        assert_eq!(find_site_idx(&config, "example.com", 80), Some(1));
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
        assert_eq!(find_site_idx(&config, "other.com", 80), Some(1));
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
            budget_percent: None,
            backoff_jitter: None,
        };
        let (url, state) = pick_with_retry(&urls, "r", &counters, &retry).unwrap();
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
            budget_percent: None,
            backoff_jitter: None,
        };
        let (url, state) = pick_with_retry(&urls, "r", &counters, &retry).unwrap();
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
            budget_percent: None,
            backoff_jitter: None,
        };
        assert!(pick_with_retry(&[], "r", &counters, &retry).is_none());
    }

    // ── pick_url_by_strategy ──────────────────────────────────────────────────

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
        let hash_val = upstream::fnv1a_hash("1.2.3.4");
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

    // ── url_to_proxy_upstream ─────────────────────────────────────────────────

    #[test]
    fn proxy_upstream_http_url() {
        let target = url_to_proxy_upstream("http://backend:4000", None).unwrap();
        match target {
            UpstreamTarget::Proxy {
                addr,
                tls,
                sni,
                strip_prefix,
                ..
            } => {
                assert_eq!(addr, "backend:4000");
                assert!(!tls);
                assert!(sni.is_empty());
                assert!(strip_prefix.is_none());
            }
            _ => panic!("expected Proxy variant"),
        }
    }

    #[test]
    fn proxy_upstream_https_url_sets_tls_and_sni() {
        let target = url_to_proxy_upstream("https://api.example.com:443", None).unwrap();
        match target {
            UpstreamTarget::Proxy { addr, tls, sni, .. } => {
                assert_eq!(addr, "api.example.com:443");
                assert!(tls);
                assert_eq!(sni, "api.example.com");
            }
            _ => panic!("expected Proxy variant"),
        }
    }

    #[test]
    fn proxy_upstream_with_strip_prefix() {
        let target =
            url_to_proxy_upstream("http://backend:4000", Some("/api".to_string())).unwrap();
        match target {
            UpstreamTarget::Proxy { strip_prefix, .. } => {
                assert_eq!(strip_prefix, Some("/api".to_string()));
            }
            _ => panic!("expected Proxy variant"),
        }
    }

    // resolve_static_roots()'s own unit tests moved to
    // crates/conduit-static/src/roots.rs alongside the function itself
    // (issue #114/#139).

    // ── route_request (integration of all routing logic) ─────────────────────

    #[test]
    fn route_request_health_path() {
        let config = AppConfig {
            sites: vec![SiteConfig {
                health_check: Some(HealthCheckConfig::Enabled(true)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/__health__",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(matches!(
            ctx.upstream,
            UpstreamTarget::Local(LocalHandler::Health)
        ));
    }

    #[test]
    #[cfg(feature = "static")]
    fn route_request_static_file() {
        use crate::config::schema::StaticConfig;
        let config = AppConfig {
            sites: vec![SiteConfig {
                static_files: Some(StaticConfig::Single("./dist".to_string())),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/index.html",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(matches!(
            ctx.upstream,
            UpstreamTarget::Local(LocalHandler::StaticFile { .. })
        ));
    }

    #[test]
    fn route_request_proxy_single() {
        use crate::config::schema::ProxyConfig;
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Single("http://backend:4000".to_string())),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/api/data",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(matches!(ctx.upstream, UpstreamTarget::Proxy { .. }));
    }

    #[test]
    fn route_request_no_config_returns_fallback() {
        let config = AppConfig::default();
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(matches!(
            ctx.upstream,
            UpstreamTarget::Local(LocalHandler::Fallback)
        ));
    }

    #[test]
    fn route_request_metrics_path() {
        use crate::config::schema::MetricsConfig;
        let config = AppConfig {
            sites: vec![SiteConfig {
                metrics: Some(MetricsConfig {
                    path: Some("/__metrics__".to_string()),
                    token: Some("tok".to_string()),
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/__metrics__",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(matches!(
            ctx.upstream,
            UpstreamTarget::Local(LocalHandler::Metrics { .. })
        ));
    }

    #[test]
    fn hash_falls_back_to_path_when_client_ip_empty() {
        // When client_ip is empty the hash must be computed from path so that
        // multiple requests without a resolvable IP still distribute across
        // upstreams rather than all mapping to the same bucket.
        use crate::config::schema::ProxyConfig;
        use indexmap::IndexMap;

        let mut routes = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(crate::config::schema::ProxyRouteConfig {
                targets: vec![
                    crate::config::schema::ProxyTarget::Simple("http://a:4000".to_string()),
                    crate::config::schema::ProxyTarget::Simple("http://b:4000".to_string()),
                ],
                strategy: Some(LoadBalanceStrategy::IpHash),
                hash_key: Some("ip".to_string()),
                ..Default::default()
            })),
        );

        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();

        // Two different paths with empty client_ip should potentially land on
        // different upstreams (demonstrating that path is used, not a fixed "").
        let ctx_a = route_request(
            &config,
            "localhost",
            "/page-a",
            "GET",
            &http::HeaderMap::new(),
            None,
            "",
            80,
            &counters,
            &reg,
            None,
        );
        let ctx_b = route_request(
            &config,
            "localhost",
            "/page-b",
            "GET",
            &http::HeaderMap::new(),
            None,
            "",
            80,
            &counters,
            &reg,
            None,
        );
        // Both should be routed to a Proxy target (not fallback).
        assert!(
            matches!(ctx_a.upstream, UpstreamTarget::Proxy { .. }),
            "empty client_ip should still route to a proxy target"
        );
        assert!(
            matches!(ctx_b.upstream, UpstreamTarget::Proxy { .. }),
            "empty client_ip should still route to a proxy target"
        );
    }

    // ── runtime override integration ──────────────────────────────────────────

    #[test]
    fn override_replaces_config_targets() {
        use crate::config::schema::ProxyConfig;
        use indexmap::IndexMap;

        let mut routes = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Url("http://config-target:4000".to_string()),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();

        // Without an override → config target is used.
        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "1.2.3.4",
            80,
            &counters,
            &reg,
            None,
        );
        let addr_config = match &ctx.upstream {
            UpstreamTarget::Proxy { addr, .. } => addr.clone(),
            other => panic!("expected Proxy, got {other:?}"),
        };
        assert!(
            addr_config.contains("config-target"),
            "before override, config target must be used"
        );

        // Apply an override.
        reg.add_upstream("*", "/", "http://override-target:9000", 1);

        let ctx2 = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "1.2.3.4",
            80,
            &counters,
            &reg,
            None,
        );
        let addr_override = match &ctx2.upstream {
            UpstreamTarget::Proxy { addr, .. } => addr.clone(),
            other => panic!("expected Proxy, got {other:?}"),
        };
        assert!(
            addr_override.contains("override-target"),
            "after override, runtime target must be used: got {addr_override}"
        );
    }

    // ── parse_rfc9218_priority ────────────────────────────────────────────────

    #[test]
    fn rfc9218_highest_urgency_maps_to_100() {
        assert_eq!(parse_rfc9218_priority("u=0"), Some(100));
    }

    #[test]
    fn rfc9218_default_urgency_maps_to_58() {
        assert_eq!(parse_rfc9218_priority("u=3"), Some(58));
    }

    #[test]
    fn rfc9218_lowest_urgency_maps_to_2() {
        assert_eq!(parse_rfc9218_priority("u=7"), Some(2));
    }

    #[test]
    fn rfc9218_incremental_flag_ignored() {
        assert_eq!(parse_rfc9218_priority("u=7,i"), Some(2));
        assert_eq!(parse_rfc9218_priority("u=1, i"), Some(86));
    }

    #[test]
    fn rfc9218_out_of_range_returns_none() {
        assert_eq!(parse_rfc9218_priority("u=8"), None);
        assert_eq!(parse_rfc9218_priority("u=255"), None);
    }

    #[test]
    fn rfc9218_malformed_returns_none() {
        assert_eq!(parse_rfc9218_priority(""), None);
        assert_eq!(parse_rfc9218_priority("i"), None);
        assert_eq!(parse_rfc9218_priority("u=abc"), None);
    }

    // ── find_route_priority ───────────────────────────────────────────────────

    fn make_priority_site(path: &str, priority: u8) -> SiteConfig {
        use crate::config::schema::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget};
        let mut routes = indexmap::IndexMap::new();
        let mut cfg = ProxyRouteConfig {
            targets: vec![],
            ..Default::default()
        };
        cfg.priority = Some(priority);
        routes.insert(path.to_string(), ProxyRouteTarget::Full(Box::new(cfg)));
        SiteConfig {
            proxy: Some(ProxyConfig::Routes(routes)),
            ..Default::default()
        }
    }

    #[test]
    fn find_route_priority_returns_configured_value() {
        let site = make_priority_site("/api", 80);
        assert_eq!(find_route_priority(&site, "/api/users"), Some(80));
    }

    #[test]
    fn find_route_priority_returns_none_when_not_set() {
        use crate::config::schema::{ProxyConfig, ProxyRouteTarget};
        let mut routes = indexmap::IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Url("http://u:4000".to_string()),
        );
        let site = SiteConfig {
            proxy: Some(ProxyConfig::Routes(routes)),
            ..Default::default()
        };
        assert!(find_route_priority(&site, "/").is_none());
    }

    #[test]
    fn find_route_priority_no_match_returns_none() {
        let site = make_priority_site("/api", 80);
        // Path does not start with /api → no match → None.
        assert!(find_route_priority(&site, "/other").is_none());
    }

    #[test]
    fn find_route_priority_low_priority_is_zero() {
        let site = make_priority_site("/batch", 0);
        assert_eq!(find_route_priority(&site, "/batch/jobs"), Some(0));
    }

    #[test]
    fn empty_override_falls_through_to_fallback() {
        // When the override list is explicitly empty, no URL can be selected
        // and the request should fall through to the Fallback handler.
        use crate::config::schema::ProxyConfig;
        use indexmap::IndexMap;

        let mut routes = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Url("http://config-target:4000".to_string()),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();

        // Create an override, then remove the only entry → empty list.
        reg.add_upstream("*", "/", "http://temp:4000", 1);
        reg.remove_upstream("*", "/", "http://temp:4000");

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "1.2.3.4",
            80,
            &counters,
            &reg,
            None,
        );
        // Empty override list → no URL → falls through to Fallback (not Proxy).
        assert!(
            matches!(ctx.upstream, UpstreamTarget::Local(LocalHandler::Fallback)),
            "empty override must yield Fallback, got {:?}",
            ctx.upstream
        );
    }

    // ── extract_cookie ────────────────────────────────────────────────────────

    #[test]
    fn extract_cookie_found() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "session=abc123; lang=en".parse().unwrap());
        assert_eq!(extract_cookie(&hdrs, "session"), Some("abc123".to_owned()));
    }

    #[test]
    fn extract_cookie_second_value() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "a=1; b=2; c=3".parse().unwrap());
        assert_eq!(extract_cookie(&hdrs, "b"), Some("2".to_owned()));
        assert_eq!(extract_cookie(&hdrs, "c"), Some("3".to_owned()));
    }

    #[test]
    fn extract_cookie_missing_returns_none() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "a=1; b=2".parse().unwrap());
        assert!(extract_cookie(&hdrs, "missing").is_none());
    }

    #[test]
    fn extract_cookie_no_cookie_header_returns_none() {
        let hdrs = http::HeaderMap::new();
        assert!(extract_cookie(&hdrs, "session").is_none());
    }

    #[test]
    fn extract_cookie_strips_whitespace() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "key = value ".parse().unwrap());
        assert_eq!(extract_cookie(&hdrs, "key"), Some("value".to_owned()));
    }

    // ── acme_challenge_token ──────────────────────────────────────────────────

    #[test]
    #[cfg(feature = "acme")]
    fn acme_challenge_token_extracts_token() {
        assert_eq!(
            acme_challenge_token("/.well-known/acme-challenge/abc123"),
            Some("abc123")
        );
    }

    #[test]
    fn acme_challenge_token_none_for_other_paths() {
        assert!(acme_challenge_token("/").is_none());
        assert!(acme_challenge_token("/__health__").is_none());
        assert!(acme_challenge_token("/.well-known/other").is_none());
    }

    #[test]
    #[cfg(feature = "acme")]
    fn acme_challenge_token_empty_token() {
        // Edge case: empty token after the challenge prefix.
        let result = acme_challenge_token("/.well-known/acme-challenge/");
        assert_eq!(result, Some(""));
    }

    #[test]
    #[cfg(not(feature = "acme"))]
    fn acme_challenge_token_always_none_without_feature() {
        // Issue #341: without `acme`, this path must never win routing
        // precedence — no matter how well-formed the challenge path is.
        assert!(acme_challenge_token("/.well-known/acme-challenge/abc123").is_none());
        assert!(acme_challenge_token("/.well-known/acme-challenge/").is_none());
    }

    // ── is_hot_reload_sse_path and is_hot_reload_js_path ─────────────────────

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_sse_path_when_enabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(is_hot_reload_sse_path(Some(&site), "/__hot-reload__"));
        assert!(!is_hot_reload_sse_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
        assert!(!is_hot_reload_sse_path(Some(&site), "/other"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_sse_path_when_disabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(false)),
            ..Default::default()
        };
        assert!(!is_hot_reload_sse_path(Some(&site), "/__hot-reload__"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_sse_path_no_site_returns_false() {
        assert!(!is_hot_reload_sse_path(None, "/__hot-reload__"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_js_path_when_enabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
        assert!(!is_hot_reload_js_path(Some(&site), "/__hot-reload__"));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_js_path_when_disabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(false)),
            ..Default::default()
        };
        assert!(!is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn hot_reload_js_path_with_query_string() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        // Query string should be stripped before comparison.
        assert!(is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js?v=123"
        ));
    }

    #[test]
    #[cfg(not(feature = "hotreload"))]
    fn hot_reload_paths_always_false_without_feature() {
        // Issue #341's fix class, applied here: without `hotreload`, these
        // paths must never win routing precedence — no matter how the site
        // configures `hotReload`.
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(true)),
            ..Default::default()
        };
        assert!(!is_hot_reload_sse_path(Some(&site), "/__hot-reload__"));
        assert!(!is_hot_reload_js_path(
            Some(&site),
            "/__hot-reload__/client.js"
        ));
    }

    // ── find_route_rate_limit ─────────────────────────────────────────────────

    #[test]
    fn find_route_rate_limit_returns_none_when_no_proxy() {
        let site = SiteConfig::default();
        assert!(find_route_rate_limit(&site, "/api").is_none());
    }

    #[test]
    fn find_route_rate_limit_returns_rl_when_configured() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RateLimitConfig,
        };
        use indexmap::IndexMap;
        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/api".to_owned(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b:4000".to_owned())],
                rate_limit: Some(RateLimitConfig {
                    limit: 100,
                    window_secs: 60,
                    burst: None,
                    key_by: None,
                    skip_paths: None,
                    dry_run: None,
                    store: None,
                    algorithm: None,
                }),
                ..Default::default()
            })),
        );
        let site = SiteConfig {
            proxy: Some(ProxyConfig::Routes(routes)),
            ..Default::default()
        };
        let result = find_route_rate_limit(&site, "/api/users");
        assert!(result.is_some(), "rate limit must be found for /api prefix");
        let (rl, key) = result.unwrap();
        assert_eq!(rl.limit, 100);
        assert!(key.contains("api"), "route key must contain 'api': {key}");
    }

    // ── match_static_or_fallback ──────────────────────────────────────────────

    #[test]
    #[cfg(feature = "static")]
    fn static_site_returns_static_file_handler() {
        use crate::config::schema::StaticConfig;
        let site = SiteConfig {
            static_files: Some(StaticConfig::Single("./dist".to_owned())),
            ..Default::default()
        };
        let result = match_static_or_fallback(&site, "/index.html");
        assert!(
            matches!(
                result.upstream,
                UpstreamTarget::Local(LocalHandler::StaticFile { .. })
            ),
            "static site must return StaticFile handler"
        );
    }

    #[test]
    fn no_static_returns_fallback() {
        let site = SiteConfig::default();
        let result = match_static_or_fallback(&site, "/");
        assert!(
            matches!(
                result.upstream,
                UpstreamTarget::Local(LocalHandler::Fallback)
            ),
            "site without static must return Fallback handler"
        );
    }

    // ── grouped upstream routing ──────────────────────────────────────────────

    #[test]
    fn route_with_upstream_groups() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, UpstreamGroup,
        };
        use indexmap::IndexMap;

        let groups = vec![
            UpstreamGroup {
                name: "group-a".to_owned(),
                targets: vec![ProxyTarget::Simple("http://a1:4000".to_owned())],
                strategy: None,
            },
            UpstreamGroup {
                name: "group-b".to_owned(),
                targets: vec![ProxyTarget::Simple("http://b1:4000".to_owned())],
                strategy: None,
            },
        ];
        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                groups: Some(groups),
                targets: vec![], // groups override targets
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        // Must route to one of the group backends.
        match &ctx.upstream {
            UpstreamTarget::Proxy { addr, .. } => {
                assert!(
                    addr == "a1:4000" || addr == "b1:4000",
                    "must pick from one of the groups: {addr}"
                );
            }
            other => panic!("expected Proxy, got {:?}", other),
        }
    }

    // ── circuit breaker: all upstreams at max connections ────────────────────

    #[test]
    fn circuit_breaker_returns_overloaded_when_all_at_max() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, UpstreamHealthCheck,
        };
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://backend:4000".to_owned())],
                health_check: Some(UpstreamHealthCheck {
                    max_connections_per_upstream: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        // Fill the connection slot (count = 1 = max).
        reg.conn_inc("http://backend:4000");

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        // Circuit breaker: all upstreams at max → Overloaded.
        assert!(
            matches!(
                ctx.upstream,
                UpstreamTarget::Local(LocalHandler::Overloaded)
            ),
            "circuit breaker must return Overloaded when all at max: {:?}",
            ctx.upstream
        );
    }

    #[test]
    fn circuit_breaker_round_robin_skips_at_capacity_peer() {
        // #156: before the fix, RoundRobin never checked conn_load, so a
        // saturated peer would still receive traffic as long as it was
        // "healthy" in the round-robin rotation. Saturate peer A at its cap
        // and assert every pick lands on B.
        let config = single_route_config(
            vec!["http://a:4000", "http://b:4000"],
            None, // default RoundRobin
            Some(1),
        );
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        reg.conn_inc("http://a:4000"); // saturate A at the cap

        for _ in 0..5 {
            let ctx = route_request(
                &config,
                "localhost",
                "/",
                "GET",
                &http::HeaderMap::new(),
                None,
                "127.0.0.1",
                80,
                &counters,
                &reg,
                None,
            );
            match &ctx.upstream {
                UpstreamTarget::Proxy { addr, .. } => {
                    assert_eq!(addr, "b:4000", "must never pick the saturated peer");
                }
                other => panic!("expected Proxy, got {other:?}"),
            }
            // circuit_tracking acquired a real conn_count slot for B (since
            // RoundRobin isn't least-conn); release it to simulate the
            // request completing, matching what logging() does in production
            // — otherwise B would itself saturate after the first iteration.
            assert!(
                ctx.upstream_conn_slot,
                "round-robin with a cap set must acquire a slot via circuit_tracking"
            );
            reg.conn_dec("http://b:4000");
        }
    }

    #[test]
    fn circuit_breaker_hash_strategy_preserves_affinity_while_one_peer_capped() {
        // #156: naive filtering before a hash pick would remap most clients
        // whenever any single peer saturates. Forward-probing must keep an
        // under-cap client's mapping unchanged, and relocate deterministically
        // only for the client(s) whose preferred peer is at capacity.
        let config = single_route_config(
            vec!["http://a:4000", "http://b:4000", "http://c:4000"],
            Some(LoadBalanceStrategy::IpHash),
            Some(1),
        );
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();

        // circuit_tracking acquires a real conn_count slot on every pick
        // (IpHash isn't least-conn, and a cap is configured); release it
        // immediately so consecutive calls simulate independent requests
        // rather than accumulating load against each other. Only the
        // explicit conn_inc/conn_dec calls around `pick()` below represent
        // "real" outstanding load for this test.
        let pick = |reg: &UpstreamRegistry| -> String {
            let ctx = route_request(
                &config,
                "localhost",
                "/",
                "GET",
                &http::HeaderMap::new(),
                None,
                "127.0.0.1",
                80,
                &counters,
                reg,
                None,
            );
            let addr = match ctx.upstream {
                UpstreamTarget::Proxy { addr, .. } => addr,
                other => panic!("expected Proxy, got {other:?}"),
            };
            if ctx.upstream_conn_slot {
                reg.conn_dec(&format!("http://{addr}"));
            }
            addr
        };

        // Learn this client's preferred peer under no load.
        let preferred = pick(&reg);
        assert_eq!(
            pick(&reg),
            preferred,
            "mapping must be stable under no load"
        );

        // Saturate a DIFFERENT peer and confirm the mapping is unaffected.
        let other = ["a:4000", "b:4000", "c:4000"]
            .into_iter()
            .find(|p| *p != preferred)
            .unwrap();
        reg.conn_inc(&format!("http://{other}"));
        assert_eq!(
            pick(&reg),
            preferred,
            "an unrelated peer saturating must not move this client's mapping"
        );
        reg.conn_dec(&format!("http://{other}"));

        // Saturate the PREFERRED peer: the client must relocate.
        reg.conn_inc(&format!("http://{preferred}"));
        let relocated = pick(&reg);
        assert_ne!(
            relocated, preferred,
            "must relocate off the now-saturated preferred peer"
        );

        // Free the slot: mapping returns to the original preference.
        reg.conn_dec(&format!("http://{preferred}"));
        assert_eq!(
            pick(&reg),
            preferred,
            "mapping must return once the preferred peer has capacity again"
        );
    }

    #[test]
    fn circuit_breaker_grouped_route_returns_overloaded_when_selected_group_saturated() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, UpstreamGroup,
            UpstreamHealthCheck,
        };
        use indexmap::IndexMap;

        let groups = vec![UpstreamGroup {
            name: "only-group".to_owned(),
            targets: vec![ProxyTarget::Simple("http://g1:4000".to_owned())],
            strategy: None,
        }];
        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                groups: Some(groups),
                targets: vec![],
                health_check: Some(UpstreamHealthCheck {
                    max_connections_per_upstream: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        reg.conn_inc("http://g1:4000"); // saturate the only target in the only group

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(
            matches!(
                ctx.upstream,
                UpstreamTarget::Local(LocalHandler::Overloaded)
            ),
            "grouped route must 503 when every target in the selected group is at capacity: {:?}",
            ctx.upstream
        );
    }

    // ── failover to backup upstream ───────────────────────────────────────────

    #[test]
    fn route_to_backup_when_all_primary_unhealthy() {
        use crate::config::schema::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget};
        use indexmap::IndexMap;

        // Set up a route with one primary and one backup.
        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://primary:4000".to_owned())],
                backup: Some("http://backup:4001".to_owned()),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };

        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        // Mark the primary as unhealthy.
        {
            let mut entry = reg
                .statuses
                .entry("http://primary:4000".to_owned())
                .or_default();
            entry.healthy = false;
        }

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        // Must route to the backup.
        match &ctx.upstream {
            UpstreamTarget::Proxy { addr, .. } => {
                assert_eq!(
                    addr, "backup:4001",
                    "must route to backup when primary is unhealthy"
                );
            }
            other => panic!("expected Proxy upstream, got {:?}", other),
        }
    }

    #[test]
    fn malformed_backup_url_falls_through_to_fallback() {
        use crate::config::schema::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget};
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://primary:4000".to_owned())],
                // `url_to_host_port` (upstream.rs) is lenient about bare hostnames
                // ("not-a-url" alone parses fine, host="not-a-url", default port) --
                // the only input it actually rejects is an empty host portion after
                // scheme-trimming, e.g. a scheme with nothing after it. Matches the
                // existing `url_to_host_port_empty_host_returns_none` unit test.
                backup: Some("http://".to_owned()),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };

        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        // Mark the primary as unhealthy so the (malformed) backup path is tried.
        {
            let mut entry = reg
                .statuses
                .entry("http://primary:4000".to_owned())
                .or_default();
            entry.healthy = false;
        }

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        // A malformed backup URL must not panic and must not silently load-balance
        // across the known-unhealthy primaries -- it falls through to
        // match_static_or_fallback (no static config here -> Fallback).
        assert!(
            matches!(ctx.upstream, UpstreamTarget::Local(LocalHandler::Fallback)),
            "malformed backup URL must fall through to Fallback, got {:?}",
            ctx.upstream
        );
    }

    // ── sticky sessions: HMAC-verified routing ────────────────────────────────

    #[test]
    fn sticky_hmac_routes_to_pinned_upstream() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, StickyConfig,
        };
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![
                    ProxyTarget::Simple("http://a:4000".to_owned()),
                    ProxyTarget::Simple("http://b:4000".to_owned()),
                ],
                sticky: Some(StickyConfig {
                    cookie: "srv_id".to_owned(),
                    secret: Some("s3cret".to_owned()),
                    strict: None,
                }),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };

        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();

        // Cookie signed specifically for "b", not "a" -- pinning must follow the
        // signature, not round-robin/hash selection.
        let signed = hmac_sign_sticky("http://b:4000", "s3cret");
        let mut headers = http::HeaderMap::new();
        headers.insert("cookie", format!("srv_id={signed}").parse().unwrap());

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &headers,
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        match &ctx.upstream {
            UpstreamTarget::Proxy { addr, .. } => {
                assert_eq!(addr, "b:4000", "must route to the HMAC-pinned upstream");
            }
            other => panic!("expected Proxy upstream, got {:?}", other),
        }
    }

    #[test]
    fn sticky_capacity_relocation_does_not_re_pin_and_self_heals() {
        // #156 review finding (Gitar): re-signing the sticky cookie to a
        // capacity-relocated fallback would permanently migrate the session
        // to it, since the next request's hash input is derived from the
        // pinned URL string itself. Prove: (1) relocating away from a
        // saturated-but-healthy pin does NOT re-sign the cookie, and (2)
        // once the original pin frees capacity, presenting the SAME
        // (unchanged) original cookie routes back to it.
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, StickyConfig,
            UpstreamHealthCheck,
        };
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![
                    ProxyTarget::Simple("http://a:4000".to_owned()),
                    ProxyTarget::Simple("http://b:4000".to_owned()),
                ],
                sticky: Some(StickyConfig {
                    cookie: "srv_id".to_owned(),
                    secret: Some("s3cret".to_owned()),
                    strict: None,
                }),
                health_check: Some(UpstreamHealthCheck {
                    max_connections_per_upstream: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };

        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();

        // Pin the cookie to "a", then saturate "a" at its cap (still
        // healthy, just at capacity) so the pick must relocate.
        let signed_a = hmac_sign_sticky("http://a:4000", "s3cret");
        let mut headers = http::HeaderMap::new();
        headers.insert("cookie", format!("srv_id={signed_a}").parse().unwrap());
        reg.conn_inc("http://a:4000");

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &headers,
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        match &ctx.upstream {
            UpstreamTarget::Proxy { addr, .. } => {
                assert_eq!(
                    addr, "b:4000",
                    "must relocate off the saturated pinned peer"
                );
            }
            other => panic!("expected Proxy upstream, got {:?}", other),
        }
        assert!(
            ctx.sticky_set_cookie.is_none(),
            "capacity relocation must NOT re-sign the cookie — doing so would \
             permanently migrate the session to the fallback peer: {:?}",
            ctx.sticky_set_cookie
        );

        // Free "a"'s slot and present the SAME original cookie again (no new
        // cookie was issued, so the client would still be holding this one).
        reg.conn_dec("http://a:4000");
        let ctx2 = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &headers,
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        match &ctx2.upstream {
            UpstreamTarget::Proxy { addr, .. } => {
                assert_eq!(
                    addr, "a:4000",
                    "must self-heal back to the original pin once capacity frees"
                );
            }
            other => panic!("expected Proxy upstream, got {:?}", other),
        }
    }

    #[test]
    fn sticky_strict_mode_returns_503_when_pinned_upstream_unhealthy() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, StickyConfig,
        };
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![
                    ProxyTarget::Simple("http://a:4000".to_owned()),
                    ProxyTarget::Simple("http://b:4000".to_owned()),
                ],
                sticky: Some(StickyConfig {
                    cookie: "srv_id".to_owned(),
                    secret: Some("s3cret".to_owned()),
                    strict: Some(true),
                }),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };

        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        // Pin the cookie to "a", then mark "a" unhealthy -- strict mode must
        // refuse rather than silently fail over to "b" (which would break the
        // session-affinity guarantee strict mode exists to provide).
        {
            let mut entry = reg.statuses.entry("http://a:4000".to_owned()).or_default();
            entry.healthy = false;
        }
        let signed = hmac_sign_sticky("http://a:4000", "s3cret");
        let mut headers = http::HeaderMap::new();
        headers.insert("cookie", format!("srv_id={signed}").parse().unwrap());

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &headers,
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(
            matches!(
                ctx.upstream,
                UpstreamTarget::Local(LocalHandler::Overloaded)
            ),
            "strict mode must return Overloaded when the pinned upstream is unhealthy: {:?}",
            ctx.upstream
        );
    }

    #[test]
    fn sticky_forged_cookie_ignored_falls_back_to_load_balancing() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, StickyConfig,
        };
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![
                    ProxyTarget::Simple("http://a:4000".to_owned()),
                    ProxyTarget::Simple("http://b:4000".to_owned()),
                ],
                sticky: Some(StickyConfig {
                    cookie: "srv_id".to_owned(),
                    secret: Some("s3cret".to_owned()),
                    strict: None,
                }),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };

        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        // A cookie that doesn't match either upstream's HMAC -- must not steer
        // routing to an attacker-chosen peer; must fall through to normal
        // load-balancing across the healthy set instead.
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "cookie",
            "srv_id=not-a-valid-hmac-signature".parse().unwrap(),
        );

        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &headers,
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        let chosen_url = match &ctx.upstream {
            UpstreamTarget::Proxy { addr, .. } => {
                assert!(
                    addr == "a:4000" || addr == "b:4000",
                    "forged cookie must still resolve to a real upstream via normal \
                     load-balancing: {addr}"
                );
                format!("http://{addr}")
            }
            other => panic!("expected Proxy upstream, got {:?}", other),
        };
        // Normal sticky-cookie-setting behavior must still apply to whichever
        // upstream was actually chosen (the forged cookie is ignored for
        // *selection*, not for the response-side re-signing) -- verify the
        // replacement cookie is both correctly named AND actually signed for
        // the upstream that was picked, not merely present. A regression that
        // set the wrong cookie name or signed for the wrong upstream must fail
        // this test.
        let (cookie_name, cookie_value) = ctx
            .sticky_set_cookie
            .as_ref()
            .expect("a fresh signed cookie must still be set for the chosen upstream");
        assert_eq!(
            cookie_name, "srv_id",
            "cookie name must match sticky.cookie"
        );
        assert!(
            hmac_verify_sticky(&chosen_url, cookie_value, "s3cret"),
            "fresh cookie must be validly signed for the actually-chosen upstream {chosen_url}"
        );
    }

    // ── proxy_upstream_url / upstream_conn_slot (#155) ────────────────────────

    /// Build a single-route AppConfig with the given targets/strategy/healthCheck.
    fn single_route_config(
        targets: Vec<&str>,
        strategy: Option<LoadBalanceStrategy>,
        max_conns_per_upstream: Option<u64>,
    ) -> AppConfig {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, UpstreamHealthCheck,
        };
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: targets
                    .into_iter()
                    .map(|u| ProxyTarget::Simple(u.to_owned()))
                    .collect(),
                strategy,
                health_check: max_conns_per_upstream.map(|max| UpstreamHealthCheck {
                    max_connections_per_upstream: Some(max),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        );
        AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn round_robin_populates_url_without_conn_slot() {
        // Default strategy, no maxConnectionsPerUpstream: proxy_upstream_url
        // must still be Some (for passive-health attribution), but
        // upstream_conn_slot must be false (no conn_count slot was acquired).
        let config = single_route_config(vec!["http://a:4000", "http://b:4000"], None, None);
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(
            ctx.proxy_upstream_url.is_some(),
            "URL must be populated for passive-health attribution regardless of strategy"
        );
        assert!(
            !ctx.upstream_conn_slot,
            "round-robin without a connection cap must not claim a conn_count slot"
        );
    }

    #[test]
    fn least_conn_populates_url_with_conn_slot() {
        let config = single_route_config(
            vec!["http://a:4000", "http://b:4000"],
            Some(LoadBalanceStrategy::LeastConn),
            None,
        );
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(ctx.proxy_upstream_url.is_some());
        assert!(
            ctx.upstream_conn_slot,
            "least-conn always acquires a conn_count slot"
        );
    }

    #[test]
    fn round_robin_with_max_conns_populates_url_with_conn_slot() {
        // Default strategy but maxConnectionsPerUpstream set: circuit_tracking
        // kicks in, so a slot IS acquired even though the strategy isn't least-conn.
        let config = single_route_config(vec!["http://a:4000", "http://b:4000"], None, Some(5));
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(ctx.proxy_upstream_url.is_some());
        assert!(
            ctx.upstream_conn_slot,
            "round-robin with maxConnectionsPerUpstream set must acquire a conn_count slot"
        );
    }

    #[test]
    fn attribution_only_route_does_not_corrupt_shared_conn_count() {
        // Two routes share the same target X: /lc is least-conn (acquires a
        // slot), /rr is plain round-robin (attribution only, no slot). Routing
        // /rr must NOT touch X's conn_count -- otherwise a later logging()
        // decrement for the /rr request would phantom-decrement /lc's slot.
        use crate::config::schema::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget};
        use indexmap::IndexMap;

        const SHARED: &str = "http://x:4000";
        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/lc".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple(SHARED.to_owned())],
                strategy: Some(LoadBalanceStrategy::LeastConn),
                ..Default::default()
            })),
        );
        routes.insert(
            "/rr".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple(SHARED.to_owned())],
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();

        let ctx_lc = route_request(
            &config,
            "localhost",
            "/lc",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(ctx_lc.upstream_conn_slot);
        assert_eq!(
            reg.conn_load(SHARED),
            1,
            "least-conn route must have claimed exactly one slot"
        );

        let ctx_rr = route_request(
            &config,
            "localhost",
            "/rr",
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        assert!(
            ctx_rr.proxy_upstream_url.is_some(),
            "round-robin route still gets the URL for attribution"
        );
        assert!(
            !ctx_rr.upstream_conn_slot,
            "round-robin route on a shared target must not claim a slot"
        );
        assert_eq!(
            reg.conn_load(SHARED),
            1,
            "the round-robin route must not have touched the shared conn_count"
        );
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

    // ── hmac_sign_sticky / hmac_verify_sticky (#39) ───────────────────────────

    #[test]
    fn hmac_sign_sticky_produces_non_empty_base64() {
        let signed = hmac_sign_sticky("http://backend:4000", "mysecret");
        assert!(!signed.is_empty(), "HMAC signature must not be empty");
        // URL-safe base64: only A-Z a-z 0-9 - _ (no + / =)
        assert!(
            signed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "signature must be URL-safe base64 (no +, /, or =): {signed}"
        );
    }

    #[test]
    fn hmac_sign_sticky_is_deterministic() {
        let a = hmac_sign_sticky("http://backend:4000", "s3cret");
        let b = hmac_sign_sticky("http://backend:4000", "s3cret");
        assert_eq!(a, b, "same inputs must always produce the same HMAC");
    }

    #[test]
    fn hmac_sign_sticky_differs_for_different_urls() {
        let a = hmac_sign_sticky("http://backend-a:4000", "s3cret");
        let b = hmac_sign_sticky("http://backend-b:4000", "s3cret");
        assert_ne!(a, b, "different URLs must produce different HMACs");
    }

    #[test]
    fn hmac_sign_sticky_differs_for_different_secrets() {
        let a = hmac_sign_sticky("http://backend:4000", "secret1");
        let b = hmac_sign_sticky("http://backend:4000", "secret2");
        assert_ne!(a, b, "different secrets must produce different HMACs");
    }

    #[test]
    fn hmac_verify_sticky_correct_value_returns_true() {
        let url = "http://backend:4000";
        let secret = "s3cret";
        let signed = hmac_sign_sticky(url, secret);
        assert!(
            hmac_verify_sticky(url, &signed, secret),
            "correct HMAC must verify successfully"
        );
    }

    #[test]
    fn hmac_verify_sticky_wrong_url_returns_false() {
        let secret = "s3cret";
        let signed = hmac_sign_sticky("http://backend-a:4000", secret);
        assert!(
            !hmac_verify_sticky("http://backend-b:4000", &signed, secret),
            "HMAC signed for URL-A must not verify against URL-B"
        );
    }

    #[test]
    fn hmac_verify_sticky_wrong_secret_returns_false() {
        let url = "http://backend:4000";
        let signed = hmac_sign_sticky(url, "correct-secret");
        assert!(
            !hmac_verify_sticky(url, &signed, "wrong-secret"),
            "HMAC signed with one secret must not verify with a different secret"
        );
    }

    #[test]
    fn hmac_verify_sticky_tampered_value_returns_false() {
        let url = "http://backend:4000";
        let secret = "s3cret";
        let mut signed = hmac_sign_sticky(url, secret);
        // Flip the first character to simulate tampering.
        if signed.starts_with('A') {
            signed.replace_range(0..1, "B");
        } else {
            signed.replace_range(0..1, "A");
        }
        assert!(
            !hmac_verify_sticky(url, &signed, secret),
            "tampered cookie value must not verify"
        );
    }

    #[test]
    fn hmac_verify_sticky_garbage_input_returns_false() {
        // Non-base64 input must not panic, just return false.
        assert!(
            !hmac_verify_sticky("http://backend:4000", "not!valid!base64!!!!", "s3cret"),
            "invalid base64 must return false without panicking"
        );
    }
}
