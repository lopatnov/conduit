use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::proxy::health;
use crate::proxy::service::AppState;

/// Request body for `/upstreams/add`, `/upstreams/remove`, and `/upstreams/weight`.
#[derive(Deserialize)]
struct UpstreamModifyRequest {
    /// Proxy route path (e.g. `"/api"`).
    route: String,
    /// Full upstream URL (e.g. `"http://backend:4000"`).
    target: String,
    /// Target weight — required for `/weight`, optional for `/add` (default 1).
    weight: Option<u32>,
}

pub struct AdminApiService {
    pub state: Arc<AppState>,
    pub bind: String,
}

#[async_trait]
impl BackgroundService for AdminApiService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        // Spawn a background task that evicts stale rate-limiter entries every 60 s.
        {
            let limiter = self.state.rate_limiter.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    crate::filter::rate_limit::cleanup(&limiter);
                }
            });
        }

        // Spawn upstream health check tasks for every route that has healthCheck configured.
        {
            let config = self.state.config.load();
            health::spawn_health_checks(self.state.upstream_health.clone(), &config);
        }

        let app = build_router(self.state.clone());
        let listener = match TcpListener::bind(&self.bind).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("admin API failed to bind {}: {e}", self.bind);
                return;
            }
        };
        let addr = listener.local_addr().ok();
        if let Some(addr) = addr {
            tracing::info!("admin API listening on http://{addr}");
        }
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown.changed().await.ok();
            })
            .await
            .ok();
    }
}

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/status", get(status_handler))
        .route("/reload", post(reload_handler))
        .route("/shutdown", post(shutdown_handler))
        .route("/upstreams", get(upstreams_handler))
        .route("/upstreams/add", post(upstreams_add_handler))
        .route("/upstreams/remove", post(upstreams_remove_handler))
        .route("/upstreams/weight", post(upstreams_weight_handler))
        .with_state(state)
}

async fn status_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "running",
        "inflight": state.inflight.load(Ordering::Relaxed),
    }))
}

async fn reload_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Clear runtime upstream overrides so the config file is the single source
    // of truth.  Full hot config reload (swapping AppConfig via ArcSwap) is
    // Phase 2.7; here we only handle the in-memory side of reload.
    state.upstream_health.clear_overrides();
    Json(json!({
        "status": "partial",
        "message": "runtime upstream overrides cleared; full hot reload — Phase 2.7"
    }))
}

async fn shutdown_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
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

async fn upstreams_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let registry = &state.upstream_health;
    let mut entries: Vec<Value> = registry
        .statuses
        .iter()
        .map(|e| {
            json!({
                "url":                   e.key(),
                "healthy":               e.value().healthy,
                "latency_ms":            e.value().latency_ms,
                "consecutive_failures":  e.value().consecutive_failures,
                "consecutive_successes": e.value().consecutive_successes,
            })
        })
        .collect();
    // Stable sort for deterministic output.
    entries.sort_by(|a, b| a["url"].as_str().cmp(&b["url"].as_str()));
    Json(json!({ "upstreams": entries }))
}

async fn upstreams_add_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpstreamModifyRequest>,
) -> Json<Value> {
    let weight = req.weight.unwrap_or(1).max(1);
    state
        .upstream_health
        .add_upstream(&req.route, &req.target, weight);
    Json(json!({
        "status": "ok",
        "route":  req.route,
        "target": req.target,
        "weight": weight,
    }))
}

async fn upstreams_remove_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpstreamModifyRequest>,
) -> Json<Value> {
    let removed = state
        .upstream_health
        .remove_upstream(&req.route, &req.target);
    Json(json!({
        "status": if removed { "ok" } else { "not_found" },
        "route":   req.route,
        "target":  req.target,
    }))
}

async fn upstreams_weight_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpstreamModifyRequest>,
) -> Json<Value> {
    let Some(weight) = req.weight else {
        return Json(json!({ "status": "error", "message": "weight is required" }));
    };
    // Clamp to minimum 1 — weight 0 causes division-by-zero in WRR scheduling.
    let weight = weight.max(1);
    let updated = state
        .upstream_health
        .set_weight(&req.route, &req.target, weight);
    Json(json!({
        "status": if updated { "ok" } else { "not_found" },
        "route":   req.route,
        "target":  req.target,
        "weight":  weight,
    }))
}
