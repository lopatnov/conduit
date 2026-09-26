//! `GET /upstreams` and `POST /upstreams/{add,remove,weight}`.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::proxy::health;
use crate::proxy::service::AppState;

use super::upstreams_view::{
    build_flat_upstream_list, collect_site_proxy_entries, format_proxy_route_targets,
    resolve_runtime_targets, url_health_entry,
};

/// Request body for `/upstreams/add`, `/upstreams/remove`, and `/upstreams/weight`.
#[derive(Deserialize)]
pub(super) struct UpstreamModifyRequest {
    /// Proxy route path (e.g. `"/api"`).
    route: String,
    /// Full upstream URL (e.g. `"http://backend:4000"`).
    target: String,
    /// Target weight — required for `/weight`, optional for `/add` (default 1).
    weight: Option<u32>,
    /// Site label to scope the override (e.g. `"app.example.com:443"` or
    /// `"*:8080"`).  When absent the override uses the wildcard key `"*"` and
    /// applies to every site that serves this route (backward-compatible).
    site: Option<String>,
}

pub(super) async fn upstreams_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    use crate::config::schema::ProxyConfig;

    let registry = &state.upstream_health;
    let config = state.config.load();
    let mut routes: Vec<Value> = Vec::new();

    for site in &config.sites {
        let label = make_site_label(&site.host, site.port);

        // Single-target proxy shortcut — no route map, just one URL.
        if let Some(ProxyConfig::Single(url)) = &site.proxy {
            let target = url_health_entry(registry, url, 1, None);
            routes.push(json!({
                "site":     label.clone(),
                "path":     "/",
                "strategy": "round-robin",
                "targets":  [target],
            }));
        }

        // Multi-route entries from both the proxy map and the `routes` array.
        for (path, rt) in collect_site_proxy_entries(site) {
            let (strategy_str, targets) = format_proxy_route_targets(rt, registry);
            let targets = resolve_runtime_targets(registry, &label, &path, targets);
            routes.push(json!({
                "site":     label.clone(),
                "path":     path,
                "strategy": strategy_str,
                "targets":  targets,
            }));
        }
    }

    let flat = build_flat_upstream_list(registry);
    Json(json!({ "upstreams": flat, "routes": routes }))
}

/// Format a site's host+port as a human-readable label.
pub(super) fn make_site_label(host: &Option<String>, port: Option<u16>) -> String {
    health::site_label(host, port)
}

pub(super) async fn upstreams_add_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpstreamModifyRequest>,
) -> Json<Value> {
    let weight = req.weight.unwrap_or(1).max(1);
    let site = req.site.as_deref().unwrap_or("*");
    state
        .upstream_health
        .add_upstream(site, &req.route, &req.target, weight);
    Json(json!({
        "status": "ok",
        "site":   site,
        "route":  req.route,
        "target": req.target,
        "weight": weight,
    }))
}

pub(super) async fn upstreams_remove_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpstreamModifyRequest>,
) -> Json<Value> {
    let site = req.site.as_deref().unwrap_or("*");
    let removed = state
        .upstream_health
        .remove_upstream(site, &req.route, &req.target);
    Json(json!({
        "status": if removed { "ok" } else { "not_found" },
        "site":   site,
        "route":   req.route,
        "target":  req.target,
    }))
}

pub(super) async fn upstreams_weight_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpstreamModifyRequest>,
) -> Json<Value> {
    let Some(weight) = req.weight else {
        return Json(json!({ "status": "error", "message": "weight is required" }));
    };
    // Clamp to minimum 1 — weight 0 causes division-by-zero in WRR scheduling.
    let weight = weight.max(1);
    let site = req.site.as_deref().unwrap_or("*");
    let updated = state
        .upstream_health
        .set_weight(site, &req.route, &req.target, weight);
    Json(json!({
        "status": if updated { "ok" } else { "not_found" },
        "site":   site,
        "route":   req.route,
        "target":  req.target,
        "weight":  weight,
    }))
}
