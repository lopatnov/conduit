use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
#[cfg(feature = "static")]
use std::sync::Arc;

use dashmap::DashMap;

use crate::config::schema::{AppConfig, ProxyConfig, RouteConfig, SiteConfig, StaticOptions};
use crate::proxy::ctx::{
    LocalHandler, ProxyReqState, RequestCtx, RetryState, RouteRateLimit, UpstreamTarget,
};
use crate::proxy::dispatch;
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::routes::{self, RouteMatch};
use crate::proxy::upstream;
use conduit_proxy_http::options::ProxyCtx;
use conduit_proxy_http::outcome::{ProxyOutcome, ProxyUpstream};
use conduit_proxy_http::resolve;

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
    pub proxy_timeout: Option<crate::config::schema::ProxyTimeout>,
    /// Per-route connection-pool settings.
    pub proxy_pool: Option<crate::config::schema::ConnectionPoolConfig>,
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
    pub proxy_cache_cfg: Option<crate::config::schema::CacheConfig>,
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
    /// Per-route rate limit selected during routing (#360). `None` when the
    /// matched route has no `rateLimit` configured, or for `ProxyConfig::
    /// Single`/static/fallback resolutions (`local()` below).
    pub route_rate_limit: Option<RouteRateLimit>,
    /// Effective route priority (`proxy.*.priority`) selected during routing
    /// (#360), for post-routing load-shedding. `None` when the matched route
    /// has no `priority` configured.
    pub route_priority: Option<u8>,
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
            route_rate_limit: None,
            route_priority: None,
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
    let site_idx = dispatch::find_site_idx(config, host, server_port).unwrap_or(0);
    let site = config.sites.get(site_idx);

    let res: RouteResolution = if let Some(token) = dispatch::acme_challenge_token(path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::AcmeChallenge {
            token: token.to_owned(),
        }))
    } else if dispatch::is_health_path(site, path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::Health))
    } else if let Some(token) = dispatch::metrics_token(site, path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::Metrics { token }))
    } else if dispatch::is_hot_reload_js_path(site, path) {
        RouteResolution::local(UpstreamTarget::Local(LocalHandler::HotReloadJs))
    } else if dispatch::is_hot_reload_sse_path(site, path) {
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
    let proxy = ProxyReqState {
        retry: res.retry,
        proxy_timeout: res.proxy_timeout,
        proxy_pool: res.proxy_pool,
        proxy_http2: res.proxy_http2,
        proxy_upstream_url: res.proxy_upstream_url,
        upstream_conn_slot: res.upstream_conn_slot,
        proxy_cache_cfg: res.proxy_cache_cfg,
        // Populate passive health thresholds so logging() can apply them.
        passive_unhealthy_status: res.passive_unhealthy_status,
        passive_unhealthy_latency_ms: res.passive_unhealthy_latency_ms,
        websocket_allowed: res.websocket_allowed,
        sticky_set_cookie: res.sticky_set_cookie,
        // #360: carried on the routing decision itself so enforcement
        // (`request_phase.rs::enforce_route_rate_limit`/`shed_low_priority_request`)
        // can never disagree with which mechanism (`proxy` map vs. `routes[]`)
        // actually matched.
        route_rate_limit: res.route_rate_limit,
        route_priority: res.route_priority,
        ..Default::default()
    };
    RequestCtx::new(site_idx, res.upstream, proxy, response_transform)
}

