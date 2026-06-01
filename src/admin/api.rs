use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::extract::Query;
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
        .route("/ip-deny", post(ip_deny_add_handler))
        .route("/ip-deny", delete(ip_deny_remove_handler));

    // Wrap with bearer-token auth middleware if a token is configured.
    let token: Option<String> = state
        .config
        .load()
        .global
        .as_ref()
        .and_then(|g| g.admin.as_ref())
        .and_then(|a| a.token.clone());

    if let Some(required_token) = token {
        // Use a closure that captures the token string so we avoid Axum state
        // type conflicts (the router state is Arc<AppState>, not Arc<String>).
        protected
            .layer(axum::middleware::from_fn(
                move |request: Request<Body>, next: Next| {
                    let token = required_token.clone();
                    async move {
                        let auth = request
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        let provided = auth.strip_prefix("Bearer ").map(str::trim).unwrap_or("");
                        if provided == token.as_str() {
                            Ok(next.run(request).await)
                        } else {
                            Err(StatusCode::UNAUTHORIZED)
                        }
                    }
                },
            ))
            .with_state(state)
    } else {
        protected.with_state(state)
    }
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

    // Validate the new config before applying it.
    let errors = validate::validate(&new_config);
    if !errors.is_empty() {
        return Json(json!({
            "status": "error",
            "message": "config validation failed",
            "errors": errors,
        }));
    }

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
    flat
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
async fn cache_purge_handler(
    Query(params): Query<CachePurgeParams>,
) -> (StatusCode, Json<Value>) {
    use pingora_cache::storage::{PurgeType, Storage};
    use pingora_cache::trace::Span;

    let raw = params.url.trim();

    // Parse scheme, host, path, query from the URL.
    let (scheme, rest) = if let Some(r) = raw.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = raw.strip_prefix("http://") {
        ("http", r)
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "error", "message": "url must start with http:// or https://" })),
        );
    };

    let (authority, path_query) = rest
        .find('/')
        .map(|i| (&rest[..i], &rest[i..]))
        .unwrap_or((rest, "/"));

    let (path, query) = path_query
        .find('?')
        .map(|i| (&path_query[..i], Some(&path_query[i + 1..])))
        .unwrap_or((path_query, None));

    let cache_key = crate::proxy::cache::build_cache_key(
        authority,
        scheme,
        path,
        query,
        None,
        None,
    );
    let compact = cache_key.to_compact();
    let storage = crate::proxy::cache::cache_storage();

    let span = Span::inactive().handle();
    let purged = storage
        .purge(&compact, PurgeType::Invalidation, &span)
        .await
        .unwrap_or(false);

    (
        StatusCode::OK,
        Json(json!({ "status": "ok", "purged": purged, "url": raw })),
    )
}

// ── Dynamic IP deny-list ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct IpDenyBody {
    /// CIDR to add or remove, e.g. `"1.2.3.0/24"` or `"10.0.0.5"`.
    cidr: String,
}

/// `POST /ip-deny` — add a CIDR to the runtime deny-list.
async fn ip_deny_add_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IpDenyBody>,
) -> Json<Value> {
    let cidr = body.cidr.trim().to_owned();
    {
        let mut list = state.dynamic_deny.write().unwrap_or_else(|e| e.into_inner());
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
        let mut list = state.dynamic_deny.write().unwrap_or_else(|e| e.into_inner());
        list.retain(|c| c != &cidr);
    }
    Json(json!({ "status": "ok", "action": "removed", "cidr": cidr }))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the `logging.file` path from a `LoggingConfig`, if any.
fn log_file_path(cfg: &Option<LoggingConfig>) -> Option<&str> {
    match cfg {
        Some(LoggingConfig::Options(opts)) => opts.file.as_deref(),
        _ => None,
    }
}
