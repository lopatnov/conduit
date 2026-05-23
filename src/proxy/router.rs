use std::path::PathBuf;
use std::sync::Arc;

use crate::config::schema::{AppConfig, SiteConfig, StaticConfig};
use crate::proxy::ctx::{LocalHandler, RequestCtx, UpstreamTarget};

pub fn route_request(config: &AppConfig, host: &str, path: &str) -> RequestCtx {
    let site_idx = find_site_idx(config, host).unwrap_or(0);
    let site = config.sites.get(site_idx);

    let upstream = if is_health_path(site, path) {
        UpstreamTarget::Local(LocalHandler::Health)
    } else if let Some(site) = site {
        route_site(site, path)
    } else {
        UpstreamTarget::Local(LocalHandler::Fallback)
    };

    RequestCtx::new(site_idx, upstream)
}

fn route_site(site: &SiteConfig, path: &str) -> UpstreamTarget {
    if let Some(static_cfg) = &site.static_files {
        let options = Arc::new(site.static_options.clone().unwrap_or_default());
        let (roots, strip_prefix) = resolve_static_roots(static_cfg, path);
        if !roots.is_empty() {
            return UpstreamTarget::Local(LocalHandler::StaticFile {
                roots,
                options,
                strip_prefix,
            });
        }
    }

    if let Some(proxy) = &site.proxy {
        // Proxy routing implemented in Phase 1.7
        let _ = proxy;
    }

    UpstreamTarget::Local(LocalHandler::Fallback)
}

fn resolve_static_roots(
    cfg: &StaticConfig,
    path: &str,
) -> (Vec<PathBuf>, Option<String>) {
    match cfg {
        StaticConfig::Single(s) => (vec![PathBuf::from(s)], None),
        StaticConfig::Multi(v) => (v.iter().map(PathBuf::from).collect(), None),
        StaticConfig::Mapped(m) => {
            // Longest matching prefix wins
            let mut best: Option<(&str, &str)> = None;
            for (prefix, root) in m {
                let norm = prefix.trim_end_matches('/');
                let matches = if norm.is_empty() {
                    true // "/" matches everything
                } else {
                    path == norm || path.starts_with(&format!("{norm}/"))
                };
                if matches {
                    let len = norm.len();
                    if best.map_or(true, |(b, _)| len > b.trim_end_matches('/').len()) {
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
