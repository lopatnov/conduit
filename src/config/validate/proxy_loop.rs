//! Warn when a proxy target points back at a listener of this same Conduit instance.

// `url` is optional (issue #144, PR 4b): only the proxy-loop warning (`proxy`) and the
// forwardAuth-targets-the-Admin-API check (`forward-auth`) parse URLs.
#[cfg(feature = "proxy")]
use url::Url as ParsedUrl;

#[cfg(feature = "proxy")]
use super::cross_site::effective_port;
#[cfg(feature = "proxy")]
use super::warnings::sanitize_for_log;

use crate::config::schema::AppConfig;
#[cfg(feature = "proxy")]
use crate::config::schema::{ProxyRouteTarget, SiteConfig};

/// Warn when a proxy target points back to a port Conduit itself is listening on
/// -- the `proxy` variant.
#[cfg(feature = "proxy")]
pub(super) fn check_proxy_loop_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    let listening_ports: Vec<u16> = config.sites.iter().map(effective_port).collect();
    for (i, site) in config.sites.iter().enumerate() {
        let targets = collect_proxy_targets(site);
        for target in &targets {
            if let Some(port) = loopback_port(target) {
                if listening_ports.contains(&port) {
                    warnings.push(format!(
                        "sites[{i}] proxies to '{}' which appears to point back to \
                         Conduit itself (loopback + port {port} is a configured listening port) \
                         — this will create an infinite request loop.",
                        sanitize_for_log(target)
                    ));
                }
            }
        }
    }
}

/// No-`proxy` variant of [`check_proxy_loop_warnings`]: nothing is proxied
/// without the feature (`check_site_proxy_feature_warnings` already says every
/// `proxy` entry is ignored), so there is no loop to warn about -- and the
/// loopback parsing that needs the `url` crate is compiled out with it.
#[cfg(not(feature = "proxy"))]
pub(super) fn check_proxy_loop_warnings(_config: &AppConfig, _warnings: &mut Vec<String>) {}

/// Extract all URLs from a single `ProxyRouteTarget` into `out`.
#[cfg(feature = "proxy")]
fn collect_route_target_urls(target: &ProxyRouteTarget, out: &mut Vec<String>) {
    match target {
        ProxyRouteTarget::Url(u) => out.push(u.clone()),
        ProxyRouteTarget::RoundRobin(urls) => out.extend(urls.iter().cloned()),
        ProxyRouteTarget::Full(cfg) => {
            for t in &cfg.targets {
                let url = match t {
                    crate::config::schema::ProxyTarget::Simple(u) => u.clone(),
                    crate::config::schema::ProxyTarget::Weighted(w) => w.url.clone(),
                };
                out.push(url);
            }
        }
    }
}

/// Collect every proxy upstream URL configured for a site (from all proxy modes).
#[cfg(feature = "proxy")]
fn collect_proxy_targets(site: &SiteConfig) -> Vec<String> {
    use crate::config::schema::ProxyConfig;
    let mut out = Vec::new();

    if let Some(proxy) = &site.proxy {
        match proxy {
            ProxyConfig::Single(url) => out.push(url.clone()),
            ProxyConfig::Routes(routes) => {
                for target in routes.values() {
                    collect_route_target_urls(target, &mut out);
                }
            }
        }
    }

    // Also check routes[] array targets (same type: ProxyRouteTarget).
    if let Some(routes) = &site.routes {
        for route in routes {
            if let Some(target) = &route.proxy {
                collect_route_target_urls(target, &mut out);
            }
        }
    }
    out
}

/// If `url` has a loopback host (127.x.x.x, ::1, localhost), return its port.
#[cfg(feature = "proxy")]
fn loopback_port(url: &str) -> Option<u16> {
    let parsed = ParsedUrl::parse(url).ok()?;
    let host = parsed.host_str()?;
    let is_loopback = host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
        || host.starts_with("127.");
    if is_loopback {
        parsed.port().or_else(|| match parsed.scheme() {
            "https" => Some(443),
            _ => Some(80),
        })
    } else {
        None
    }
}
