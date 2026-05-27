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

use crate::config;
use crate::config::schema::LoggingConfig;
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
            let redis_rl = self.state.redis_rate_limiter.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    crate::filter::rate_limit::cleanup(&limiter);
                    // Also clean up the Redis fallback map if in use.
                    if let Some(ref rrl) = redis_rl {
                        rrl.cleanup_fallback();
                    }
                }
            });
        }

        // Spawn upstream health check tasks for every route that has healthCheck configured.
        {
            let config = self.state.config.load();
            health::spawn_health_checks(self.state.upstream_health.clone(), &config);
        }

        // Spawn the browser hot-reload file watcher if any site has hotReload enabled.
        {
            let config = self.state.config.load();
            if let Some((dirs, extensions)) =
                crate::handler::hot_reload::build_watch_config(&config)
            {
                let reload_tx = self.state.hot_reload_tx.clone();
                tokio::spawn(crate::handler::hot_reload::run_file_watcher(
                    dirs, extensions, reload_tx,
                ));
            }
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
    // Re-parse the config file.
    let new_config = match config::load_config(&state.config_path) {
        Ok(c) => c,
        Err(e) => {
            return Json(json!({
                "status": "error",
                "message": format!("failed to parse config: {e}"),
            }));
        }
    };

    // Detect fields that require a restart (cold changes).
    let cold_fields = detect_cold_changes(&state.config.load(), &new_config);
    if !cold_fields.is_empty() {
        return Json(json!({
            "status": "error",
            "message": "cold fields changed — restart required",
            "cold_fields": cold_fields,
        }));
    }

    // Switch log writer if any site's logging.file path changed.
    {
        let old_cfg = state.config.load();
        for (i, new_site) in new_config.sites.iter().enumerate() {
            let old_file = old_cfg.sites.get(i).and_then(|s| log_file_path(&s.logging));
            let new_file = log_file_path(&new_site.logging);
            if old_file != new_file {
                match new_file {
                    Some(path) => {
                        if let Err(e) = state.log_writer.switch_file(path) {
                            tracing::warn!(path, "reload: failed to switch log file: {e}");
                        }
                    }
                    None => state.log_writer.use_stdout(),
                }
            }
        }
    }

    // Spawn health-check tasks for any newly-configured routes.
    health::spawn_health_checks(state.upstream_health.clone(), &new_config);

    // Apply: hot-swap config, clear runtime upstream overrides, reset rate limiter.
    state.config.store(Arc::new(new_config));
    state.upstream_health.clear_overrides();
    state.rate_limiter.clear();

    Json(json!({ "status": "ok", "message": "config reloaded" }))
}

