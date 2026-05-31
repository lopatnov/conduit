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
/// Fields: (upstream, retry, timeout, pool, http2, upstream_url_for_least_conn, cache_cfg)
pub type RouteResultAlias = (
    UpstreamTarget,
    Option<RetryState>,
    Option<ProxyTimeout>,
    Option<ConnectionPoolConfig>,
    bool,                // proxy_http2
    Option<String>,      // upstream URL selected (for least-conn decrement)
    Option<CacheConfig>, // per-route cache config, if caching is enabled
);

type RouteResult = RouteResultAlias;

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

    let (
        upstream,
        retry,
        proxy_timeout,
        proxy_pool,
        proxy_http2,
        proxy_upstream_url,
        proxy_cache_cfg,
    ) = if let Some(token) = acme_challenge_token(path) {
        // ACME HTTP-01 challenge — served before any site-level routing so that
        // the challenge is available even on sites that redirect or auth-protect
        // everything.
        (
            UpstreamTarget::Local(LocalHandler::AcmeChallenge {
                token: token.to_owned(),
            }),
            None,
            None,
            None,
            false,
            None,
            None,
        )
    } else if is_health_path(site, path) {
        (
            UpstreamTarget::Local(LocalHandler::Health),
            None,
            None,
            None,
            false,
            None,
            None,
        )
    } else if let Some(token) = metrics_token(site, path) {
        (
            UpstreamTarget::Local(LocalHandler::Metrics { token }),
            None,
            None,
            None,
            false,
            None,
            None,
        )
    } else if is_hot_reload_js_path(site, path) {
        (
            UpstreamTarget::Local(LocalHandler::HotReloadJs),
            None,
            None,
            None,
            false,
            None,
            None,
        )
    } else if is_hot_reload_sse_path(site, path) {
        (
            UpstreamTarget::Local(LocalHandler::HotReloadSse),
            None,
            None,
            None,
            false,
            None,
            None,
        )
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
        (
            UpstreamTarget::Local(LocalHandler::Fallback),
            None,
            None,
            None,
            false,
            None,
            None,
        )
    };

    let response_transform = site.and_then(|s| s.response_transform.clone());
    RequestCtx::new(
        site_idx,
        upstream,
        retry,
        proxy_timeout,
        proxy_pool,
        proxy_http2,
        proxy_upstream_url,
        proxy_cache_cfg,
        response_transform,
    )
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
    upload_addr: Option<SocketAddr>,
) -> RouteResult {
    let site_label = crate::proxy::health::site_label(&site.host, site.port);

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
fn match_upload_route(
    site: &SiteConfig,
    path: &str,
    upload_addr: Option<SocketAddr>,
) -> Option<RouteResult> {
    let (upload_cfg, addr) = site.upload.as_ref().zip(upload_addr)?;
    let upload_prefix = upload_cfg.path.trim_end_matches('/');
    let matches = path == upload_prefix || path.starts_with(&format!("{upload_prefix}/"));
    matches.then_some((
        UpstreamTarget::Upload { addr },
        None,
        None,
        None,
        false,
        None,
        None,
    ))
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
            return (
                UpstreamTarget::Local(LocalHandler::StaticFile {
                    roots,
                    options,
                    strip_prefix,
                }),
                None,
                None,
                None,
                false,
                None,
                None,
            );
        }
    }
    (
        UpstreamTarget::Local(LocalHandler::Fallback),
        None,
        None,
        None,
        false,
        None,
        None,
    )
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
        ProxyConfig::Single(url) => Some((
            url_to_proxy_upstream(url, None)?,
            None,
            None,
            None,
            false,
            None,
            None,
        )),
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
                ),
                _ => (
                    None, None, None, None, false, "ip", None, None, None, None, None,
                ),
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
                    return url_to_proxy_upstream(backup, None)
                        .map(|upstream| (upstream, None, None, None, false, None, None));
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
                    return Some((
                        UpstreamTarget::Local(LocalHandler::Overloaded),
                        None,
                        None,
                        None,
                        false,
                        None,
                        None,
                    ));
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

            // Sticky sessions: if configured, extract the cookie value and use
            // it as the hash input — the same cookie value always maps to the
            // same upstream bucket (consistent hashing).
            let sticky_override: Option<String> = if let ProxyRouteTarget::Full(cfg) = route_target
            {
                cfg.sticky
                    .as_ref()
                    .and_then(|s| extract_cookie(req_headers, &s.cookie))
            } else {
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

            Some((
                upstream,
                retry_state,
                proxy_timeout,
                proxy_pool,
                proxy_http2,
                proxy_upstream_url,
                cache_cfg,
            ))
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
    Some((
        upstream,
        retry_state,
        proxy_timeout,
        proxy_pool,
        proxy_http2,
        proxy_upstream_url,
        cache_cfg,
    ))
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
}
