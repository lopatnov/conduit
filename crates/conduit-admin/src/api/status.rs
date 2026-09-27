//! `GET /status` and `POST /shutdown`.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use conduit_runtime::proxy::service::AppState;

pub(super) async fn status_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let config = state.config.load();
    let site_count = config.sites.len();
    // Count total configured upstreams (across all proxy routes).
    let upstream_count: usize = config
        .sites
        .iter()
        .filter_map(|s| s.proxy.as_ref())
        .map(|p| match p {
            conduit_config::schema::ProxyConfig::Single(_) => 1,
            conduit_config::schema::ProxyConfig::Routes(routes) => routes
                .values()
                .map(|target| match target {
                    conduit_config::schema::ProxyRouteTarget::Url(_) => 1,
                    conduit_config::schema::ProxyRouteTarget::RoundRobin(v) => v.len(),
                    conduit_config::schema::ProxyRouteTarget::Full(cfg) => cfg.targets.len(),
                })
                .sum(),
        })
        .sum();

    let healthy_upstreams = state
        .upstream_health
        .statuses
        .iter()
        .filter(|e| e.healthy)
        .count();
    let total_upstreams = state.upstream_health.statuses.len();

    Json(json!({
        "status": "running",
        "inflight": state.inflight.load(Ordering::Relaxed),
        "retry_inflight": state.retry_inflight.load(Ordering::Relaxed),
        "sites": site_count,
        "configured_upstreams": upstream_count,
        "healthy_upstreams": healthy_upstreams,
        "total_probed_upstreams": total_upstreams,
        "config_path": state.config_path.display().to_string(),
    }))
}

pub(super) async fn shutdown_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let timeout = {
        let cfg = state.config.load();
        cfg.global
            .as_ref()
            .and_then(|g| g.shutdown_timeout_secs)
            .unwrap_or(30)
    };
    let inflight = state.inflight.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
        loop {
            if inflight.load(Ordering::Relaxed) == 0 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        std::process::exit(0);
    });
    Json(json!({ "status": "shutting_down" }))
}
