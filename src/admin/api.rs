use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::Query;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::config;
use crate::config::schema::LoggingConfig;
use crate::config::validate;

/// Constant-time byte-slice equality to prevent timing attacks on Bearer tokens.
///
/// A naive `a == b` short-circuits on the first differing byte, leaking how
/// many leading bytes the attacker guessed correctly.  This function always
/// inspects every byte of both slices regardless of where they diverge.
fn subtle_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    // Length check is intentionally NOT constant-time: a different length
    // does not help an attacker who must guess the full token anyway, and
    // this avoids allocating a padded buffer.
    a.len() == b.len() && a.ct_eq(b).into()
}

// ── Typed error responses ─────────────────────────────────────────────────────

/// Typed error for Admin API handlers.
///
/// Implements [`IntoResponse`] so handlers can return `Result<T, AdminError>`
/// and get consistent JSON error bodies without ad-hoc `json!({ "status": "error" })`.
#[derive(Debug)]
pub enum AdminError {
    /// 400 Bad Request — invalid input (e.g. bad URL format, missing field).
    BadRequest(String),
    /// 500 Internal Server Error — config parse / validation failure.
    ServerError(String),
    /// 400 — reload rejected because cold fields changed; includes the list of
    /// fields as a top-level JSON array so callers can inspect them without
    /// parsing the human-readable `message` string.
    ColdFieldsChanged {
        message: String,
        fields: Vec<String>,
    },
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        match self {
            AdminError::BadRequest(m) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "status": "error", "message": m })),
            )
                .into_response(),
            AdminError::ServerError(m) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": m })),
            )
                .into_response(),
            AdminError::ColdFieldsChanged { message, fields } => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status":      "error",
                    "message":     message,
                    "cold_fields": fields,
                })),
            )
                .into_response(),
        }
    }
}

/// Shorthand result type for Admin API handlers.
pub type AdminResult<T> = Result<T, AdminError>;
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
    /// Site label to scope the override (e.g. `"app.example.com:443"` or
    /// `"*:8080"`).  When absent the override uses the wildcard key `"*"` and
    /// applies to every site that serves this route (backward-compatible).
    site: Option<String>,
}

pub struct AdminApiService {
    pub state: Arc<AppState>,
    /// Address to bind the Admin HTTP server on, e.g. `"127.0.0.1:2019"`.
    ///
    /// `None` when `global.admin` is absent from the config — in that case
    /// the internal background tasks (health checks, rate-limiter cleanup,
    /// hot-reload watcher) still run, but no HTTP endpoint is exposed.
    pub bind: Option<String>,
}

