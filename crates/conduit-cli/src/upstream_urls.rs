use crate::config::schema::{AppConfig, ProxyConfig, ProxyRouteTarget, ProxyTarget, SiteConfig};

/// Collect all unique upstream URLs from an `AppConfig`.
pub(crate) fn collect_upstream_urls(app: &AppConfig) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut urls = Vec::new();
    for site in &app.sites {
        for url in site_upstream_urls(site) {
            if seen.insert(url.clone()) {
                urls.push(url);
            }
        }
    }
    urls
}

/// Return every upstream URL referenced by a single site's `proxy` and `routes`.
fn site_upstream_urls(site: &SiteConfig) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(proxy) = &site.proxy {
        out.extend(extract_proxy_urls(proxy));
    }
    if let Some(routes) = &site.routes {
        for route in routes {
            if let Some(proxy_target) = &route.proxy {
                out.extend(extract_route_target_urls(proxy_target));
            }
        }
    }
    out
}

/// Flatten a `ProxyConfig` into a list of raw URL strings.
fn extract_proxy_urls(proxy: &ProxyConfig) -> Vec<String> {
    match proxy {
        ProxyConfig::Single(url) => vec![url.clone()],
        ProxyConfig::Routes(routes) => routes
            .values()
            .flat_map(extract_route_target_urls)
            .collect(),
    }
}

/// Flatten a `ProxyRouteTarget` into raw URL strings.
fn extract_route_target_urls(target: &ProxyRouteTarget) -> Vec<String> {
    match target {
        ProxyRouteTarget::Url(url) => vec![url.clone()],
        ProxyRouteTarget::RoundRobin(urls) => urls.clone(),
        ProxyRouteTarget::Full(cfg) => collect_full_target_urls(cfg),
    }
}

/// Collect every URL from a `Full` proxy route config (targets + group targets).
fn collect_full_target_urls(cfg: &crate::config::schema::ProxyRouteConfig) -> Vec<String> {
    let mut out: Vec<String> = cfg.targets.iter().map(proxy_target_url).collect();
    if let Some(groups) = &cfg.groups {
        for group in groups {
            out.extend(group.targets.iter().map(proxy_target_url));
        }
    }
    out
}

/// Extract the URL string from either form of `ProxyTarget`.
fn proxy_target_url(t: &ProxyTarget) -> String {
    match t {
        ProxyTarget::Simple(url) => url.clone(),
        ProxyTarget::Weighted(w) => w.url.clone(),
    }
}