/// Route a request within a matched `SiteConfig`.
///
/// **The two proxy-matching mechanisms deliberately disagree on what happens
/// when a route matches but its target fails to resolve** ("the trap" — see
/// this PR's own description):
/// - `resolve_routes_array` (the `routes[]` array): a matched entry is a
///   TERMINAL decision — even an unresolved target maps straight to
///   `Fallback` and returns immediately, never falling through to
///   `site.proxy`/`site.static`.
/// - `resolve_legacy_proxy` (the legacy `proxy` map/shorthand): a matched
///   route whose target is unresolved FALLS THROUGH to
///   `match_static_or_fallback` (pre-existing behavior, preserved exactly) —
///   but its already-computed rate-limit/priority stamp must survive that
///   fallthrough (issue #415).
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

    if let Some(result) = resolve_routes_array(
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
        if let Some(result) = resolve_legacy_proxy(proxy_cfg, &proxy_ctx, site, path) {
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
/// A `routes[]` entry that matches — even one whose `proxy` action fails to
/// resolve a usable upstream — is a TERMINAL routing decision: this always
/// returns `Some(...)` once any entry matches, so `route_site` returns
/// immediately and never falls through to `site.proxy`/`site.static` (see
/// `route_site`'s own doc comment, "the trap", for why this intentionally
/// differs from `resolve_legacy_proxy`'s fallthrough-to-static behavior
/// below).
#[allow(clippy::too_many_arguments)]
fn resolve_routes_array(
    site: &SiteConfig,
    path: &str,
    method: &str,
    req_headers: &http::HeaderMap,
    query: Option<&str>,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
) -> Option<RouteResult> {
    let routes_cfg = site.routes.as_ref()?;
    let route_match = routes::match_routes(
        routes_cfg,
        path,
        method,
        req_headers,
        query,
        counters,
        upstream_health,
    )?;
    Some(match route_match {
        RouteMatch::Proxy { resolution, .. } => match resolution.outcome {
            ProxyOutcome::Upstream(pu) => proxy_route_result(pu, resolution.state),
            ProxyOutcome::Overloaded => overloaded_with_state(resolution.state),
            // Terminal: unlike the legacy `proxy` map (`resolve_legacy_proxy`
            // below), `routes[]` never falls through to `site.static`/
            // `site.proxy` on an unresolved target — map straight to
            // `Fallback`, the same terminal shape as a malformed `Url`/
            // `RoundRobin` shorthand target.
            ProxyOutcome::Unresolved => fallback_with_state(resolution.state),
        },
        RouteMatch::NonProxy { index } => {
            resolve_non_proxy_route(&routes_cfg[index], path, site.static_options.as_ref())
        }
    })
}

/// Resolve a `routes[]` entry with no `proxy` action: its `static` action, or
/// the global fallback.
///
/// Without the `static` feature compiled in, `route.static` is never
/// resolved into a `StaticFile` handler — see `match_static_or_fallback`'s
/// doc comment for the shared degradation rationale.
fn resolve_non_proxy_route(
    #[cfg_attr(not(feature = "static"), allow(unused_variables))] route: &RouteConfig,
    #[cfg_attr(not(feature = "static"), allow(unused_variables))] path: &str,
    #[cfg_attr(not(feature = "static"), allow(unused_variables))] static_options: Option<
        &StaticOptions,
    >,
) -> RouteResult {
    #[cfg(feature = "static")]
    if let Some(static_cfg) = &route.static_files {
        let options = Arc::new(static_options.cloned().unwrap_or_default());
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

/// Resolve `site.proxy` (legacy shorthand/map format).
///
/// Returns `None` when nothing in `config` matched this request at all
/// (`Single` with a malformed URL, or an empty/non-matching `Routes` map) —
/// the caller (`route_site`) continues to its own `match_static_or_fallback`
/// call with no state to carry, identical to today's behavior.
///
/// Unlike `resolve_routes_array` above (the `routes[]` array, which treats an
/// unresolved match as terminal — see `route_site`'s own doc comment, "the
/// trap"), a `proxy` map route that matches but fails to resolve a usable
/// upstream FALLS THROUGH to `match_static_or_fallback` here (pre-existing
/// behavior, preserved exactly) — but its already-computed rate-limit/
/// priority stamp must survive that fallthrough (issue #415), so this
/// function performs the fallthrough itself rather than handing an
/// ambiguous `None` back up to a caller that can't distinguish "no match"
/// from "matched but carries state to preserve."
fn resolve_legacy_proxy(
    config: &ProxyConfig,
    ctx: &ProxyCtx<'_>,
    site: &SiteConfig,
    path: &str,
) -> Option<RouteResult> {
    match config {
        ProxyConfig::Single(url) => url_to_proxy_upstream(url, None).map(RouteResolution::local),
        ProxyConfig::Routes(routes_map) => {
            let resolution = resolve::resolve_proxy_routes(routes_map, ctx)?;
            Some(match resolution.outcome {
                ProxyOutcome::Upstream(pu) => proxy_route_result(pu, resolution.state),
                ProxyOutcome::Overloaded => overloaded_with_state(resolution.state),
                ProxyOutcome::Unresolved => {
                    let mut result = match_static_or_fallback(site, path);
                    result.route_rate_limit = resolution.state.route_rate_limit;
                    result.route_priority = resolution.state.route_priority;
                    result
                }
            })
        }
    }
}

/// Build a `RouteResolution` for a resolved proxy target, carrying the
/// routing-time `ProxyReqState` (retry, timeout, pool, sticky cookie, rate
/// limit/priority stamp, ...) alongside it. Shared by both
/// `resolve_routes_array` and `resolve_legacy_proxy` — purely mechanical
/// struct-building with no outcome-dependent behavior, unlike the
/// `Unresolved` handling in each of those two call sites (deliberately kept
/// separate and inline — see `route_site`'s doc comment, "the trap").
fn proxy_route_result(upstream: ProxyUpstream, state: ProxyReqState) -> RouteResult {
    RouteResolution {
        upstream: UpstreamTarget::Proxy {
            addr: upstream.addr,
            tls: upstream.tls,
            sni: upstream.sni,
            strip_prefix: upstream.strip_prefix,
            rewrite: upstream.rewrite,
            mirror_url: upstream.mirror_url,
            upstream_tls: upstream.upstream_tls,
        },
        retry: state.retry,
        proxy_timeout: state.proxy_timeout,
        proxy_pool: state.proxy_pool,
        proxy_http2: state.proxy_http2,
        proxy_upstream_url: state.proxy_upstream_url,
        upstream_conn_slot: state.upstream_conn_slot,
        proxy_cache_cfg: state.proxy_cache_cfg,
        passive_unhealthy_status: state.passive_unhealthy_status,
        passive_unhealthy_latency_ms: state.passive_unhealthy_latency_ms,
        websocket_allowed: state.websocket_allowed,
        sticky_set_cookie: state.sticky_set_cookie,
        route_rate_limit: state.route_rate_limit,
        route_priority: state.route_priority,
    }
}

/// 503 — circuit open / sticky strict reject — carrying whatever rate-limit/
/// priority stamp routing had already computed (#360, #415).
fn overloaded_with_state(state: ProxyReqState) -> RouteResult {
    let mut result = RouteResolution::local(UpstreamTarget::Local(LocalHandler::Overloaded));
    result.route_rate_limit = state.route_rate_limit;
    result.route_priority = state.route_priority;
    result
}

/// Fallback (404-eligible) — carrying whatever rate-limit/priority stamp
/// routing had already computed (#360, #415).
fn fallback_with_state(state: ProxyReqState) -> RouteResult {
    let mut result = RouteResolution::local(UpstreamTarget::Local(LocalHandler::Fallback));
    result.route_rate_limit = state.route_rate_limit;
    result.route_priority = state.route_priority;
    result
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

/// Extracted into `crates/conduit-static` (issue #114/#139) — this is a
/// facade re-export so `crate::proxy::router::resolve_static_roots` keeps
/// resolving to the same function at the same location for every existing
/// call site (`resolve_non_proxy_route`/`match_static_or_fallback` above).
/// Its unit tests moved with it — see `crates/conduit-static/src/roots.rs`.
#[cfg(feature = "static")]
pub use conduit_static::roots::resolve_static_roots;

/// Re-exported so `crate::proxy::router::parse_rfc9218_priority` keeps
/// resolving for `request_phase.rs` and this crate's own doctest below.
/// `parse_rfc9218_priority` deliberately did NOT move into
/// `conduit-proxy-http` (issue #143 PR B) along with the rest of
/// `routing::dispatch` — see `crates/conduit-proxy-http/src/lib.rs`'s own
/// doc comment ("`dispatch.rs` deliberately did NOT move here") for why.
pub use crate::proxy::dispatch::parse_rfc9218_priority;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        AppConfig, HealthCheckConfig, LoadBalanceStrategy, ProxyRouteTarget, RetryConfig,
        SiteConfig,
    };
    use crate::proxy::health::UpstreamRegistry;
    use conduit_proxy_http::sticky::{hmac_sign_sticky, hmac_verify_sticky};

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

    // ── route_priority stamping (#360, formerly `find_route_priority`) ────────
    //
    // `find_route_priority` was a second, post-routing path matcher that
    // re-scanned `site.proxy` after routing already happened — deleted for
    // issue #360 (it could disagree with which mechanism actually matched).
    // These tests now assert the same behavior through the real routing
    // entry point, `route_request(...)`, reading `RequestCtx.proxy.route_priority`
    // — proving the legacy `proxy` map path stamps the identical value it
    // used to return.

    fn make_priority_site(path: &str, priority: u8) -> SiteConfig {
        use crate::config::schema::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget};
        let mut routes = indexmap::IndexMap::new();
        // A real upstream target is required: with an empty `targets` list,
        // routing itself produces no resolution at all (not even a
        // fallback/overloaded one) for this route, so there is nothing to
        // stamp `route_priority` onto — same reason `route_rate_limit`'s own
        // fixtures below always carry a real target.
        let mut cfg = ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://b:4000".to_owned())],
            ..Default::default()
        };
        cfg.priority = Some(priority);
        routes.insert(path.to_string(), ProxyRouteTarget::Full(Box::new(cfg)));
        SiteConfig {
            proxy: Some(ProxyConfig::Routes(routes)),
            ..Default::default()
        }
    }

    /// Route `path` against `site` (the sole site in a fresh `AppConfig`) and
    /// return the resulting `RequestCtx.proxy.route_priority`.
    fn route_priority_for(site: SiteConfig, path: &str) -> Option<u8> {
        let config = AppConfig {
            sites: vec![site],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        route_request(
            &config,
            "localhost",
            path,
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        )
        .proxy
        .route_priority
    }

    #[test]
    fn route_priority_returns_configured_value() {
        let site = make_priority_site("/api", 80);
        assert_eq!(route_priority_for(site, "/api/users"), Some(80));
    }

    #[test]
    fn route_priority_returns_none_when_not_set() {
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
        assert!(route_priority_for(site, "/").is_none());
    }

    #[test]
    fn route_priority_no_match_returns_none() {
        let site = make_priority_site("/api", 80);
        // Path does not start with /api → no match → falls through to
        // static/fallback, which carries no route priority.
        assert!(route_priority_for(site, "/other").is_none());
    }

    #[test]
    fn route_priority_low_priority_is_zero() {
        let site = make_priority_site("/batch", 0);
        assert_eq!(route_priority_for(site, "/batch/jobs"), Some(0));
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

    // ── route_rate_limit stamping (#360, formerly `find_route_rate_limit`) ────
    //
    // `find_route_rate_limit` was the rate-limit twin of `find_route_priority`
    // above — same deletion reason (#360). These tests assert the identical
    // `(config, route_key)` pair is now stamped onto `RequestCtx.proxy.route_rate_limit`
    // by the real routing entry point instead.

    /// Route `path` against `site` (the sole site in a fresh `AppConfig`) and
    /// return the resulting `RequestCtx.proxy.route_rate_limit`.
    fn route_rate_limit_for(site: SiteConfig, path: &str) -> Option<RouteRateLimit> {
        let config = AppConfig {
            sites: vec![site],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        route_request(
            &config,
            "localhost",
            path,
            "GET",
            &http::HeaderMap::new(),
            None,
            "127.0.0.1",
            80,
            &counters,
            &reg,
            None,
        )
        .proxy
        .route_rate_limit
    }

    #[test]
    fn route_rate_limit_returns_none_when_no_proxy() {
        let site = SiteConfig::default();
        assert!(route_rate_limit_for(site, "/api").is_none());
    }

    #[test]
    fn route_rate_limit_returns_rl_when_configured() {
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
        let result = route_rate_limit_for(site, "/api/users");
        assert!(result.is_some(), "rate limit must be found for /api prefix");
        let result = result.unwrap();
        assert_eq!(result.config.limit, 100);
        assert!(
            result.route_key.contains("api"),
            "route key must contain 'api': {}",
            result.route_key
        );
    }

    /// The core regression this fix guards against: a site with BOTH
    /// `routes[]` and a legacy `proxy` map (a legal, documented
    /// backward-compatible shape) must not let the *non-selected* mechanism's
    /// rate limit leak onto a request served by the other one. Before #360's
    /// fix, `find_route_rate_limit` scanned only `site.proxy` regardless of
    /// which mechanism actually matched — a request resolved via `routes[]`
    /// would wrongly inherit the proxy map's rate limit. Confirmed to fail
    /// against the pre-fix code (see this PR's description) before trusting
    /// this as a regression guard.
    #[test]
    fn routes_array_match_does_not_inherit_proxy_map_rate_limit() {
        use crate::config::schema::{
            MatchConfig, ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget,
            RateLimitConfig, RouteConfig,
        };
        let mut proxy_map = indexmap::IndexMap::new();
        proxy_map.insert(
            "/".to_owned(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://legacy:4000".to_owned())],
                rate_limit: Some(RateLimitConfig {
                    limit: 5,
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
        let route = RouteConfig {
            r#match: MatchConfig {
                path: Some("/api/**".to_owned()),
                ..Default::default()
            },
            // No `rateLimit` on the routes[] entry itself.
            proxy: Some(ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://api:4000".to_owned())],
                ..Default::default()
            }))),
            static_files: None,
        };
        let site = SiteConfig {
            proxy: Some(ProxyConfig::Routes(proxy_map)),
            routes: Some(vec![route]),
            ..Default::default()
        };
        assert!(
            route_rate_limit_for(site, "/api/x").is_none(),
            "a routes[]-matched request must not inherit the non-selected proxy map's rate limit"
        );
    }

    /// A route matches but every upstream is at its connection cap (circuit
    /// breaker → `Overloaded`, not the "normal" success `RouteResolution`
    /// literal). The rate limit must still be stamped on this outcome — the
    /// old design (a second, outcome-independent post-routing scan) applied
    /// regardless of what routing actually produced, so stamping only on the
    /// success path would silently drop rate limiting exactly when every
    /// upstream is down (a DoS regression). Proves the stamp is applied by
    /// `resolve_proxy_routes`'s wrapper around every return path, not just
    /// inline in the success branch.
    #[test]
    fn route_rate_limit_stamped_even_when_overloaded() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RateLimitConfig,
            UpstreamHealthCheck,
        };
        let mut routes = indexmap::IndexMap::new();
        routes.insert(
            "/api".to_owned(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: vec![ProxyTarget::Simple("http://b:4000".to_owned())],
                health_check: Some(UpstreamHealthCheck {
                    max_connections_per_upstream: Some(1),
                    ..Default::default()
                }),
                rate_limit: Some(RateLimitConfig {
                    limit: 10,
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
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let counters = DashMap::new();
        let reg = UpstreamRegistry::new();
        // Fill the connection slot (count = 1 = max) so capacity is exhausted.
        reg.conn_inc("http://b:4000");

        let ctx = route_request(
            &config,
            "localhost",
            "/api/x",
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
            "expected Overloaded, got {:?}",
            ctx.upstream
        );
        assert!(
            ctx.proxy.route_rate_limit.is_some(),
            "rate limit must still be stamped on an overloaded resolution, not only on the success path"
        );
    }

    /// Issue #415 regression: a `proxy` map route with `rateLimit` configured,
    /// whose target is a malformed URL, must still stamp `route_rate_limit`
    /// onto the resulting (fallback) resolution — the rate limit must still
    /// be enforced/observable even though the request never reaches a real
    /// upstream. Before the fix, the already-computed stamp was discarded by
    /// a `?` short-circuit on the inner resolution function's `None` result;
    /// verified this actually reproduces pre-fix by temporarily reverting the
    /// stamping in `resolve_proxy_routes` and confirming this test fails with
    /// the predicted symptom (see this PR's description).
    #[test]
    fn malformed_target_still_stamps_rate_limit_issue_415() {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RateLimitConfig,
        };
        use indexmap::IndexMap;

        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/api".to_owned(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                // Malformed: empty host after scheme-trimming — the one input
                // `url_to_host_port` actually rejects (see
                // `malformed_backup_url_falls_through_to_fallback`'s own
                // note a bit further down this file).
                targets: vec![ProxyTarget::Simple("http://".to_owned())],
                rate_limit: Some(RateLimitConfig {
                    limit: 5,
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
            "/api/x",
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
            matches!(ctx.upstream, UpstreamTarget::Local(LocalHandler::Fallback)),
            "malformed target must fall through to Fallback (no static config here): {:?}",
            ctx.upstream
        );
        assert!(
            ctx.proxy.route_rate_limit.is_some(),
            "issue #415: the route's rate-limit stamp must survive an unresolved \
             target falling through to fallback"
        );
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

    // ── resolve_non_proxy_route (routes[] entries with no `proxy` action) ─────
    //
    // Formerly covered by `routes.rs::route_to_result_static_files`/
    // `..._no_proxy_no_static_gives_fallback` before the static-action branch
    // moved here from `routes.rs`'s combined dispatcher (issue #143).

    #[test]
    #[cfg(feature = "static")]
    fn resolve_non_proxy_route_serves_static_file() {
        use crate::config::schema::{RouteConfig, StaticConfig};
        let route = RouteConfig {
            r#match: crate::config::schema::MatchConfig::default(),
            proxy: None,
            static_files: Some(StaticConfig::Single("./dist".to_string())),
        };
        let result = resolve_non_proxy_route(&route, "/", None);
        assert!(matches!(
            result.upstream,
            UpstreamTarget::Local(LocalHandler::StaticFile { .. })
        ));
    }

    #[test]
    fn resolve_non_proxy_route_no_static_falls_back() {
        use crate::config::schema::RouteConfig;
        let route = RouteConfig {
            r#match: crate::config::schema::MatchConfig::default(),
            proxy: None,
            static_files: None,
        };
        let result = resolve_non_proxy_route(&route, "/api", None);
        assert!(matches!(
            result.upstream,
            UpstreamTarget::Local(LocalHandler::Fallback)
        ));
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
                ctx.proxy.upstream_conn_slot,
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
            if ctx.proxy.upstream_conn_slot {
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

    /// Build an n-peer sticky route (`http://a:4000` .. ), optionally with
    /// `retry` / `strict`, and route one request carrying a cookie signed
    /// for `pin_idx`.
    fn sticky_route_request(
        n: usize,
        pin_idx: usize,
        strict: bool,
        retry: Option<crate::config::schema::RetryConfig>,
        reg: &UpstreamRegistry,
    ) -> RequestCtx {
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, StickyConfig,
        };
        use indexmap::IndexMap;

        let urls: Vec<String> = (0..n)
            .map(|i| format!("http://{}:4000", (b'a' + i as u8) as char))
            .collect();
        let mut routes: IndexMap<String, ProxyRouteTarget> = IndexMap::new();
        routes.insert(
            "/".to_string(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                targets: urls.iter().cloned().map(ProxyTarget::Simple).collect(),
                sticky: Some(StickyConfig {
                    cookie: "srv_id".to_owned(),
                    secret: Some("s3cret".to_owned()),
                    strict: strict.then_some(true),
                }),
                retry,
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
        let signed = hmac_sign_sticky(&urls[pin_idx], "s3cret");
        let mut headers = http::HeaderMap::new();
        headers.insert("cookie", format!("srv_id={signed}").parse().unwrap());

        route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &headers,
            None,
            "127.0.0.1",
            80,
            &counters,
            reg,
            None,
        )
    }

    fn chosen_addr(ctx: &RequestCtx) -> String {
        match &ctx.upstream {
            UpstreamTarget::Proxy { addr, .. } => addr.clone(),
            other => panic!("expected Proxy upstream, got {other:?}"),
        }
    }

    /// Regression test for #220. The previous version of this test used
    /// exactly two peers (`a`, `b`) and asserted the pinned one was chosen —
    /// and it passed, but only by luck: `fnv1a("http://b:4000") % 2` happens
    /// to equal `1`, b's own index. Measured across 2..8-peer rings, a peer's
    /// URL hashes back to its own index only ~23% of the time (i.e. chance);
    /// with **4** peers not a single one does. So the old test asserted the
    /// right thing, passed, and still let the bug ship.
    ///
    /// Sweeping every (ring size, pinned index) pair makes luck impossible:
    /// any implementation that routes by hashing the pin's URL instead of
    /// honoring it directly fails this at n=3 and fails it four times over
    /// at n=4.
    #[test]
    fn sticky_hmac_routes_to_pinned_upstream() {
        for n in 2..=5usize {
            for pin_idx in 0..n {
                let reg = UpstreamRegistry::new();
                let ctx = sticky_route_request(n, pin_idx, false, None, &reg);
                let expected = format!("{}:4000", (b'a' + pin_idx as u8) as char);
                assert_eq!(
                    chosen_addr(&ctx),
                    expected,
                    "n={n}, pinned index {pin_idx}: must route to the HMAC-pinned \
                     upstream, not wherever hashing its URL happens to land"
                );
            }
        }
    }

    /// #366: a route with `retry` configured used to bypass strategy
    /// dispatch entirely and do blind round-robin, so sticky affinity was
    /// not merely mis-mapped there (#220) but absent outright. The pin must
    /// be honored on retry-configured routes too, and must be attempt 0 of
    /// the retry rotation (the invariant `select_retry_target` relies on).
    #[test]
    fn sticky_pin_is_honored_and_anchored_on_a_retry_configured_route() {
        use crate::config::schema::RetryConfig;
        for n in 2..=5usize {
            for pin_idx in 0..n {
                let reg = UpstreamRegistry::new();
                let retry = RetryConfig {
                    attempts: 3,
                    conditions: vec!["connection_error".to_owned()],
                    backoff_ms: None,
                    backoff_jitter: None,
                    budget_percent: None,
                };
                let ctx = sticky_route_request(n, pin_idx, false, Some(retry), &reg);
                let expected_addr = format!("{}:4000", (b'a' + pin_idx as u8) as char);
                let expected_url = format!("http://{}:4000", (b'a' + pin_idx as u8) as char);
                assert_eq!(
                    chosen_addr(&ctx),
                    expected_addr,
                    "n={n}, pin {pin_idx}: retry-configured route must still honor the pin"
                );
                let retry_state = ctx
                    .proxy
                    .retry
                    .as_ref()
                    .expect("retry state must be populated");
                assert_eq!(
                    retry_state.urls.first(),
                    Some(&expected_url),
                    "n={n}, pin {pin_idx}: retry.urls[0] must be the pinned peer"
                );
            }
        }
    }

    /// `strict: true` guards the health of the peer that actually serves the
    /// request. Before #220 it checked the pin's health while hashing routed
    /// elsewhere — guarding one peer and serving another.
    #[test]
    fn sticky_strict_serves_the_peer_whose_health_it_guards() {
        for n in 2..=5usize {
            for pin_idx in 0..n {
                let reg = UpstreamRegistry::new();
                let ctx = sticky_route_request(n, pin_idx, true, None, &reg);
                let expected = format!("{}:4000", (b'a' + pin_idx as u8) as char);
                assert_eq!(
                    chosen_addr(&ctx),
                    expected,
                    "n={n}, pin {pin_idx}: strict mode passed on the pin's health, \
                     so the pin must be what gets served"
                );
            }
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
                // Three peers pinned to "b" deliberately: at n=2 every peer
                // happens to hash back to its own index, so a 2-peer fixture
                // routes identically whether the pin is honored directly
                // (#220's fix) or re-hashed the old way — it cannot tell the
                // two apart. `b` in a 3-peer ring hashes to `c`, so the
                // self-heal leg below genuinely discriminates.
                targets: vec![
                    ProxyTarget::Simple("http://a:4000".to_owned()),
                    ProxyTarget::Simple("http://b:4000".to_owned()),
                    ProxyTarget::Simple("http://c:4000".to_owned()),
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

        // Pin the cookie to "b", then saturate "b" at its cap (still
        // healthy, just at capacity) so the pick must relocate.
        let signed_a = hmac_sign_sticky("http://b:4000", "s3cret");
        let mut headers = http::HeaderMap::new();
        headers.insert("cookie", format!("srv_id={signed_a}").parse().unwrap());
        reg.conn_inc("http://b:4000");

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
                assert_ne!(
                    addr, "b:4000",
                    "must relocate off the saturated pinned peer"
                );
            }
            other => panic!("expected Proxy upstream, got {:?}", other),
        }
        assert!(
            ctx.proxy.sticky_set_cookie.is_none(),
            "capacity relocation must NOT re-sign the cookie — doing so would \
             permanently migrate the session to the fallback peer: {:?}",
            ctx.proxy.sticky_set_cookie
        );

        // Free "b"'s slot and present the SAME original cookie again (no new
        // cookie was issued, so the client would still be holding this one).
        reg.conn_dec("http://b:4000");
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
                    addr, "b:4000",
                    "must self-heal back to the original pin once capacity frees"
                );
            }
            other => panic!("expected Proxy upstream, got {:?}", other),
        }
    }

    #[test]
    fn sticky_total_outage_with_saturated_pin_re_signs_cookie_not_self_heal() {
        // #374: during a total-pool outage (every peer marked unhealthy,
        // filter_healthy fails open), a pin that's ALSO over its connection
        // cap must not be classified as a "self-healing capacity
        // relocation" (which keeps the old cookie, betting on a comeback) --
        // fail-open's "healthy" list can't actually vouch for the pin, so
        // the old `healthy_urls.contains(pin)` check trivially succeeded
        // here even though nothing in the pool is genuinely healthy.
        // Correct behavior: re-sign the cookie onto wherever the request
        // actually landed, the same as a genuinely-gone pin.
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
                    ProxyTarget::Simple("http://c:4000".to_owned()),
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

        // Every peer genuinely unhealthy -- filter_healthy() will fail open.
        for url in ["http://a:4000", "http://b:4000", "http://c:4000"] {
            reg.statuses.entry(url.to_owned()).or_default().healthy = false;
        }
        // Pin to "b" and also saturate it, so `honored_pin` is None due to
        // capacity -- the specific compound case #374 is about.
        let signed = hmac_sign_sticky("http://b:4000", "s3cret");
        let mut headers = http::HeaderMap::new();
        headers.insert("cookie", format!("srv_id={signed}").parse().unwrap());
        reg.conn_inc("http://b:4000");

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
            ctx.proxy.sticky_set_cookie.is_some(),
            "a total outage with a saturated pin must re-sign the cookie, not \
             silently keep pointing at an unverifiable peer: {:?}",
            ctx.proxy.sticky_set_cookie
        );
    }

    #[test]
    fn slow_start_exemption_covers_the_retry_candidate_list_on_hash_routes() {
        // #375: slow_start.rs's own module doc claims the hash/sticky
        // exemption is structural and needs zero code -- true for the
        // *primary* pick (pick_bounded early-returns before the ramp is
        // ever consulted), but the retry-candidate list used a separate,
        // unconditional ramp filter that didn't check strategy at all. On
        // an ipHash/consistentHash route with retry configured, a
        // just-recovered ("mid-ramp") peer could be silently dropped from
        // retry attempts 1+ even though it's fully eligible for the
        // primary pick under the same strategy.
        use crate::config::schema::{
            ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, UpstreamHealthCheck,
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
                strategy: Some(LoadBalanceStrategy::IpHash),
                health_check: Some(UpstreamHealthCheck {
                    slow_start_secs: Some(30),
                    ..Default::default()
                }),
                retry: Some(RetryConfig {
                    attempts: 3,
                    conditions: vec!["5xx".to_owned()],
                    backoff_ms: None,
                    backoff_jitter: None,
                    budget_percent: None,
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
        // "a" just recovered -- fraction 0.0, deterministically excluded by
        // the ramp filter if it isn't exempt. "b" has no recorded recovery
        // (fully ramped), so the filter can't fail open here -- if the
        // exemption is broken, exactly "a" goes missing from the list, not
        // both.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        reg.statuses
            .entry("http://a:4000".to_owned())
            .or_default()
            .recovery_time_secs = Some(now_secs);

        // "10.0.0.1" is deliberate, not arbitrary: fnv1a_hash("10.0.0.1") % 2
        // == 1, so the *primary* pick lands on "b", not "a" -- unlike
        // "127.0.0.1" (hashes to index 0 == "a"), which made this test
        // tautological. `retry_state_for` unconditionally re-inserts
        // `chosen_url` at the front of the retry list whenever it's absent
        // from the (possibly ramp-filtered) candidates -- a real, correct
        // invariant for #367/#216 part 2, but it means that if the primary
        // pick were "a" itself, "a" would always appear in `retry.urls`
        // regardless of whether the #375 exemption actually ran. With "b" as
        // the primary pick, "a" can only appear in `retry.urls` because the
        // exemption kept it in the ramp-filtered candidate list -- exactly
        // the mechanism this test is meant to prove.
        let ctx = route_request(
            &config,
            "localhost",
            "/",
            "GET",
            &http::HeaderMap::new(),
            None,
            "10.0.0.1",
            80,
            &counters,
            &reg,
            None,
        );
        let retry = ctx
            .proxy
            .retry
            .expect("retry must be configured for this route");
        assert_eq!(
            retry.urls[0], "http://b:4000",
            "sanity check: primary pick must be \"b\", not \"a\", or this \
             test cannot discriminate the bug it's meant to catch"
        );
        assert!(
            retry.urls.iter().any(|u| u == "http://a:4000"),
            "mid-ramp peer must still appear in the retry candidate list on \
             a hash-strategy route -- got {:?}",
            retry.urls
        );
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
            .proxy
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
            ctx.proxy.proxy_upstream_url.is_some(),
            "URL must be populated for passive-health attribution regardless of strategy"
        );
        assert!(
            !ctx.proxy.upstream_conn_slot,
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
        assert!(ctx.proxy.proxy_upstream_url.is_some());
        assert!(
            ctx.proxy.upstream_conn_slot,
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
        assert!(ctx.proxy.proxy_upstream_url.is_some());
        assert!(
            ctx.proxy.upstream_conn_slot,
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
        assert!(ctx_lc.proxy.upstream_conn_slot);
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
            ctx_rr.proxy.proxy_upstream_url.is_some(),
            "round-robin route still gets the URL for attribution"
        );
        assert!(
            !ctx_rr.proxy.upstream_conn_slot,
            "round-robin route on a shared target must not claim a slot"
        );
        assert_eq!(
            reg.conn_load(SHARED),
            1,
            "the round-robin route must not have touched the shared conn_count"
        );
    }
}