#[async_trait]
impl BackgroundService for AdminApiService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        // Spawn a background task that evicts stale rate-limiter entries every 60 s.
        {
            let limiter = self.state.rate_limiter.clone();
            #[cfg(feature = "redis")]
            let redis_rl = self.state.redis_rate_limiter.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    crate::filter::rate_limit::cleanup(&limiter);
                    // Also clean up the Redis fallback map if in use.
                    #[cfg(feature = "redis")]
                    if let Some(ref rrl) = redis_rl {
                        rrl.cleanup_fallback();
                    }
                }
            });
        }

        // Spawn event-loop lag monitor — updates conduit_eventloop_lag_ms every second.
        //
        // Uses a yield-probe technique: schedule a `yield_now()` and measure how long
        // the executor takes to resume.  This directly captures scheduling latency
        // (event-loop lag) without requiring `tokio_unstable` or external crates.
        // A rising value indicates CPU saturation or I/O stall in the runtime.
        #[cfg(feature = "tokio-metrics")]
        {
            use crate::proxy::service::ConduitMetrics;
            let gauge = ConduitMetrics::global().eventloop_lag_ms.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
                // Under heavy load the tick may be missed; Skip prevents a burst
                // of catch-up probes that would skew the lag metric.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    ticker.tick().await;
                    // Measure how long between yielding and being resumed.
                    let before = std::time::Instant::now();
                    tokio::task::yield_now().await;
                    let lag_ms = before.elapsed().as_secs_f64() * 1000.0;
                    gauge.set(lag_ms);
                }
            });
        }

        // Spawn upstream health check tasks for every route that has healthCheck configured.
        {
            let config = self.state.config.load();
            health::spawn_health_checks(self.state.upstream_health.clone(), &config);
            // Warm up connection pools for routes with prewarmConnections set.
            health::spawn_connection_warmup(&config);
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

        // HTTP Admin server — only starts when global.admin.bind is configured.
        let bind_addr = match &self.bind {
            Some(addr) => addr.clone(),
            None => {
                // No admin config: background tasks run, HTTP server does not.
                shutdown.changed().await.ok();
                return;
            }
        };

        let app = build_router(self.state.clone());
        let listener = match TcpListener::bind(&bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("admin API failed to bind {bind_addr}: {e}");
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
    // Build the protected routes first.
    let protected = Router::new()
        .route("/status", get(status_handler))
        .route("/reload", post(reload_handler))
        .route("/shutdown", post(shutdown_handler))
        .route("/upstreams", get(upstreams_handler))
        .route("/upstreams/add", post(upstreams_add_handler))
        .route("/upstreams/remove", post(upstreams_remove_handler))
        .route("/upstreams/weight", post(upstreams_weight_handler))
        .route("/cache/purge", delete(cache_purge_handler))
        .route("/rate-limits", get(rate_limits_handler))
        .route("/ip-deny", post(ip_deny_add_handler))
        .route("/ip-deny", delete(ip_deny_remove_handler))
        .route("/certs/reload", post(certs_reload_handler));

    // Wrap with bearer-token auth middleware.
    // Read the token from the live config on every request so that POST /reload
    // can add, remove, or rotate the admin token without a process restart.
    let auth_state = state.clone();
    protected
        .layer(axum::middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let state = auth_state.clone();
                async move {
                    // Read the current token from the live (ArcSwap) config.
                    let required_token = state
                        .config
                        .load()
                        .global
                        .as_ref()
                        .and_then(|g| g.admin.as_ref())
                        .and_then(|a| a.token.clone());

                    let Some(token) = required_token else {
                        // No token configured — allow all requests.
                        return Ok(next.run(request).await);
                    };

                    let auth = request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    let provided = auth.strip_prefix("Bearer ").map(str::trim).unwrap_or("");
                    // Constant-time comparison prevents timing-based brute force.
                    if subtle_eq(provided.as_bytes(), token.as_bytes()) {
                        Ok(next.run(request).await)
                    } else {
                        Err(StatusCode::UNAUTHORIZED)
                    }
                }
            },
        ))
        .with_state(state)
}

