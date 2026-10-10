use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use conduit_admin::api::{serve, AdminError, AdminResult};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use serde_json::{json, Value};

use crate::config;
use crate::config::schema::LoggingConfig;
use crate::config::validate;
#[cfg(feature = "proxy")]
use conduit_runtime::proxy::health;
use conduit_runtime::proxy::service::AppState;

// The admin crate's `cache` feature turns the purge endpoint on; a build where it drifts from this crate's own
// feature would answer 501 in a cache build (or `purged: false` against a store nothing writes to). Compile-time,
// like the root's own assert against this crate's `cache` feature (`src/config/validate/mod.rs`).
const _: () = assert!(
    conduit_admin::features::CACHE == cfg!(feature = "cache"),
    "`lopatnov-conduit-admin`'s `cache` feature and this crate's `cache` feature must be enabled together"
);

/// Resolve `(healthCheck config, target URLs)` pairs for every `proxy: {}`
/// route (the legacy map form) that has `healthCheck` configured, across
/// every site.
///
/// Lives in the root crate, not `crates/conduit-upstream` — it needs
/// `AppConfig`/`ProxyConfig`/`ProxyRouteTarget`, root-only types not yet
/// extracted (a later migration phase — #143/#144). `conduit_upstream`'s
/// `spawn_health_checks`/`spawn_connection_warmup` take this narrower,
/// already-resolved slice instead of `&AppConfig` directly — see issue #142
/// and `crates/conduit-upstream/src/health.rs`'s own doc comment. The same
/// pattern `conduit-hotreload`'s `build_watch_config` call site
/// (`config.sites.iter().map(...)`, a few lines above `start()`'s hot-reload
/// block) already uses for its own analogous problem — this one is just a
/// deeper extraction (sites → routes → route targets) instead of a flat
/// per-site map, so it earns its own named helper rather than being inlined
/// at each of the three call sites.
///
/// `proxy`-only (issue #144, PR 4a): its only callers are the two upstream
/// probe spawners below, which don't exist without the feature.
#[cfg(feature = "proxy")]
fn health_check_routes(
    config: &crate::config::schema::AppConfig,
) -> Vec<(&crate::config::schema::UpstreamHealthCheck, Vec<String>)> {
    use crate::config::schema::{ProxyConfig, ProxyRouteTarget};

    config
        .sites
        .iter()
        .filter_map(|site| match &site.proxy {
            Some(ProxyConfig::Routes(routes)) => Some(routes.values()),
            _ => None,
        })
        .flatten()
        .filter_map(|route_target| {
            let ProxyRouteTarget::Full(cfg) = route_target else {
                return None;
            };
            let hc = cfg.health_check.as_ref()?;
            Some((
                hc,
                conduit_runtime::proxy::upstream::target_urls(route_target),
            ))
        })
        .collect()
}

/// Spawn the active upstream health-check tasks for every route that has
/// `healthCheck` configured, and -- only when `warmup` is set (server start,
/// not a reload) -- warm the connection pools of routes with
/// `prewarmConnections`. The `proxy` variant.
///
/// This is the gate boundary for the admin surface (issue #144, PR 4a): it is
/// the one place the process makes timed outbound HTTP probes to third-party
/// hosts purely on behalf of proxying, and the only root call site of
/// `conduit_upstream::health::spawn_connection_warmup`, which exists only
/// with `conduit-upstream/proxy`. The registry readers (`/upstreams*`,
/// `/__health__?full=1`) stay compiled and simply see no probe results.
#[cfg(feature = "proxy")]
fn spawn_upstream_probes(
    state: &AppState,
    config: &crate::config::schema::AppConfig,
    warmup: bool,
) {
    let routes = health_check_routes(config);
    health::spawn_health_checks(
        state.upstream_health.clone(),
        routes.iter().map(|(hc, urls)| (*hc, urls.as_slice())),
    );
    if warmup {
        // Warm up connection pools for routes with prewarmConnections set.
        health::spawn_connection_warmup(routes.iter().map(|(hc, urls)| (*hc, urls.as_slice())));
    }
}

/// No-`proxy` variant of [`spawn_upstream_probes`]: nothing is proxied, so
/// there is nothing to probe or warm.
#[cfg(not(feature = "proxy"))]
fn spawn_upstream_probes(
    _state: &AppState,
    _config: &crate::config::schema::AppConfig,
    _warmup: bool,
) {
}

pub struct AdminApiService {
    pub state: Arc<AppState>,
    /// Address to bind the Admin HTTP server on, e.g. `"127.0.0.1:2019"`.
    ///
    /// `None` when `global.admin` is absent from the config — in that case
    /// the internal background tasks (health checks, rate-limiter cleanup,
    /// hot-reload watcher) still run, but no HTTP endpoint is exposed.
    pub bind: Option<String>,
    /// ACME certificates to keep fresh while the process runs, planned at
    /// startup from the listeners that were bound (#491).
    #[cfg(feature = "acme")]
    pub acme_renewals: Vec<crate::server::acme::RenewalJob>,
}

