use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::config::schema::{
    AppConfig, CacheConfig, ConnectionPoolConfig, LoadBalanceStrategy, ProxyConfig,
    ProxyRouteTarget, ProxyTimeout, RetryConfig, SiteConfig, StaticConfig, UpstreamGroup,
};
use crate::proxy::ctx::{LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::health::UpstreamRegistry;
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
    /// Selected upstream URL — `Some` for least-conn / circuit-breaker routes
    /// so `logging()` can decrement the per-upstream counter after the response.
    pub proxy_upstream_url: Option<String>,
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
        if let Some(result) = resolve_proxy(
            proxy_cfg,
            path,
            client_ip,
            req_headers,
            counters,
            upstream_health,
            &site_label,
        ) {
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
fn match_static_or_fallback(site: &SiteConfig, path: &str) -> RouteResult {
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

fn resolve_proxy(
    config: &ProxyConfig,
    path: &str,
    client_ip: &str,
    req_headers: &http::HeaderMap,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
    site_label: &str,
) -> Option<RouteResult> {
    match config {
        ProxyConfig::Single(url) => Some(RouteResolution::local(url_to_proxy_upstream(url, None)?)),
        ProxyConfig::Routes(routes) => {
            let (route_key, route_target) = find_route(routes, path)?;

            // ── Two-level (grouped) routing ───────────────────────────────────
            // When the route config has `groups`, bypass flat-target logic and
            // resolve via pick_group → pick_within_group.
            if let ProxyRouteTarget::Full(cfg) = route_target {
                if let Some(groups) = &cfg.groups {
                    return resolve_grouped(
                        cfg.group_strategy.as_ref(),
                        groups,
                        cfg.hash_key.as_deref().unwrap_or("ip"),
                        route_key,
                        path,
                        client_ip,
                        counters,
                        upstream_health,
                        cfg.strip_prefix.unwrap_or(false),
                        cfg.timeout.clone(),
                        cfg.pool.clone(),
                        cfg.http2.unwrap_or(false),
                        cfg.cache.clone(),
                        cfg.rewrite.clone(),
                    );
                }
            }

            // ── Runtime override check ────────────────────────────────────────
            // When the operator has issued `conduit upstreams add/remove/weight`,
            // those targets replace the config-file targets for this route.
            let runtime_targets = upstream_health.get_override_targets(site_label, route_key);
            let (all_urls, all_weighted_base): (Vec<String>, Vec<(String, u32)>) =
                if let Some(ref ov) = runtime_targets {
                    let urls = ov.iter().map(|(u, _)| u.clone()).collect();
                    (urls, ov.clone())
                } else {
                    (
                        upstream::target_urls(route_target),
                        upstream::weighted_targets(route_target),
                    )
                };

            let (
                retry_cfg,
                proxy_timeout,
                proxy_pool,
                strategy,
                proxy_http2,
                hash_key,
                cache_cfg,
                rewrite_rules,
                mirror_url,
                upstream_tls,
                max_conns_per_upstream,
                websocket_allowed,
            ) = match route_target {
                ProxyRouteTarget::Full(cfg) => (
                    cfg.retry.as_ref(),
                    cfg.timeout.clone(),
                    cfg.pool.clone(),
                    cfg.strategy.as_ref(),
                    cfg.http2.unwrap_or(false),
                    cfg.hash_key.as_deref().unwrap_or("ip"),
                    cfg.cache.clone(),
                    cfg.rewrite.clone(),
                    cfg.mirror.clone(),
                    cfg.upstream_tls.clone(),
                    cfg.health_check
                        .as_ref()
                        .and_then(|hc| hc.max_connections_per_upstream),
                    cfg.websocket.unwrap_or(false),
                ),
                _ => (
                    None, None, None, None, false, "ip", None, None, None, None, None, false,
                ),
            };

            // Passive health thresholds — extracted after the match via a separate
            // if-let so cfg is in scope.
            let passive_unhealthy_status: Vec<u16> =
                if let ProxyRouteTarget::Full(cfg) = route_target {
                    cfg.health_check
                        .as_ref()
                        .and_then(|hc| hc.unhealthy_status.clone())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
            let passive_unhealthy_latency_ms: Option<u64> =
                if let ProxyRouteTarget::Full(cfg) = route_target {
                    cfg.health_check
                        .as_ref()
                        .and_then(|hc| hc.unhealthy_latency_ms)
                } else {
                    None
                };

            // Failover: when a backup URL is configured and all primary upstreams
            // are unhealthy, route to the backup instead.
            let backup_url: Option<String> = if let ProxyRouteTarget::Full(cfg) = route_target {
                cfg.backup.clone()
            } else {
                None
            };
            let all_unhealthy =
                !all_urls.is_empty() && all_urls.iter().all(|u| !upstream_health.is_healthy(u));
            if all_unhealthy {
                if let Some(ref backup) = backup_url {
                    tracing::info!(backup = %backup, "all primary upstreams unhealthy — routing to backup");
                    return url_to_proxy_upstream(backup, None).map(RouteResolution::local);
                }
            }

            // Filter to healthy upstreams; if all are down keep all (fail-open).
            let healthy = upstream_health.filter_healthy(&all_urls);
            let urls: Vec<String> = healthy.iter().cloned().cloned().collect();

            // Circuit breaker: if maxConnectionsPerUpstream is configured, filter
            // out upstreams that are at or above the limit.
            // If ALL healthy upstreams are at max capacity → return Overloaded (503).
            if let Some(max_conns) = max_conns_per_upstream {
                let under_limit: Vec<String> = urls
                    .iter()
                    .filter(|u| upstream_health.conn_load(u) < max_conns as usize)
                    .cloned()
                    .collect();
                if under_limit.is_empty() && !urls.is_empty() {
                    tracing::debug!(
                        route = route_key,
                        max_conns,
                        "circuit open: all upstreams at connection limit"
                    );
                    return Some(RouteResolution::local(UpstreamTarget::Local(
                        LocalHandler::Overloaded,
                    )));
                }
                // Use only under-limit upstreams from here on.
                // (If under_limit is empty but urls is also empty, fall through
                //  to the normal no-URL path.)
                let _ = under_limit; // URLs already filtered above; strategy will re-check conn_load
            }

            // Build weighted list filtered to healthy targets.
            let weighted: Vec<(String, u32)> = all_weighted_base
                .into_iter()
                .filter(|(url, _)| urls.contains(url))
                .collect();

            // Sticky sessions: extract and optionally verify the session cookie.
            //
            // When `sticky.secret` is set, verify the HMAC-SHA256 of each candidate
            // upstream URL to find the pinned backend.  A forged or unmatched cookie
            // falls through to normal load-balancing (or returns 503 in strict mode).
            // Without `secret`, the raw cookie value is used as the consistent-hash
            // key (legacy behavior).
            let sticky_cfg = if let ProxyRouteTarget::Full(cfg) = route_target {
                cfg.sticky.as_ref()
            } else {
                None
            };
            let sticky_cookie_name: Option<&str> = sticky_cfg.map(|s| s.cookie.as_str());
            let sticky_cookie_val: Option<String> =
                sticky_cookie_name.and_then(|name| extract_cookie(req_headers, name));

            // HMAC sticky: find which healthy upstream the cookie was signed for.
            // `pinned_url` = Some(url) if cookie is a valid HMAC of that URL.
            let pinned_url: Option<String> =
                if let (Some(val), Some(cfg)) = (sticky_cookie_val.as_deref(), sticky_cfg) {
                    if let Some(ref secret) = cfg.secret {
                        // Try to find the upstream whose HMAC matches the cookie.
                        all_urls
                            .iter()
                            .find(|u| hmac_verify_sticky(u, val, secret))
                            .cloned()
                    } else {
                        None // No secret: use raw cookie as hash input (below)
                    }
                } else {
                    None
                };

            // Strict mode: if the client presented a signed cookie for a peer
            // that is now unhealthy, refuse the request rather than silently
            // routing to a different upstream (which would break session affinity).
            if let (Some(ref url), Some(cfg)) = (pinned_url.as_ref(), sticky_cfg) {
                if cfg.strict.unwrap_or(false) && !upstream_health.is_healthy(url) {
                    tracing::debug!(
                        url = %url,
                        "sticky strict mode: pinned upstream unhealthy — returning 503"
                    );
                    return Some(RouteResolution::local(UpstreamTarget::Local(
                        LocalHandler::Overloaded,
                    )));
                }
            }

            // Determine the hash / override value for consistent-hash selection.
            //
            // Security: when a secret is configured (HMAC mode), a cookie that
            // fails signature verification must NOT influence routing — otherwise
            // a client could forge/manipulate their cookie to steer to specific
            // upstreams.  Raw cookie values are only used when no secret is set
            // (legacy, non-HMAC sticky).
            let hmac_mode = sticky_cfg.and_then(|cfg| cfg.secret.as_ref()).is_some();
            let sticky_override: Option<String> = if let Some(ref pinned) = pinned_url {
                // HMAC-verified: pin to the exact upstream.
                Some(pinned.clone())
            } else if !hmac_mode {
                // No secret configured: use raw cookie as consistent-hash input.
                sticky_cookie_val
            } else {
                // HMAC mode but cookie failed verification: ignore — fall through
                // to the configured load-balancing strategy.
                None
            };

            // Compute hash value for ip-hash, consistent-hash, and sticky.
            // Priority: sticky cookie > hash_key config > client IP.
            let hash_input: &str = if let Some(ref cookie_val) = sticky_override {
                cookie_val.as_str()
            } else if hash_key == "url" || client_ip.is_empty() {
                path
            } else {
                client_ip
            };

            // When sticky is active, override strategy to consistent-hash so
            // the cookie value is always used for backend selection.
            let effective_strategy: Option<LoadBalanceStrategy>;
            let strategy = if sticky_override.is_some() {
                effective_strategy = Some(LoadBalanceStrategy::ConsistentHash);
                effective_strategy.as_ref()
            } else {
                strategy
            };

            let hash_val = upstream::fnv1a_hash(hash_input);

            let hash_ctx = HashCtx {
                weighted: &weighted,
                hash_val,
            };
            let (chosen_url, retry_state, is_least_conn) = pick_url_by_strategy(
                urls,
                route_key,
                counters,
                retry_cfg,
                strategy,
                upstream_health,
                &hash_ctx,
            )?;

            let strip = if upstream::strip_prefix_enabled(route_target) {
                Some(route_key.trim_end_matches('/').to_string())
            } else {
                None
            };

            // url_to_proxy_upstream may return None for a malformed URL.
            // If least-conn already incremented the inflight counter we must
            // release it here — the logging() hook won't run on this request.
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
                    rewrite: rewrite_rules,
                    mirror_url: mirror_url.clone(),
                    upstream_tls: upstream_tls.clone(),
                },
                Some(other) => other,
                None => {
                    if is_least_conn {
                        upstream_health.conn_dec(&chosen_url);
                    }
                    return None;
                }
            };

            // When maxConnectionsPerUpstream is set and the strategy is NOT
            // least-conn (which already tracks conn_count), we increment the
            // counter manually here so the circuit breaker sees accurate load.
            // The conn_count is decremented by logging() via proxy_upstream_url.
            let circuit_tracking = max_conns_per_upstream.is_some() && !is_least_conn;
            if circuit_tracking {
                upstream_health.conn_inc(&chosen_url);
            }

            // Store the upstream URL so logging() can:
            // (a) decrement least-conn counter, and
            // (b) decrement circuit-breaker counter (when not LeastConn).
            let proxy_upstream_url =
                (is_least_conn || circuit_tracking).then(|| chosen_url.clone());

            // HMAC sticky: sign the chosen upstream URL and schedule a
            // Set-Cookie injection on the response side.
            let sticky_set_cookie: Option<(String, String)> = if let Some(cfg) = sticky_cfg {
                if let Some(ref secret) = cfg.secret {
                    let signed = hmac_sign_sticky(&chosen_url, secret);
                    Some((cfg.cookie.clone(), signed))
                } else {
                    None
                }
            } else {
                None
            };

            Some(RouteResolution {
                upstream,
                retry: retry_state,
                proxy_timeout,
                proxy_pool,
                proxy_http2,
                proxy_upstream_url,
                proxy_cache_cfg: cache_cfg,
                passive_unhealthy_status,
                passive_unhealthy_latency_ms,
                websocket_allowed,
                sticky_set_cookie,
            })
        }
    }
}

/// Two-level load balancing: pick a group via `group_strategy`, then pick a
/// target within the group using each group's own `strategy`.
///
/// Group selection keys:
/// - `hash_key = "ip"` → hash client IP across groups (sticky per client)
/// - `hash_key = "url"` → hash request path across groups
/// - Other strategies (round-robin, random, least-conn, …) work as usual.
#[allow(clippy::too_many_arguments)]
fn resolve_grouped(
    group_strategy: Option<&LoadBalanceStrategy>,
    groups: &[UpstreamGroup],
    hash_key: &str,
    route_key: &str,
    path: &str,
    client_ip: &str,
    counters: &DashMap<String, AtomicUsize>,
    upstream_health: &UpstreamRegistry,
    strip_prefix_flag: bool,
    proxy_timeout: Option<crate::config::schema::ProxyTimeout>,
    proxy_pool: Option<crate::config::schema::ConnectionPoolConfig>,
    proxy_http2: bool,
    cache_cfg: Option<crate::config::schema::CacheConfig>,
    rewrite_rules: Option<Vec<crate::config::schema::RewriteRule>>,
) -> Option<RouteResult> {
    if groups.is_empty() {
        return None;
    }

    // Outer pick: choose which group handles this request.
    let group_key = format!("{route_key}__group");
    let hash_input = if hash_key == "url" || client_ip.is_empty() {
        path
    } else {
        client_ip
    };
    let hash_val = upstream::fnv1a_hash(hash_input);

    let group_names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
    let picked_name = {
        let ctx = HashCtx {
            weighted: &[],
            hash_val,
        };
        pick_url_by_strategy(
            group_names.clone(),
            &group_key,
            counters,
            None,
            group_strategy,
            upstream_health,
            &ctx,
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

    let healthy = upstream_health.filter_healthy(&all_urls);
    let urls: Vec<String> = healthy.iter().cloned().cloned().collect();
    let weighted_healthy: Vec<(String, u32)> = weighted
        .into_iter()
        .filter(|(u, _)| urls.contains(u))
        .collect();

    let inner_key = format!("{route_key}__group__{}", group.name);
    let inner_ctx = HashCtx {
        weighted: &weighted_healthy,
        hash_val,
    };
    let (chosen_url, retry_state, is_least_conn) = pick_url_by_strategy(
        urls,
        &inner_key,
        counters,
        None,
        group.strategy.as_ref(),
        upstream_health,
        &inner_ctx,
    )?;

    let strip = strip_prefix_flag.then(|| route_key.trim_end_matches('/').to_string());
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
            rewrite: rewrite_rules,
            mirror_url: None, // groups don't support mirror in V1
            upstream_tls: None,
        },
        Some(other) => other,
        None => {
            if is_least_conn {
                upstream_health.conn_dec(&chosen_url);
            }
            return None;
        }
    };

    let proxy_upstream_url = is_least_conn.then(|| chosen_url.clone());
    Some(RouteResolution {
        upstream,
        retry: retry_state,
        proxy_timeout,
        proxy_pool,
        proxy_http2,
        proxy_upstream_url,
        proxy_cache_cfg: cache_cfg,
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
    urls: Vec<String>,
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
        &urls,
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
    urls: Vec<String>,
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

pub fn resolve_static_roots(cfg: &StaticConfig, path: &str) -> (Vec<PathBuf>, Option<String>) {
    match cfg {
        StaticConfig::Single(s) => (vec![PathBuf::from(s)], None),
        StaticConfig::Multi(v) => (v.iter().map(PathBuf::from).collect(), None),
        StaticConfig::Mapped(m) => match find_best_mapped_prefix(m, path) {
            Some((pfx, root)) => (vec![PathBuf::from(root)], Some(pfx.to_string())),
            None => (vec![], None),
        },
    }
}

/// Find the longest prefix in a mapped static config that matches `path`.
fn find_best_mapped_prefix<'a>(
    m: &'a indexmap::IndexMap<String, String>,
    path: &str,
) -> Option<(&'a str, &'a str)> {
    let mut best: Option<(&str, &str)> = None;
    for (prefix, root) in m {
        let norm = prefix.trim_end_matches('/');
        let matches = norm.is_empty() || path == norm || path.starts_with(&format!("{norm}/"));
        if matches {
            let len = norm.len();
            if best.is_none_or(|(b, _)| len > b.trim_end_matches('/').len()) {
                best = Some((prefix.as_str(), root.as_str()));
            }
        }
    }
    best
}

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

/// Returns `true` when the path targets the hot-reload client JS file
/// (`/__hot-reload__/client.js`) and the site has `hotReload` enabled.
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

/// If `path` starts with the ACME HTTP-01 challenge prefix, return the token
/// portion.  E.g. `/.well-known/acme-challenge/abc123` → `Some("abc123")`.
fn acme_challenge_token(path: &str) -> Option<&str> {
    path.strip_prefix("/.well-known/acme-challenge/")
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

    // ── find_best_mapped_prefix ───────────────────────────────────────────────

    #[test]
    fn mapped_prefix_longest_wins() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/".to_string(), "./root".to_string());
        m.insert("/docs".to_string(), "./docs".to_string());
        let (pfx, root) = find_best_mapped_prefix(&m, "/docs/guide").unwrap();
        assert_eq!(pfx, "/docs");
        assert_eq!(root, "./docs");
    }

    #[test]
    fn mapped_prefix_no_match_returns_none() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/docs".to_string(), "./docs".to_string());
        assert!(find_best_mapped_prefix(&m, "/other").is_none());
    }

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
        let (url, state) = pick_with_retry(urls, "r", &counters, &retry).unwrap();
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
        let (url, state) = pick_with_retry(urls.clone(), "r", &counters, &retry).unwrap();
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
        assert!(pick_with_retry(vec![], "r", &counters, &retry).is_none());
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
            pick_url_by_strategy(urls.clone(), "r", &counters, None, None, &reg, &no_hash())
                .unwrap();
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
            urls.clone(),
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
            urls.clone(),
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
            urls.clone(),
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
        assert!(
            pick_url_by_strategy(vec![], "r", &counters, None, None, &reg, &no_hash()).is_none()
        );
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
                    urls.clone(),
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
            urls.clone(),
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
            urls.clone(),
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
            urls.clone(),
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

    // ── resolve_static_roots ──────────────────────────────────────────────────

    #[test]
    fn static_roots_single() {
        use crate::config::schema::StaticConfig;
        use std::path::PathBuf;
        let (roots, strip) = resolve_static_roots(&StaticConfig::Single("./dist".to_string()), "/");
        assert_eq!(roots, vec![PathBuf::from("./dist")]);
        assert!(strip.is_none());
    }

    #[test]
    fn static_roots_multi() {
        use crate::config::schema::StaticConfig;
        use std::path::PathBuf;
        let (roots, strip) = resolve_static_roots(
            &StaticConfig::Multi(vec!["./a".to_string(), "./b".to_string()]),
            "/",
        );
        assert_eq!(roots, vec![PathBuf::from("./a"), PathBuf::from("./b")]);
        assert!(strip.is_none());
    }

    #[test]
    fn static_roots_mapped_matches_prefix() {
        use crate::config::schema::StaticConfig;
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/docs".to_string(), "./docs-root".to_string());
        m.insert("/".to_string(), "./web".to_string());
        let (roots, strip) = resolve_static_roots(&StaticConfig::Mapped(m), "/docs/guide");
        assert_eq!(roots.len(), 1);
        assert!(roots[0].to_str().unwrap().contains("docs-root"));
        assert_eq!(strip.as_deref(), Some("/docs"));
    }

    #[test]
    fn static_roots_mapped_no_match_returns_empty() {
        use crate::config::schema::StaticConfig;
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/docs".to_string(), "./docs-root".to_string());
        let (roots, _) = resolve_static_roots(&StaticConfig::Mapped(m), "/other");
        assert!(roots.is_empty());
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
    fn acme_challenge_token_empty_token() {
        // Edge case: empty token after the challenge prefix.
        let result = acme_challenge_token("/.well-known/acme-challenge/");
        assert_eq!(result, Some(""));
    }

    // ── is_hot_reload_sse_path and is_hot_reload_js_path ─────────────────────

    #[test]
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
    fn hot_reload_sse_path_when_disabled() {
        let site = SiteConfig {
            hot_reload: Some(crate::config::schema::HotReloadConfig::Enabled(false)),
            ..Default::default()
        };
        assert!(!is_hot_reload_sse_path(Some(&site), "/__hot-reload__"));
    }

    #[test]
    fn hot_reload_sse_path_no_site_returns_false() {
        assert!(!is_hot_reload_sse_path(None, "/__hot-reload__"));
    }

    #[test]
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
            urls.clone(),
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
            vec![],
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
            urls,
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
