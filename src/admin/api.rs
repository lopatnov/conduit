use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::proxy::service::AppState;

pub struct AdminApiService {
    pub state: Arc<AppState>,
    pub bind: String,
}

#[async_trait]
impl BackgroundService for AdminApiService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
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

async fn reload_handler() -> Json<Value> {
    Json(json!({ "status": "not_implemented", "message": "hot reload — Phase 2.7" }))
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

async fn upstreams_handler() -> Json<Value> {
    Json(json!({ "upstreams": [] }))
}

async fn upstreams_add_handler() -> Json<Value> {
    Json(json!({ "status": "not_implemented", "message": "dynamic upstreams — Phase 2.5c" }))
}

async fn upstreams_remove_handler() -> Json<Value> {
    Json(json!({ "status": "not_implemented", "message": "dynamic upstreams — Phase 2.5c" }))
}

async fn upstreams_weight_handler() -> Json<Value> {
    Json(json!({ "status": "not_implemented", "message": "dynamic upstreams — Phase 2.5c" }))
}