/// Return the list of field paths that changed between `old` and `new` and
/// require a server restart (cold fields).
///
/// Cold fields: `global.workers`, `global.backlog`, `global.admin.bind`,
/// `sites[N].port`, `sites[N].tls.cert`, `sites[N].tls.key`.
fn detect_cold_changes(
    old: &crate::config::schema::AppConfig,
    new: &crate::config::schema::AppConfig,
) -> Vec<String> {
    let mut cold = Vec::new();

    // global.workers / global.backlog / global.admin.bind
    let old_g = old.global.as_ref();
    let new_g = new.global.as_ref();
    if old_g.and_then(|g| g.workers) != new_g.and_then(|g| g.workers) {
        cold.push("global.workers".to_string());
    }
    if old_g.and_then(|g| g.backlog) != new_g.and_then(|g| g.backlog) {
        cold.push("global.backlog".to_string());
    }
    let old_bind = old_g
        .and_then(|g| g.admin.as_ref())
        .and_then(|a| a.bind.as_deref());
    let new_bind = new_g
        .and_then(|g| g.admin.as_ref())
        .and_then(|a| a.bind.as_deref());
    if old_bind != new_bind {
        cold.push("global.admin.bind".to_string());
    }

    // per-site cold fields: port, tls.cert, tls.key
    let old_sites = &old.sites;
    let new_sites = &new.sites;
    let n = old_sites.len().max(new_sites.len());
    for i in 0..n {
        let o = old_sites.get(i);
        let nw = new_sites.get(i);
        // port
        if o.and_then(|s| s.port) != nw.and_then(|s| s.port) {
            cold.push(format!("sites[{i}].port"));
        }
        // tls.cert / tls.key (manual cert, not ACME)
        let old_cert = o
            .and_then(|s| s.tls.as_ref())
            .and_then(|t| t.cert.as_deref());
        let new_cert = nw
            .and_then(|s| s.tls.as_ref())
            .and_then(|t| t.cert.as_deref());
        if old_cert != new_cert {
            cold.push(format!("sites[{i}].tls.cert"));
        }
        let old_key = o
            .and_then(|s| s.tls.as_ref())
            .and_then(|t| t.key.as_deref());
        let new_key = nw
            .and_then(|s| s.tls.as_ref())
            .and_then(|t| t.key.as_deref());
        if old_key != new_key {
            cold.push(format!("sites[{i}].tls.key"));
        }
    }

    cold
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
    use crate::config::schema::{LoadBalanceStrategy, ProxyConfig, ProxyRouteTarget, ProxyTarget};

    let registry = &state.upstream_health;
    let config = state.config.load();

    // Helper: look up health entry for a URL.
    let health_of = |url: &str| -> Value {
        match registry.statuses.get(url) {
            Some(e) => json!({
                "healthy":               e.healthy,
                "latency_ms":            e.latency_ms,
                "consecutive_failures":  e.consecutive_failures,
                "consecutive_successes": e.consecutive_successes,
            }),
            // URL has no health-check probe configured — unknown status.
            None => json!({
                "healthy":               Value::Null,
                "latency_ms":            Value::Null,
                "consecutive_failures":  0,
                "consecutive_successes": 0,
            }),
        }
    };

    // Build per-route view from config.
    let mut routes: Vec<Value> = Vec::new();
    for site in &config.sites {
        let site_label = match (&site.host, site.port) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.clone(),
            (None, Some(p)) => format!("*:{p}"),
            (None, None) => "*".to_string(),
        };

        // Collect proxy targets from both top-level `proxy` and `routes` array.
        // Each entry is (match_path, route_target).
        let mut proxy_entries: Vec<(String, &ProxyRouteTarget)> = Vec::new();

        if let Some(proxy_cfg) = &site.proxy {
            match proxy_cfg {
                ProxyConfig::Single(url) => {
                    // Wrap the single URL as a Url variant and push directly.
                    let mut h = health_of(url);
                    h["url"] = json!(url);
                    routes.push(json!({
                        "site":     site_label.clone(),
                        "path":     "/",
                        "strategy": "round-robin",
                        "targets":  [h],
                    }));
                }
                ProxyConfig::Routes(route_map) => {
                    for (path, rt) in route_map {
                        proxy_entries.push((path.clone(), rt));
                    }
                }
            }
        }

        // Also collect proxy targets from the `routes` array (Phase 3.6).
        if let Some(route_list) = &site.routes {
            for rc in route_list {
                if let Some(rt) = &rc.proxy {
                    let path = rc.r#match.path.clone().unwrap_or_else(|| "/**".to_string());
                    proxy_entries.push((path, rt));
                }
            }
        }

        for (path, route_target) in &proxy_entries {
            let (strategy_str, targets): (&str, Vec<Value>) = match route_target {
                ProxyRouteTarget::Url(url) => {
                    let mut h = health_of(url);
                    h["url"] = json!(url);
                    h["weight"] = json!(1u32);
                    ("round-robin", vec![h])
                }
                ProxyRouteTarget::RoundRobin(urls) => {
                    let tgts = urls
                        .iter()
                        .map(|url| {
                            let mut h = health_of(url);
                            h["url"] = json!(url);
                            h["weight"] = json!(1u32);
                            h
                        })
                        .collect();
                    ("round-robin", tgts)
                }
                ProxyRouteTarget::Full(cfg) => {
                    let strat = match cfg
                        .strategy
                        .as_ref()
                        .unwrap_or(&LoadBalanceStrategy::RoundRobin)
                    {
                        LoadBalanceStrategy::RoundRobin => "round-robin",
                        LoadBalanceStrategy::WeightedRoundRobin => "weighted-round-robin",
                        LoadBalanceStrategy::Random => "random",
                        LoadBalanceStrategy::LeastConn => "least-conn",
                        LoadBalanceStrategy::LeastResponseTime => "least-response-time",
                        LoadBalanceStrategy::IpHash => "ip-hash",
                        LoadBalanceStrategy::ConsistentHash => "consistent-hash",
                    };
                    let tgts = cfg
                        .targets
                        .iter()
                        .map(|t| {
                            let (url, weight) = match t {
                                ProxyTarget::Simple(u) => (u.as_str(), 1u32),
                                ProxyTarget::Weighted(w) => (w.url.as_str(), w.weight),
                            };
                            let mut h = health_of(url);
                            h["url"] = json!(url);
                            h["weight"] = json!(weight);
                            h
                        })
                        .collect();
                    (strat, tgts)
                }
            };

            // Include any runtime overrides for this route.
            let override_targets: Vec<Value> = registry
                .get_override_targets(path)
                .unwrap_or_default()
                .iter()
                .map(|(url, weight)| {
                    let mut h = health_of(url);
                    h["url"] = json!(url);
                    h["weight"] = json!(weight);
                    h["runtime"] = json!(true);
                    h
                })
                .collect();

            let all_targets: Vec<Value> = if override_targets.is_empty() {
                targets
            } else {
                override_targets
            };

            routes.push(json!({
                "site":     site_label.clone(),
                "path":     path,
                "strategy": strategy_str,
                "targets":  all_targets,
            }));
        }
    }

    // Backward-compatible flat list: all URLs with known health status.
    let mut flat: Vec<Value> = registry
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
    flat.sort_by(|a, b| a["url"].as_str().cmp(&b["url"].as_str()));

    Json(json!({ "upstreams": flat, "routes": routes }))
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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the `logging.file` path from a `LoggingConfig`, if any.
fn log_file_path(cfg: &Option<LoggingConfig>) -> Option<&str> {
    match cfg {
        Some(LoggingConfig::Options(opts)) => opts.file.as_deref(),
        _ => None,
    }
}