async fn status_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let config = state.config.load();
    let site_count = config.sites.len();
    // Count total configured upstreams (across all proxy routes).
    let upstream_count: usize = config
        .sites
        .iter()
        .filter_map(|s| s.proxy.as_ref())
        .map(|p| match p {
            crate::config::schema::ProxyConfig::Single(_) => 1,
            crate::config::schema::ProxyConfig::Routes(routes) => routes
                .values()
                .map(|target| match target {
                    crate::config::schema::ProxyRouteTarget::Url(_) => 1,
                    crate::config::schema::ProxyRouteTarget::RoundRobin(v) => v.len(),
                    crate::config::schema::ProxyRouteTarget::Full(cfg) => cfg.targets.len(),
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

async fn reload_handler(State(state): State<Arc<AppState>>) -> AdminResult<Json<Value>> {
    // Re-parse the config file.
    let new_config = config::load_config(&state.config_path)
        .map_err(|e| AdminError::ServerError(format!("failed to parse config: {e}")))?;

    // Validate the new config before applying it.
    let errors = validate::validate(&new_config);
    if !errors.is_empty() {
        return Err(AdminError::ServerError(format!(
            "config validation failed: {}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    // Collect feature warnings once — used for both logging and the response body.
    let fw: Vec<String> = validate::feature_warnings(&new_config);
    for w in &fw {
        tracing::warn!("feature not compiled in: {w}");
    }

    // Detect fields that require a restart (cold changes).
    // Return 400 so callers get a non-2xx status — returning 200 with
    // "status":"error" in the body would let automated tools treat a rejected
    // reload as success.
    let cold_fields = detect_cold_changes(&state.config.load(), &new_config);
    if !cold_fields.is_empty() {
        // HTTP 400 so callers get a non-2xx status; cold_fields is a separate
        // JSON array so callers can inspect fields without parsing the message.
        return Err(AdminError::ColdFieldsChanged {
            message: format!(
                "cold fields changed — restart required: {}",
                cold_fields.join(", ")
            ),
            fields: cold_fields,
        });
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

    let mut resp = json!({ "status": "ok", "message": "config reloaded" });
    if !fw.is_empty() {
        resp["warnings"] = json!(fw);
    }
    Ok(Json(resp))
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

// ── upstreams_handler helpers ─────────────────────────────────────────────────

/// Build a JSON object that combines a target URL with its health-check status.
fn url_health_entry(
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
fn strategy_label(s: &crate::config::schema::LoadBalanceStrategy) -> &'static str {
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
fn proxy_target_url_weight(t: &crate::config::schema::ProxyTarget) -> (&str, u32) {
    use crate::config::schema::ProxyTarget;
    match t {
        ProxyTarget::Simple(u) => (u.as_str(), 1),
        ProxyTarget::Weighted(w) => (w.url.as_str(), w.weight),
    }
}

/// Convert a `ProxyRouteConfig`'s targets (flat or grouped) to JSON entries.
fn format_full_config_targets(
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
fn format_proxy_route_targets(
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
fn collect_site_proxy_entries(
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
fn resolve_runtime_targets(
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
fn build_flat_upstream_list(registry: &health::UpstreamRegistry) -> Vec<Value> {
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

/// `GET /rate-limits` — per-site/route rate-limiter counters.
///
/// Returns a nested object: `{ site: { route_key: { passed, rejected } } }`.
/// The key format mirrors the internal rate-limiter key (`"{site}\0{route}"`).
async fn rate_limits_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    use serde_json::Map;
    let mut result: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();

    for entry in state.rate_limiter.iter() {
        // Key format: "{site}\0{route}" or "*\0{route}" for wildcard.
        let key = entry.key();
        let bucket = entry.value();
        let (site, route) = key.split_once('\0').unwrap_or(("*", key));
        result
            .entry(site.to_owned())
            .or_insert_with(|| serde_json::Value::Object(Map::new()))
            .as_object_mut()
            .unwrap()
            .insert(
                route.to_owned(),
                json!({ "passed": bucket.passed, "rejected": bucket.rejected }),
            );
    }

    Json(json!(result))
}

/// Format a site's host+port as a human-readable label.
fn make_site_label(host: &Option<String>, port: Option<u16>) -> String {
    health::site_label(host, port)
}

async fn upstreams_add_handler(
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

async fn upstreams_remove_handler(
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

async fn upstreams_weight_handler(
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

// ── Cache purge ───────────────────────────────────────────────────────────────

/// Query parameters for `DELETE /cache/purge`.
#[derive(Deserialize)]
struct CachePurgeParams {
    /// Full URL to purge, e.g. `https://example.com/api/data?page=1`
    url: String,
}

/// `DELETE /cache/purge?url=<url>` — invalidate a specific cache entry.
///
/// Parses the URL into its components, builds the same `CacheKey` that the
/// proxy would use, and calls `MemCache::purge()` on the shared storage.
///
/// Returns `{"status":"ok","purged":true}` when an entry was found and removed,
/// `{"status":"ok","purged":false}` when no matching entry existed, or an error
/// JSON on bad input.
async fn cache_purge_handler(Query(params): Query<CachePurgeParams>) -> AdminResult<Json<Value>> {
    use pingora_cache::storage::{PurgeType, Storage};
    use pingora_cache::trace::Span;

    let raw = params.url.trim();

    // Use the url crate for robust parsing (handles IPv6, query-only URLs, etc.)
    let parsed =
        url::Url::parse(raw).map_err(|e| AdminError::BadRequest(format!("invalid url: {e}")))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(AdminError::BadRequest(
            "url must start with http:// or https://".to_owned(),
        ));
    }

    let authority = parsed
        .host_str()
        .ok_or_else(|| AdminError::BadRequest("url has no host".to_owned()))?;
    let authority = if let Some(port) = parsed.port() {
        format!("{authority}:{port}")
    } else {
        authority.to_owned()
    };

    let path = parsed.path();
    let query = parsed.query();

    let cache_key =
        crate::proxy::cache::build_cache_key(&authority, scheme, path, query, None, None);
    let compact = cache_key.to_compact();
    let storage = crate::proxy::cache::cache_storage();

    let span = Span::inactive().handle();
    let purged = storage
        .purge(&compact, PurgeType::Invalidation, &span)
        .await
        .map_err(|e| AdminError::ServerError(format!("cache purge failed: {e}")))?;

    Ok(Json(
        json!({ "status": "ok", "purged": purged, "url": raw }),
    ))
}

// ── Dynamic IP deny-list ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct IpDenyBody {
    /// CIDR to add or remove, e.g. `"1.2.3.0/24"` or `"10.0.0.5"`.
    cidr: String,
}

/// Validate that a string is a valid IP address or CIDR notation.
fn validate_cidr(s: &str) -> bool {
    if let Some((addr_str, prefix_str)) = s.split_once('/') {
        if let Ok(prefix) = prefix_str.parse::<u32>() {
            if let Ok(addr) = addr_str.parse::<std::net::IpAddr>() {
                let max_prefix = if addr.is_ipv4() { 32 } else { 128 };
                return prefix <= max_prefix;
            }
        }
        return false;
    }
    s.parse::<std::net::IpAddr>().is_ok()
}

/// `POST /ip-deny` — add a CIDR to the runtime deny-list.
async fn ip_deny_add_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IpDenyBody>,
) -> Json<Value> {
    let cidr = body.cidr.trim().to_owned();
    if !validate_cidr(&cidr) {
        return Json(
            json!({ "status": "error", "message": format!("invalid CIDR or IP address: {cidr:?}") }),
        );
    }
    {
        let mut list = state
            .dynamic_deny
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if !list.contains(&cidr) {
            list.push(cidr.clone());
        }
    }
    Json(json!({ "status": "ok", "action": "added", "cidr": cidr }))
}

/// `DELETE /ip-deny` — remove a CIDR from the runtime deny-list.
async fn ip_deny_remove_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IpDenyBody>,
) -> Json<Value> {
    let cidr = body.cidr.trim().to_owned();
    {
        let mut list = state
            .dynamic_deny
            .write()
            .unwrap_or_else(|e| e.into_inner());
        list.retain(|c| c != &cidr);
    }
    Json(json!({ "status": "ok", "action": "removed", "cidr": cidr }))
}

// ── Certificate rotation ──────────────────────────────────────────────────────

/// Request body for `POST /certs/reload`.
#[derive(Deserialize)]
struct CertReloadRequest {
    /// PEM-encoded certificate chain (leaf + intermediates).
    cert: String,
    /// PEM-encoded private key (PKCS#1, PKCS#8, or SEC1).
    key: String,
}

/// `POST /certs/reload` — validate new cert+key and write them to disk.
///
/// The new certificate is validated (cert/key must match and be parseable),
/// then written atomically to the file paths configured in `tls.cert` /
/// `tls.key`.  After writing, a `conduit reload` or process restart will
/// activate the new certificate for new TLS connections.
///
/// # Notes on zero-downtime rotation
///
/// Pingora 0.8's rustls backend does not expose a runtime cert-swap API.
/// True zero-downtime rotation (hot-swap without restarting the listener)
/// requires a process upgrade: start the new process with `--upgrade` so it
/// inherits the listening socket FDs from the old process, then send SIGQUIT
/// to the old process.  On systems managed by systemd this is done via
/// `systemctl reload conduit`.
///
/// # Errors
///
/// Returns `400 Bad Request` when:
/// - No site has `tls.cert` / `tls.key` configured.
/// - The provided cert/key PEM is invalid or the pair does not match.
///
/// Returns `500 Internal Server Error` when the atomic file write fails.
async fn certs_reload_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CertReloadRequest>,
) -> AdminResult<Json<Value>> {
    use crate::server::tls::validate_cert_key_pem;

    // Find the first site that has manual TLS cert/key configured.
    let config = state.config.load();
    let (cert_path, key_path) = config
        .sites
        .iter()
        .find_map(|site| {
            let tls = site.tls.as_ref()?;
            let cert = tls.cert.as_deref()?;
            let key = tls.key.as_deref()?;
            Some((cert.to_owned(), key.to_owned()))
        })
        .ok_or_else(|| {
            AdminError::BadRequest(
                "no site has tls.cert/tls.key configured — nothing to rotate".to_owned(),
            )
        })?;

    // Validate cert+key before touching any files.
    validate_cert_key_pem(&body.cert, &body.key)
        .map_err(|e| AdminError::BadRequest(format!("invalid cert/key: {e}")))?;

    // Write cert atomically: write to a temp file next to the destination,
    // then rename so readers never see a partial write.
    atomic_write(&cert_path, body.cert.as_bytes()).map_err(|e| {
        AdminError::ServerError(format!("failed to write cert to {cert_path}: {e}"))
    })?;
    atomic_write(&key_path, body.key.as_bytes())
        .map_err(|e| AdminError::ServerError(format!("failed to write key to {key_path}: {e}")))?;

    tracing::info!(cert = %cert_path, key = %key_path, "TLS certificate written via /certs/reload");

    Ok(Json(json!({
        "status": "ok",
        "cert_path": cert_path,
        "key_path": key_path,
        "note": "certificate written to disk — restart or POST /reload (if not a cold-field change) to activate"
    })))
}

/// Write `data` to `path` atomically by writing to a sibling `.tmp` file
/// and then renaming it into place.
fn atomic_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write as _;
    let tmp = format!("{path}.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the `logging.file` path from a `LoggingConfig`, if any.
fn log_file_path(cfg: &Option<LoggingConfig>) -> Option<&str> {
    match cfg {
        Some(LoggingConfig::Options(opts)) => opts.file.as_deref(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::from_str as parse_config;

    // ── validate_cidr ────────────────────────────────────────────────────────

    #[test]
    fn valid_ipv4_cidr() {
        assert!(validate_cidr("192.168.1.0/24"));
        assert!(validate_cidr("10.0.0.0/8"));
        assert!(validate_cidr("0.0.0.0/0"));
        assert!(validate_cidr("255.255.255.255/32"));
    }

    #[test]
    fn valid_ipv6_cidr() {
        assert!(validate_cidr("2001:db8::/32"));
        assert!(validate_cidr("::/0"));
        assert!(validate_cidr("::1/128"));
    }

    #[test]
    fn valid_single_ips() {
        assert!(validate_cidr("192.168.1.1"));
        assert!(validate_cidr("::1"));
        assert!(validate_cidr("10.0.0.1"));
    }

    #[test]
    fn invalid_cidrs() {
        assert!(!validate_cidr("not-a-cidr"));
        assert!(!validate_cidr("999.999.999.999"));
        assert!(!validate_cidr("192.168.1.0/99")); // prefix > 32 for IPv4
        assert!(!validate_cidr("192.168.1.0/abc")); // non-numeric prefix
        assert!(!validate_cidr(""));
    }

    // ── detect_cold_changes ──────────────────────────────────────────────────

    fn cfg(json: &str) -> crate::config::schema::AppConfig {
        parse_config(json).expect("parse")
    }

    #[test]
    fn no_changes_returns_empty() {
        let base = cfg(r#"{"sites":[{"port":8080}]}"#);
        let same = cfg(r#"{"sites":[{"port":8080}]}"#);
        assert!(detect_cold_changes(&base, &same).is_empty());
    }

    #[test]
    fn port_change_is_cold() {
        let old = cfg(r#"{"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"sites":[{"port":9090}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("port")),
            "port must be cold: {cold:?}"
        );
    }

    #[test]
    fn workers_change_is_cold() {
        let old = cfg(r#"{"global":{"workers":2},"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"global":{"workers":4},"sites":[{"port":8080}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("workers")),
            "workers must be cold: {cold:?}"
        );
    }

    #[test]
    fn rate_limit_change_is_not_cold() {
        let old = cfg(r#"{"sites":[{"port":8080,"rateLimit":{"windowSecs":60,"limit":100}}]}"#);
        let new = cfg(r#"{"sites":[{"port":8080,"rateLimit":{"windowSecs":60,"limit":200}}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.is_empty(),
            "rateLimit change should be hot-reloadable: {cold:?}"
        );
    }

    // ── AdminError ───────────────────────────────────────────────────────────

    #[test]
    fn bad_request_produces_error_status_json() {
        use axum::response::IntoResponse;
        let err = AdminError::BadRequest("test error".to_owned());
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn server_error_produces_500_status() {
        use axum::response::IntoResponse;
        let err = AdminError::ServerError("internal".to_owned());
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn cold_fields_changed_produces_400() {
        use axum::response::IntoResponse;
        let err = AdminError::ColdFieldsChanged {
            message: "restart required: sites[0].port".to_owned(),
            fields: vec!["sites[0].port".to_owned()],
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    // ── make_site_label ──────────────────────────────────────────────────────

    #[test]
    fn site_label_with_host_and_port() {
        let label = make_site_label(&Some("api.example.com".to_owned()), Some(443));
        assert!(label.contains("api.example.com"));
        assert!(label.contains("443"));
    }

    #[test]
    fn site_label_without_host() {
        let label = make_site_label(&None, Some(8080));
        assert!(label.contains("8080"));
    }

    #[test]
    fn site_label_both_none() {
        let label = make_site_label(&None, None);
        // Must not panic; some placeholder is returned.
        assert!(!label.is_empty());
    }

    // ── detect_cold_changes — more scenarios ──────────────────────────────────

    #[test]
    fn tls_cert_change_is_cold() {
        let old = cfg(r#"{"sites":[{"port":443,"tls":{"cert":"old.pem","key":"server.key"}}]}"#);
        let new = cfg(r#"{"sites":[{"port":443,"tls":{"cert":"new.pem","key":"server.key"}}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("tls.cert")),
            "cert change must be cold: {cold:?}"
        );
    }

    #[test]
    fn tls_key_change_is_cold() {
        let old = cfg(r#"{"sites":[{"port":443,"tls":{"cert":"server.pem","key":"old.key"}}]}"#);
        let new = cfg(r#"{"sites":[{"port":443,"tls":{"cert":"server.pem","key":"new.key"}}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("tls.key")),
            "key change must be cold: {cold:?}"
        );
    }

    #[test]
    fn admin_bind_change_is_cold() {
        let old = cfg(r#"{"global":{"admin":{"bind":"127.0.0.1:2019"}},"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"global":{"admin":{"bind":"127.0.0.1:2020"}},"sites":[{"port":8080}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("admin.bind")),
            "admin bind change must be cold: {cold:?}"
        );
    }

    #[test]
    fn backlog_change_is_cold() {
        let old = cfg(r#"{"global":{"backlog":128},"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"global":{"backlog":256},"sites":[{"port":8080}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("backlog")),
            "backlog change must be cold: {cold:?}"
        );
    }

    #[test]
    fn proxy_change_is_not_cold() {
        let old = cfg(r#"{"sites":[{"port":8080,"proxy":"http://a:4000"}]}"#);
        let new = cfg(r#"{"sites":[{"port":8080,"proxy":"http://b:4000"}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().all(|f| !f.contains("proxy")),
            "proxy change should be hot-reloadable: {cold:?}"
        );
    }

    #[test]
    fn adding_a_new_site_detects_port_change() {
        // Old: 1 site; New: 2 sites — the new site's port would be detected.
        let old = cfg(r#"{"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"sites":[{"port":8080},{"port":9090}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("sites[1]")),
            "extra site should produce cold change: {cold:?}"
        );
    }

    // ── validate_cidr — additional edge cases ─────────────────────────────────

    #[test]
    fn validate_cidr_ipv4_prefix_32_valid() {
        assert!(validate_cidr("192.168.1.1/32"), "/32 is valid for IPv4");
    }

    #[test]
    fn validate_cidr_ipv4_prefix_33_invalid() {
        assert!(!validate_cidr("10.0.0.0/33"), "/33 is invalid for IPv4");
    }

    #[test]
    fn validate_cidr_ipv6_prefix_128_valid() {
        assert!(validate_cidr("::1/128"), "/128 is valid for IPv6");
    }

    #[test]
    fn validate_cidr_ipv6_prefix_129_invalid() {
        assert!(!validate_cidr("::1/129"), "/129 is invalid for IPv6");
    }

    #[test]
    fn validate_cidr_whitespace_invalid() {
        assert!(
            !validate_cidr(" 192.168.1.0/24"),
            "leading space is invalid"
        );
        assert!(
            !validate_cidr("192.168.1.0/24 "),
            "trailing space is invalid"
        );
    }

    // ── subtle_eq ─────────────────────────────────────────────────────────────

    #[test]
    fn subtle_eq_equal_slices() {
        assert!(subtle_eq(b"secret", b"secret"));
    }

    #[test]
    fn subtle_eq_different_slices() {
        assert!(!subtle_eq(b"secret", b"wrong!"));
    }

    #[test]
    fn subtle_eq_different_lengths() {
        assert!(!subtle_eq(b"short", b"longer-value"));
    }

    #[test]
    fn subtle_eq_empty_slices() {
        assert!(subtle_eq(b"", b""));
    }

    // ── strategy_label ────────────────────────────────────────────────────────

    #[test]
    fn strategy_label_all_variants() {
        use crate::config::schema::LoadBalanceStrategy as S;
        assert_eq!(strategy_label(&S::RoundRobin), "round-robin");
        assert_eq!(
            strategy_label(&S::WeightedRoundRobin),
            "weighted-round-robin"
        );
        assert_eq!(strategy_label(&S::Random), "random");
        assert_eq!(strategy_label(&S::LeastConn), "least-conn");
        assert_eq!(strategy_label(&S::LeastResponseTime), "least-response-time");
        assert_eq!(strategy_label(&S::IpHash), "ip-hash");
        assert_eq!(strategy_label(&S::ConsistentHash), "consistent-hash");
        assert_eq!(strategy_label(&S::P2c), "p2c");
    }

    // ── proxy_target_url_weight ───────────────────────────────────────────────

    #[test]
    fn proxy_target_simple_has_weight_one() {
        use crate::config::schema::ProxyTarget;
        let t = ProxyTarget::Simple("http://backend:4000".to_owned());
        let (url, weight) = proxy_target_url_weight(&t);
        assert_eq!(url, "http://backend:4000");
        assert_eq!(weight, 1);
    }

    #[test]
    fn proxy_target_weighted_uses_configured_weight() {
        use crate::config::schema::{ProxyTarget, WeightedTarget};
        let t = ProxyTarget::Weighted(WeightedTarget {
            url: "http://backend:4000".to_owned(),
            weight: 5,
        });
        let (url, weight) = proxy_target_url_weight(&t);
        assert_eq!(url, "http://backend:4000");
        assert_eq!(weight, 5);
    }

    // ── log_file_path ─────────────────────────────────────────────────────────

    #[test]
    fn log_file_path_none_when_no_config() {
        assert!(log_file_path(&None).is_none());
    }

    #[test]
    fn log_file_path_none_when_enabled_true() {
        use crate::config::schema::LoggingConfig;
        assert!(log_file_path(&Some(LoggingConfig::Enabled(true))).is_none());
    }

    #[test]
    fn log_file_path_none_when_enabled_false() {
        use crate::config::schema::LoggingConfig;
        assert!(log_file_path(&Some(LoggingConfig::Enabled(false))).is_none());
    }

    #[test]
    fn log_file_path_returns_file_when_options_set() {
        use crate::config::schema::{LoggingConfig, LoggingOptions};
        let opts = LoggingOptions {
            file: Some("/var/log/conduit/access.log".to_owned()),
            ..Default::default()
        };
        let cfg = Some(LoggingConfig::Options(opts));
        let result = log_file_path(&cfg);
        assert_eq!(result, Some("/var/log/conduit/access.log"));
    }

    #[test]
    fn log_file_path_none_when_options_no_file() {
        use crate::config::schema::{LoggingConfig, LoggingOptions};
        let opts = LoggingOptions {
            file: None,
            ..Default::default()
        };
        let cfg = Some(LoggingConfig::Options(opts));
        let result = log_file_path(&cfg);
        assert!(result.is_none());
    }

    // ── atomic_write ──────────────────────────────────────────────────────────

    #[test]
    fn atomic_write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.txt");
        atomic_write(path.to_str().unwrap(), b"hello atomic").expect("atomic_write must succeed");
        let content = std::fs::read_to_string(&path).expect("file must exist");
        assert_eq!(content, "hello atomic");
    }

    #[test]
    fn atomic_write_no_tmp_file_left_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.txt");
        atomic_write(path.to_str().unwrap(), b"data").expect("must succeed");
        let tmp = dir.path().join("output.txt.tmp");
        assert!(!tmp.exists(), ".tmp file must be removed after rename");
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        atomic_write(path.to_str().unwrap(), b"v1").unwrap();
        atomic_write(path.to_str().unwrap(), b"v2").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "v2", "second write must overwrite first");
    }

    // ── url_health_entry ──────────────────────────────────────────────────────

    #[test]
    fn url_health_entry_unknown_url_returns_null_health() {
        let reg = crate::proxy::health::UpstreamRegistry::new();
        let entry = url_health_entry(&reg, "http://unknown:4000", 1, None);
        assert_eq!(entry["url"], "http://unknown:4000");
        assert_eq!(entry["weight"], 1);
        assert!(
            entry["healthy"].is_null(),
            "unknown URL must have null health"
        );
    }

    #[test]
    fn url_health_entry_known_url_includes_health_data() {
        let reg = crate::proxy::health::UpstreamRegistry::new();
        {
            let mut e = reg
                .statuses
                .entry("http://backend:4000".to_owned())
                .or_default();
            e.healthy = true;
            e.latency_ms = Some(15);
            e.consecutive_failures = 0;
            e.consecutive_successes = 5;
        }
        let entry = url_health_entry(&reg, "http://backend:4000", 2, None);
        assert_eq!(entry["healthy"], true);
        assert_eq!(entry["latency_ms"], 15);
        assert_eq!(entry["consecutive_failures"], 0);
        assert_eq!(entry["consecutive_successes"], 5);
        assert_eq!(entry["weight"], 2);
    }

    #[test]
    fn url_health_entry_with_group_includes_group_field() {
        let reg = crate::proxy::health::UpstreamRegistry::new();
        let entry = url_health_entry(&reg, "http://a:4000", 1, Some("primary"));
        assert_eq!(entry["group"], "primary");
    }

    // ── format_proxy_route_targets ────────────────────────────────────────────

    #[test]
    fn format_proxy_route_targets_url_variant() {
        use crate::config::schema::ProxyRouteTarget;
        use crate::proxy::health::UpstreamRegistry;
        let reg = UpstreamRegistry::new();
        let rt = ProxyRouteTarget::Url("http://a:4000".to_owned());
        let (strategy, targets) = format_proxy_route_targets(&rt, &reg);
        assert_eq!(strategy, "round-robin");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0]["url"], "http://a:4000");
    }

    #[test]
    fn format_proxy_route_targets_round_robin_variant() {
        use crate::config::schema::ProxyRouteTarget;
        use crate::proxy::health::UpstreamRegistry;
        let reg = UpstreamRegistry::new();
        let rt = ProxyRouteTarget::RoundRobin(vec![
            "http://a:4000".to_owned(),
            "http://b:4000".to_owned(),
        ]);
        let (strategy, targets) = format_proxy_route_targets(&rt, &reg);
        assert_eq!(strategy, "round-robin");
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn format_full_config_targets_no_groups() {
        use crate::config::schema::{ProxyRouteConfig, ProxyTarget};
        use crate::proxy::health::UpstreamRegistry;
        let reg = UpstreamRegistry::new();
        let cfg = ProxyRouteConfig {
            targets: vec![
                ProxyTarget::Simple("http://a:4000".to_owned()),
                ProxyTarget::Simple("http://b:4000".to_owned()),
            ],
            ..Default::default()
        };
        let result = format_full_config_targets(&cfg, &reg);
        assert_eq!(result.len(), 2);
        let urls: Vec<&str> = result.iter().filter_map(|e| e["url"].as_str()).collect();
        assert!(urls.contains(&"http://a:4000"));
        assert!(urls.contains(&"http://b:4000"));
    }

    // ── collect_site_proxy_entries ────────────────────────────────────────────

    #[test]
    fn collect_site_proxy_entries_empty_site() {
        use crate::config::schema::SiteConfig;
        let site = SiteConfig::default();
        let entries = collect_site_proxy_entries(&site);
        assert!(entries.is_empty(), "empty site must yield empty entries");
    }

    #[test]
    fn collect_site_proxy_entries_from_routes_map() {
        use crate::config::schema::{ProxyConfig, ProxyRouteTarget, SiteConfig};
        use indexmap::IndexMap;
        let mut routes = IndexMap::new();
        routes.insert(
            "/api".to_string(),
            ProxyRouteTarget::Url("http://backend:4000".to_string()),
        );
        let site = SiteConfig {
            proxy: Some(ProxyConfig::Routes(routes)),
            ..Default::default()
        };
        let entries = collect_site_proxy_entries(&site);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "/api");
    }

    // ── build_flat_upstream_list ──────────────────────────────────────────────

    #[test]
    fn build_flat_upstream_list_empty_registry() {
        let reg = crate::proxy::health::UpstreamRegistry::new();
        let list = build_flat_upstream_list(&reg);
        assert!(list.is_empty(), "empty registry must return empty list");
    }

    #[test]
    fn build_flat_upstream_list_includes_known_urls() {
        use crate::proxy::health::UpstreamRegistry;
        let reg = UpstreamRegistry::new();
        reg.statuses.entry("http://a:4000".to_owned()).or_default();
        reg.statuses.entry("http://b:4000".to_owned()).or_default();
        let list = build_flat_upstream_list(&reg);
        assert_eq!(list.len(), 2);
        let urls: Vec<&str> = list.iter().filter_map(|e| e["url"].as_str()).collect();
        assert!(urls.contains(&"http://a:4000"));
        assert!(urls.contains(&"http://b:4000"));
    }

    #[test]
    fn build_flat_upstream_list_sorted_by_url() {
        use crate::proxy::health::UpstreamRegistry;
        let reg = UpstreamRegistry::new();
        reg.statuses.entry("http://z:4000".to_owned()).or_default();
        reg.statuses.entry("http://a:4000".to_owned()).or_default();
        let list = build_flat_upstream_list(&reg);
        assert_eq!(list[0]["url"], "http://a:4000");
        assert_eq!(list[1]["url"], "http://z:4000");
    }

    // ── resolve_runtime_targets ───────────────────────────────────────────────

    #[test]
    fn resolve_runtime_targets_no_overrides_returns_config() {
        use crate::proxy::health::UpstreamRegistry;
        let reg = UpstreamRegistry::new();
        let config_targets = vec![serde_json::json!({"url": "http://config:4000"})];
        let result = resolve_runtime_targets(&reg, "*", "/api", config_targets.clone());
        assert_eq!(
            result, config_targets,
            "no overrides → config targets returned"
        );
    }

    #[test]
    fn resolve_runtime_targets_with_overrides_returns_overrides() {
        use crate::proxy::health::UpstreamRegistry;
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/api", "http://override:4000", 2);
        let config_targets = vec![serde_json::json!({"url": "http://config:4000"})];
        let result = resolve_runtime_targets(&reg, "*", "/api", config_targets);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["url"], "http://override:4000");
        assert_eq!(result[0]["runtime"], true);
    }
}
