//! The JSON view of the upstreams that `GET /upstreams` and the `conduit upstreams` CLI read.

use serde_json::{json, Value};

use crate::proxy::health;

// ── upstreams_handler helpers ─────────────────────────────────────────────────

/// Build a JSON object that combines a target URL with its health-check status.
pub(super) fn url_health_entry(
    registry: &health::UpstreamRegistry,
    url: &str,
    weight: u32,
    group: Option<&str>,
) -> Value {
    let health = match registry.statuses.get(url) {
        Some(e) => json!({
            "healthy":               e.healthy,
            "latency_ms":            e.latency_ms,
            "consecutive_failures":  e.consecutive_failures,
            "consecutive_successes": e.consecutive_successes,
        }),
        None => json!({
            "healthy":               Value::Null,
            "latency_ms":            Value::Null,
            "consecutive_failures":  0,
            "consecutive_successes": 0,
        }),
    };
    let mut entry = json!({
        "url":    url,
        "weight": weight,
    });
    // Merge health fields into the entry object.
    if let (Some(obj), Some(h_obj)) = (entry.as_object_mut(), health.as_object()) {
        for (k, v) in h_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    if let Some(g) = group {
        entry["group"] = json!(g);
    }
    entry
}

/// Map a `LoadBalanceStrategy` to its JSON-API string form.
pub(super) fn strategy_label(s: &crate::config::schema::LoadBalanceStrategy) -> &'static str {
    use crate::config::schema::LoadBalanceStrategy as S;
    match s {
        S::RoundRobin => "round-robin",
        S::WeightedRoundRobin => "weighted-round-robin",
        S::Random => "random",
        S::LeastConn => "least-conn",
        S::LeastResponseTime => "least-response-time",
        S::IpHash => "ip-hash",
        S::ConsistentHash => "consistent-hash",
        S::P2c => "p2c",
    }
}

/// Extract `(url, weight)` from a `ProxyTarget`.
pub(super) fn proxy_target_url_weight(t: &crate::config::schema::ProxyTarget) -> (&str, u32) {
    use crate::config::schema::ProxyTarget;
    match t {
        ProxyTarget::Simple(u) => (u.as_str(), 1),
        ProxyTarget::Weighted(w) => (w.url.as_str(), w.weight),
    }
}

/// Convert a `ProxyRouteConfig`'s targets (flat or grouped) to JSON entries.
pub(super) fn format_full_config_targets(
    cfg: &crate::config::schema::ProxyRouteConfig,
    registry: &health::UpstreamRegistry,
) -> Vec<Value> {
    if let Some(groups) = &cfg.groups {
        groups
            .iter()
            .flat_map(|g| {
                g.targets.iter().map(|t| {
                    let (url, w) = proxy_target_url_weight(t);
                    url_health_entry(registry, url, w, Some(&g.name))
                })
            })
            .collect()
    } else {
        cfg.targets
            .iter()
            .map(|t| {
                let (url, w) = proxy_target_url_weight(t);
                url_health_entry(registry, url, w, None)
            })
            .collect()
    }
}

/// Convert a `ProxyRouteTarget` to `(strategy_label, target_list)`.
pub(super) fn format_proxy_route_targets(
    rt: &crate::config::schema::ProxyRouteTarget,
    registry: &health::UpstreamRegistry,
) -> (&'static str, Vec<Value>) {
    use crate::config::schema::{LoadBalanceStrategy, ProxyRouteTarget};
    match rt {
        ProxyRouteTarget::Url(url) => (
            "round-robin",
            vec![url_health_entry(registry, url, 1, None)],
        ),
        ProxyRouteTarget::RoundRobin(urls) => {
            let tgts = urls
                .iter()
                .map(|u| url_health_entry(registry, u, 1, None))
                .collect();
            ("round-robin", tgts)
        }
        ProxyRouteTarget::Full(cfg) => {
            let strat = strategy_label(
                cfg.strategy
                    .as_ref()
                    .unwrap_or(&LoadBalanceStrategy::RoundRobin),
            );
            (strat, format_full_config_targets(cfg, registry))
        }
    }
}

/// Collect `(path, route_target)` pairs from a site's proxy map and routes array.
pub(super) fn collect_site_proxy_entries(
    site: &crate::config::schema::SiteConfig,
) -> Vec<(String, &crate::config::schema::ProxyRouteTarget)> {
    use crate::config::schema::ProxyConfig;
    let mut entries = Vec::new();
    if let Some(ProxyConfig::Routes(route_map)) = &site.proxy {
        for (path, rt) in route_map {
            entries.push((path.clone(), rt));
        }
    }
    if let Some(route_list) = &site.routes {
        for rc in route_list {
            if let Some(rt) = &rc.proxy {
                let path = rc.r#match.path.clone().unwrap_or_else(|| "/**".to_string());
                entries.push((path, rt));
            }
        }
    }
    entries
}

/// Replace config targets with runtime overrides when present.
pub(super) fn resolve_runtime_targets(
    registry: &health::UpstreamRegistry,
    site_label: &str,
    path: &str,
    config_targets: Vec<Value>,
) -> Vec<Value> {
    let overrides: Vec<Value> = registry
        .get_override_targets(site_label, path)
        .unwrap_or_default()
        .iter()
        .map(|(url, weight)| {
            let mut h = url_health_entry(registry, url, *weight, None);
            h["runtime"] = json!(true);
            h
        })
        .collect();
    if overrides.is_empty() {
        config_targets
    } else {
        overrides
    }
}

/// Build the backward-compatible flat list of all known upstream URLs.
pub(super) fn build_flat_upstream_list(registry: &health::UpstreamRegistry) -> Vec<Value> {
    let mut flat: Vec<Value> = registry
        .statuses
        .iter()
        .map(|e| {
            let url = e.key().as_str();
            let active_conns = registry.conn_load(url);
            // Compute ejection once with a single wall-clock read so that the
            // "state" and "ejected" fields are always consistent in the same item.
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let is_ejected = e
                .value()
                .ejected_until_secs
                .is_some_and(|until| until > now_secs);
            let state = if is_ejected {
                "ejected"
            } else if e.value().half_open {
                "half-open"
            } else if !e.value().healthy {
                "unhealthy"
            } else if active_conns > 0 {
                "busy"
            } else {
                "healthy"
            };
            json!({
                "url":                   url,
                "healthy":               e.value().healthy,
                "state":                 state,
                "latency_ms":            e.value().latency_ms,
                "ewma_latency_ms":       (e.value().ewma_latency_us / 1000.0) as u64,
                "consecutive_failures":  e.value().consecutive_failures,
                "consecutive_successes": e.value().consecutive_successes,
                "consecutive_5xx":       e.value().consecutive_5xx,
                "active_connections":    active_conns,
                "ejected":               is_ejected,
                "responses": {
                    "2xx": e.value().responses_2xx,
                    "4xx": e.value().responses_4xx,
                    "5xx": e.value().responses_5xx,
                },
                "selected": {
                    "total": e.value().selected_total,
                    "last_secs": e.value().selected_last_secs,
                },
            })
        })
        .collect();
    flat.sort_by(|a, b| a["url"].as_str().cmp(&b["url"].as_str()));
    flat
}