#[async_trait]
impl BackgroundService for AdminApiService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        // Spawn a background task that evicts stale rate-limiter entries every 60 s.
        //
        // Takes its own clone of `shutdown` and exits the loop once it fires — without this the task
        // would keep running (and the process couldn't cleanly finish shutting down its background
        // services) until process exit, regardless of what `BackgroundService::start()`'s caller expects
        // (found during review of #147, filed as a review note on that issue before this fix).
        {
            let limiter = self.state.rate_limiter.clone();
            #[cfg(feature = "redis")]
            let redis_rl = self.state.redis_rate_limiter.clone();
            let mut shutdown = shutdown.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                loop {
                    tokio::select! {
                        _ = interval.tick() => {}
                        _ = shutdown.changed() => return,
                    }
                    conduit_runtime::filter::rate_limit::cleanup(&limiter);
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
            use conduit_runtime::proxy::service::ConduitMetrics;
            let gauge = ConduitMetrics::global().eventloop_lag_ms.clone();
            // Own clone of `shutdown`, same reasoning as the rate-limit cleanup task above.
            let mut shutdown = shutdown.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
                // Under heavy load the tick may be missed; Skip prevents a burst
                // of catch-up probes that would skew the lag metric.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = ticker.tick() => {}
                        _ = shutdown.changed() => return,
                    }
                    // Measure how long between yielding and being resumed.
                    let before = std::time::Instant::now();
                    tokio::task::yield_now().await;
                    let lag_ms = before.elapsed().as_secs_f64() * 1000.0;
                    gauge.set(lag_ms);
                }
            });
        }

        // Spawn upstream health check tasks for every route that has healthCheck
        // configured, and warm connection pools for routes with prewarmConnections.
        {
            let config = self.state.config.load();
            spawn_upstream_probes(&self.state, &config, true);
        }

        // Connect every configured Redis-backed proxy cache up front (issue
        // #330) -- the request path only ever looks up an already-connected
        // store, it never connects from inside Pingora's own runtime.
        #[cfg(all(feature = "cache", feature = "redis"))]
        {
            let config = self.state.config.load_full();
            conduit_runtime::proxy::cache_redis::connect_all(&config).await;
        }

        // Spawn the browser hot-reload file watcher if any site has hotReload enabled.
        #[cfg(feature = "hotreload")]
        {
            let config = self.state.config.load();
            let sites = config
                .sites
                .iter()
                .map(|s| (s.hot_reload.as_ref(), s.static_files.as_ref()));
            if let Some((dirs, extensions)) =
                conduit_runtime::handler::hot_reload::build_watch_config(sites)
            {
                let reload_tx = self.state.hot_reload_tx.clone();
                tokio::spawn(conduit_runtime::handler::hot_reload::run_file_watcher(
                    dirs, extensions, reload_tx,
                ));
            }
        }

        // ACME renewal: one sequential loop, checked every 12 h, stops on
        // shutdown. The renewed files take effect at the next restart; hot-swap
        // is not implemented yet.
        #[cfg(feature = "acme")]
        if !self.acme_renewals.is_empty() {
            tokio::spawn(crate::server::acme::run_renewal_loop(
                self.acme_renewals.clone(),
                self.state.acme_challenges.clone(),
                shutdown.clone(),
            ));
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

        serve(self.state.clone(), &bind_addr, root_routes(), shutdown).await;
    }
}

/// The Admin API routes that live in the root crate rather than with the built-in ones:
/// `POST /reload` needs the root's config validation.
fn root_routes() -> Router<Arc<AppState>> {
    Router::new().route("/reload", post(reload_handler))
}

async fn reload_handler(State(state): State<Arc<AppState>>) -> AdminResult<Json<Value>> {
    // Re-parse the config file.
    let new_config = config::load_config(&state.config_path)
        .map_err(|e| AdminError::ServerError(format!("failed to parse config: {e}")))?;

    // Validate the new config before applying it. Advisory findings (e.g. a
    // still-valid cert nearing expiry, issue #191) are logged but must not
    // block a reload — only a real config error does.
    let errors = validate::validate(&new_config);
    let (warnings, hard_errors) = validate::partition_by_severity(errors);
    for w in &warnings {
        tracing::warn!("config: {}: {}", w.path, w.message);
    }
    if !hard_errors.is_empty() {
        return Err(AdminError::ServerError(format!(
            "config validation failed: {}",
            hard_errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    // Collect config warnings once — used for both logging and the response body.
    let fw: Vec<String> = validate::feature_warnings(&new_config);
    for w in &fw {
        tracing::warn!("{w}");
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
    spawn_upstream_probes(&state, &new_config, false);

    // Connect any Redis-backed proxy cache URL introduced by this reload
    // (issue #330) -- before the config swap below, so there's no window
    // where the new config is live but its cache store isn't registered
    // yet. Idempotent: URLs already connected are a cheap no-op.
    #[cfg(all(feature = "cache", feature = "redis"))]
    conduit_runtime::proxy::cache_redis::connect_all(&new_config).await;

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
}
