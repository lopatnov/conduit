use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::config::schema::{AppConfig, ProxyConfig, ProxyRouteTarget, SiteConfig, StaticConfig};
use crate::proxy::ctx::{LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::upstream;

pub fn route_request(
    config: &AppConfig,
    host: &str,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
) -> RequestCtx {
    let site_idx = find_site_idx(config, host).unwrap_or(0);
    let site = config.sites.get(site_idx);

    let (upstream, retry) = if is_health_path(site, path) {
        (UpstreamTarget::Local(LocalHandler::Health), None)
    } else if let Some(token) = metrics_token(site, path) {
        (UpstreamTarget::Local(LocalHandler::Metrics { token }), None)
    } else if let Some(site) = site {
        route_site(site, path, counters)
    } else {
        (UpstreamTarget::Local(LocalHandler::Fallback), None)
    };

    RequestCtx::new(site_idx, upstream, retry)
}

fn route_site(
    site: &SiteConfig,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
) -> (UpstreamTarget, Option<RetryState>) {
    // Proxy routes take priority over static files.
    if let Some(proxy_cfg) = &site.proxy {
        if let Some(result) = resolve_proxy(proxy_cfg, path, counters) {
            return result;
        }
    }

    // Static files (only reached if no proxy route matched).
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
            );
        }
    }

    (UpstreamTarget::Local(LocalHandler::Fallback), None)
}

fn resolve_proxy(
    config: &ProxyConfig,
    path: &str,
    counters: &DashMap<String, AtomicUsize>,
) -> Option<(UpstreamTarget, Option<RetryState>)> {
    match config {
        ProxyConfig::Single(url) => {
            let addr = upstream::url_to_host_port(url)?;
            let tls = upstream::url_is_tls(url);
            let sni = if tls { upstream::url_host(url) } else { String::new() };
            Some((UpstreamTarget::Proxy { addr, tls, sni, strip_prefix: None }, None))
        }
        ProxyConfig::Routes(routes) => {
            let (route_key, route_target) = find_route(routes, path)?;
            let urls = upstream::target_urls(route_target);

            // Extract retry config (only available for `Full` targets).
            let retry_cfg = match route_target {
                ProxyRouteTarget::Full(cfg) => cfg.retry.as_ref(),
                _ => None,
            };

            let (chosen_url, retry_state) = if let Some(retry) = retry_cfg {
                // Use the round-robin counter to pick the starting position and
                // rotate the URL list so that upstream_peer() can simply walk it.
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
                };
                (first, Some(state))
            } else {
                let url = upstream::pick_round_robin(&urls, route_key, counters)?;
                (url, None)
            };

            let addr = upstream::url_to_host_port(&chosen_url)?;
            let tls = upstream::url_is_tls(&chosen_url);
            let sni = if tls { upstream::url_host(&chosen_url) } else { String::new() };
            let strip = if upstream::strip_prefix_enabled(route_target) {
                Some(route_key.trim_end_matches('/').to_string())
            } else {
                None
            };

            Some((
                UpstreamTarget::Proxy { addr, tls, sni, strip_prefix: strip },
                retry_state,
            ))
        }
    }
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

fn resolve_static_roots(cfg: &StaticConfig, path: &str) -> (Vec<PathBuf>, Option<String>) {
    match cfg {
        StaticConfig::Single(s) => (vec![PathBuf::from(s)], None),
        StaticConfig::Multi(v) => (v.iter().map(PathBuf::from).collect(), None),
        StaticConfig::Mapped(m) => {
            let mut best: Option<(&str, &str)> = None;
            for (prefix, root) in m {
                let norm = prefix.trim_end_matches('/');
                let matches = if norm.is_empty() {
                    true
                } else {
                    path == norm || path.starts_with(&format!("{norm}/"))
                };
                if matches {
                    let len = norm.len();
                    if best.is_none_or(|(b, _)| len > b.trim_end_matches('/').len()) {
                        best = Some((prefix.as_str(), root.as_str()));
                    }
                }
            }
            match best {
                Some((pfx, root)) => (vec![PathBuf::from(root)], Some(pfx.to_string())),
                None => (vec![], None),
            }
        }
    }
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

fn find_site_idx(config: &AppConfig, host: &str) -> Option<usize> {
    if config.sites.is_empty() {
        return None;
    }
    for (i, site) in config.sites.iter().enumerate() {
        if site.host.as_deref() == Some(host) {
            return Some(i);
        }
    }
    for (i, site) in config.sites.iter().enumerate() {
        if matches!(site.host.as_deref(), None | Some("*")) {
            return Some(i);
        }
    }
    Some(0)
}
