use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use conduit_admin::api::{serve, AdminError, AdminResult};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use serde_json::{json, Value};

use super::cold_keys::{
    acme_by_host, https_client_auth_by_port, proxy_protocol_by_port, tcp_by_port, tcp_tls_by_port,
};
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

/// Resolve `(healthCheck config, target URLs)` pairs for every proxy route
/// that has `healthCheck` configured, across every site: the `proxy: {}` map
/// (legacy form) **and** the `routes[]` array (issue #376). A route whose
/// targets live in `groups` is probed on the groups' URLs.
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

    let route_targets = config.sites.iter().flat_map(|site| {
        let map_routes = match &site.proxy {
            Some(ProxyConfig::Routes(routes)) => Some(routes.values()),
            _ => None,
        };
        let array_routes = site
            .routes
            .iter()
            .flatten()
            .filter_map(|r| r.proxy.as_ref());
        map_routes.into_iter().flatten().chain(array_routes)
    });
    route_targets
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
    let mut fw: Vec<String> = validate::feature_warnings(&new_config);
    fw.extend(crate::config::rate_limit_scan::redis_rate_limit_change_warning(&new_config));
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
    // An edited script or plugin file at an unchanged path takes effect with this reload (issue #566).
    conduit_middleware::clear_caches();
    state.upstream_health.clear_overrides();
    state.rate_limiter.clear();

    let mut resp = json!({ "status": "ok", "message": "config reloaded" });
    if !fw.is_empty() {
        resp["warnings"] = json!(fw);
    }
    Ok(Json(resp))
}

/// `port -> (tls.cert, tls.key)` for every HTTP listener the config binds. The first site on a port
/// with a complete manual cert/key pair decides its TLS material, even when a plain or ACME site
/// comes earlier (as `classify_site_port` does); a port without one is `(None, None)`. TCP-proxy
/// sites manage their own listeners.
fn bound_listeners(
    cfg: &crate::config::schema::AppConfig,
) -> std::collections::BTreeMap<u16, (Option<String>, Option<String>)> {
    let mut out: std::collections::BTreeMap<u16, (Option<String>, Option<String>)> =
        std::collections::BTreeMap::new();
    for site in cfg.sites.iter().filter(|s| s.tcp.is_none()) {
        let port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        let entry = out.entry(port).or_default();
        if entry.0.is_none() {
            if let Some(t) = site.tls.as_ref() {
                if let (Some(cert), Some(key)) = (&t.cert, &t.key) {
                    *entry = (Some(cert.clone()), Some(key.clone()));
                }
            }
        }
    }
    out
}

/// Return the list of field paths that changed between `old` and `new` and
/// require a server restart (cold fields).
///
/// Cold fields: `global.workers`, `global.shutdownTimeoutSecs`, `global.admin.bind`, `global.redis.url`,
/// the set of bound listener ports, each port's `tls.cert`/`tls.key`/`tls.clientAuth`, every field of a site's `tls.acme`, the `tcp`
/// block and the `tls` block of every TCP site (the TCP services are registered once, at start).
pub(crate) fn detect_cold_changes(
    old: &crate::config::schema::AppConfig,
    new: &crate::config::schema::AppConfig,
) -> Vec<String> {
    detect_cold_changes_in(
        old,
        new,
        conduit_config_core::scheme::redis_url_env().as_deref(),
    )
}

/// [`detect_cold_changes`] with the value of `CONDUIT_REDIS_URL` given: read once, so both configs see the same one.
pub(crate) fn detect_cold_changes_in(
    old: &crate::config::schema::AppConfig,
    new: &crate::config::schema::AppConfig,
    env_url: Option<&str>,
) -> Vec<String> {
    let mut cold = Vec::new();

    // global.workers / global.shutdownTimeoutSecs / global.admin.bind
    let old_g = old.global.as_ref();
    let new_g = new.global.as_ref();
    if old_g.and_then(|g| g.workers) != new_g.and_then(|g| g.workers) {
        cold.push("global.workers".to_string());
    }
    if old_g.and_then(|g| g.shutdown_timeout_secs) != new_g.and_then(|g| g.shutdown_timeout_secs) {
        cold.push("global.shutdownTimeoutSecs".to_string());
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
    // The one Redis connection of the process is made at start-up from the operator's address (issue #760): a
    // changed `global.redis.url` would leave every store named `redis` on the old address. Compared as resolved
    // (file value, else `CONDUIT_REDIS_URL`), so a change the environment overrides is not reported.
    if crate::config::rate_limit_scan::operator_redis_address_in(old, env_url)
        != crate::config::rate_limit_scan::operator_redis_address_in(new, env_url)
    {
        cold.push("global.redis.url".to_string());
    }

    // Listeners: the set of bound ports and each port's TLS cert/key. Sites are matched by listener,
    // not by position, so adding, removing or reordering sites on already-bound ports is hot (a
    // Kubernetes list is sorted by name, so a position-based check rejected every CRD add/delete).
    let old_l = bound_listeners(old);
    let new_l = bound_listeners(new);
    for port in old_l
        .keys()
        .chain(new_l.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        match (old_l.get(port), new_l.get(port)) {
            (Some(_), None) | (None, Some(_)) => cold.push(format!("listeners[{port}].port")),
            (Some((oc, ok)), Some((nc, nk))) => {
                if oc != nc {
                    cold.push(format!("listeners[{port}].tls.cert"));
                }
                if ok != nk {
                    cold.push(format!("listeners[{port}].tls.key"));
                }
            }
            (None, None) => {}
        }
    }

    // `tls.clientAuth` of an HTTP listener is loaded into it when it is built: a reload that adds, drops or changes the rule
    // would otherwise keep the old one (a client certificate still required, or still not) without a word.
    let old_ca = https_client_auth_by_port(old);
    let new_ca = https_client_auth_by_port(new);
    for (port, old_rule) in &old_ca {
        if let Some(new_rule) = new_ca.get(port) {
            if old_rule != new_rule {
                cold.push(format!("listeners[{port}].tls.clientAuth"));
            }
        }
    }

    // TCP sites: the services are registered once, at start, with their `tcp` block, so adding, removing or changing
    // one (its targets, limits or timeouts) would silently keep the old behaviour until a restart (issue #757).
    let old_t = tcp_by_port(old);
    let new_t = tcp_by_port(new);
    for port in old_t
        .keys()
        .chain(new_t.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if old_t.get(port) != new_t.get(port) {
            cold.push(format!("sites[port {port}].tcp"));
        }
    }

    // `tls` of a TCP site: the certificate, the key and `clientAuth` are loaded when its listener is built (issue #757).
    let old_tt = tcp_tls_by_port(old);
    let new_tt = tcp_tls_by_port(new);
    for port in old_tt
        .keys()
        .chain(new_tt.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if old_tt.get(port) != new_tt.get(port) {
            cold.push(format!("sites[port {port}].tls"));
        }
    }

    // `proxyProtocol`: the pre-handshake hook of HTTPS listeners, the pre-HTTP hook of plain listeners and the accept side
    // of tcp services are built at start, so a new list of trusted sources would keep the old one (and a removed block
    // would keep requiring the header) until a restart (issues #42 and #757).
    let old_p = proxy_protocol_by_port(old);
    let new_p = proxy_protocol_by_port(new);
    for port in old_p
        .keys()
        .chain(new_p.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if !crate::config::schema::ProxyProtocolConfig::same_policy(
            old_p.get(port).copied(),
            new_p.get(port).copied(),
        ) {
            cold.push(format!("sites[port {port}].proxyProtocol"));
        }
    }

    // `tls.acme`: every field is read when the certificate is ordered at start-up and by the renewal loop, which
    // a reload does not restart (issue #555). A change here would silently keep the old account, directory,
    // storage directory or contact. Sites are matched by `host`, the name the certificate is ordered for.
    let old_a = acme_by_host(old);
    let new_a = acme_by_host(new);
    for host in old_a
        .keys()
        .chain(new_a.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        match (old_a.get(host), new_a.get(host)) {
            (Some(o), Some(n)) => {
                for (name, changed) in [
                    ("email", o.email != n.email),
                    ("directory", o.directory != n.directory),
                    ("storage", o.storage != n.storage),
                    ("challenge", o.challenge != n.challenge),
                    ("profile", o.profile != n.profile),
                ] {
                    if changed {
                        cold.push(format!("sites[{host}].tls.acme.{name}"));
                    }
                }
            }
            (Some(_), None) | (None, Some(_)) => cold.push(format!("sites[{host}].tls.acme")),
            (None, None) => {}
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
    fn redis_url_change_is_cold() {
        // the address is `global.redis.url`, else the environment: no environment here, whatever the shell exports
        let with = |url: &str| {
            cfg(&format!(
                r#"{{"global":{{"redis":{{"url":"{url}"}}}},"sites":[{{"port":8080}}]}}"#
            ))
        };
        let none = cfg(r#"{"sites":[{"port":8080}]}"#);
        assert!(
            detect_cold_changes_in(&with("redis://a:6379"), &with("redis://a:6379"), None)
                .is_empty()
        );
        for (old, new) in [
            (&with("redis://a:6379"), &with("redis://b:6379")),
            (&none, &with("redis://a:6379")),
            (&with("redis://a:6379"), &none),
        ] {
            let cold = detect_cold_changes_in(old, new, None);
            assert_eq!(cold, vec!["global.redis.url".to_string()], "{cold:?}");
        }
    }

    #[test]
    fn a_tcp_site_change_is_cold() {
        let site = |extra: &str| {
            cfg(&format!(
                r#"{{"sites":[{{"port":3306,"tcp":{{"targets":["db:3306"]{extra}}}}},{{"port":8080}}]}}"#
            ))
        };
        let plain = cfg(r#"{"sites":[{"port":8080}]}"#);
        // identical: nothing
        assert!(detect_cold_changes_in(&site(""), &site(""), None).is_empty());
        let one = vec!["sites[port 3306].tcp".to_string()];
        // a field of the block changed (a cap, a timeout, a target), added, or removed
        for (old, new) in [
            (site(""), site(r#","maxConnections":10"#)),
            (
                site(r#","maxConnections":10"#),
                site(r#","maxConnections":20"#),
            ),
            (site(""), site(r#","idleTimeoutSecs":60"#)),
            (plain.clone(), site("")),
            (site(""), plain),
        ] {
            assert_eq!(detect_cold_changes_in(&old, &new, None), one);
        }
    }

    #[test]
    fn the_tls_of_a_tcp_site_is_cold() {
        let site = |tls: &str| {
            cfg(&format!(
                r#"{{"sites":[{{"port":993,"tcp":{{"targets":["mail:143"]}}{tls}}},{{"port":8080}}]}}"#
            ))
        };
        let tls =
            |cert: &str, extra: &str| format!(r#","tls":{{"cert":"{cert}","key":"k.pem"{extra}}}"#);
        assert!(
            detect_cold_changes_in(&site(&tls("a.pem", "")), &site(&tls("a.pem", "")), None)
                .is_empty()
        );
        let one = vec!["sites[port 993].tls".to_string()];
        for (old, new) in [
            (site(""), site(&tls("a.pem", ""))),                // TLS added
            (site(&tls("a.pem", "")), site("")),                // TLS removed
            (site(&tls("a.pem", "")), site(&tls("b.pem", ""))), // another certificate
            (
                site(&tls("a.pem", "")),
                site(&tls("a.pem", r#","clientAuth":{"ca":"ca.pem"}"#)), // a client certificate now required
            ),
            (
                site(&tls("a.pem", r#","clientAuth":{"ca":"ca.pem"}"#)),
                site(&tls(
                    "a.pem",
                    r#","clientAuth":{"ca":"ca.pem","optional":true}"#,
                )),
            ),
        ] {
            assert_eq!(detect_cold_changes_in(&old, &new, None), one);
        }
    }

    #[test]
    fn a_proxy_protocol_change_is_cold() {
        let site = |extra: &str| {
            cfg(&format!(
                r#"{{"sites":[{{"port":3306,"tcp":{{"targets":["db:3306"]}}{extra}}},{{"port":8080}}]}}"#
            ))
        };
        let pp = |src: &str| format!(r#","proxyProtocol":{{"trustedSources":["{src}"]}}"#);
        assert!(
            detect_cold_changes_in(&site(&pp("10.0.0.0/8")), &site(&pp("10.0.0.0/8")), None)
                .is_empty()
        );
        let one = vec!["sites[port 3306].proxyProtocol".to_string()];
        for (old, new) in [
            (site(""), site(&pp("10.0.0.0/8"))),
            (site(&pp("10.0.0.0/8")), site(&pp("192.0.2.1"))),
            (site(&pp("10.0.0.0/8")), site("")),
        ] {
            assert_eq!(detect_cold_changes_in(&old, &new, None), one);
        }
        let http = |extra: &str| cfg(&format!(r#"{{"sites":[{{"port":8080{extra}}}]}}"#));
        assert_eq!(
            detect_cold_changes_in(&http(""), &http(&pp("10.0.0.0/8")), None),
            vec!["sites[port 8080].proxyProtocol".to_string()]
        );
        // an HTTPS port counts too, under its own default port
        let https = |extra: &str| {
            cfg(&format!(
                r#"{{"sites":[{{"tls":{{"cert":"c.pem","key":"k.pem"}}{extra}}}]}}"#
            ))
        };
        let cold = detect_cold_changes_in(&https(""), &https(&pp("10.0.0.0/8")), None);
        assert_eq!(cold, vec!["sites[port 443].proxyProtocol".to_string()]);
        // the same policy written another way is not a change
        let two = |a: &str, b: &str, extra: &str| {
            https(&format!(
                r#","proxyProtocol":{{"trustedSources":["{a}","{b}"]{extra}}}"#
            ))
        };
        assert!(detect_cold_changes_in(
            &two("10.0.0.0/8", "192.0.2.1", ""),
            &two("192.0.2.1", "10.0.0.0/8", r#","timeoutMs":5000"#),
            None
        )
        .is_empty());
    }

    // the environment is part of the address: the same file with a different environment value is not a change of
    // the file, and a file address wins over the environment
    #[test]
    fn the_environment_is_read_once_and_the_file_address_wins() {
        let none = cfg(r#"{"sites":[{"port":8080}]}"#);
        let file = cfg(r#"{"global":{"redis":{"url":"redis://a:6379"}},"sites":[{"port":8080}]}"#);
        assert!(detect_cold_changes_in(&none, &none, Some("redis://env:6379")).is_empty());
        assert!(detect_cold_changes_in(&file, &file, Some("redis://env:6379")).is_empty());
        let cold = detect_cold_changes_in(&none, &file, Some("redis://env:6379"));
        assert_eq!(cold, vec!["global.redis.url".to_string()], "{cold:?}");
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
    fn the_client_auth_of_an_https_listener_is_cold() {
        let site = |auth: &str| {
            cfg(&format!(
                r#"{{"sites":[{{"port":443,"tls":{{"cert":"c.pem","key":"k.pem"{auth}}}}},{{"port":443,"host":"b.test"}}]}}"#
            ))
        };
        let ca = |name: &str, optional: bool| {
            format!(r#","clientAuth":{{"ca":"{name}","optional":{optional}}}"#)
        };
        assert!(detect_cold_changes_in(
            &site(&ca("a.pem", false)),
            &site(&ca("a.pem", false)),
            None
        )
        .is_empty());
        let one = vec!["listeners[443].tls.clientAuth".to_string()];
        for (old, new) in [
            (site(""), site(&ca("a.pem", false))), // a client certificate becomes required
            (site(&ca("a.pem", false)), site("")), // and the other way
            (site(&ca("a.pem", false)), site(&ca("b.pem", false))), // another CA
            (site(&ca("a.pem", false)), site(&ca("a.pem", true))), // optional now
        ] {
            assert_eq!(detect_cold_changes_in(&old, &new, None), one);
        }
    }

    #[test]
    fn the_client_auth_of_an_acme_https_listener_is_cold_too() {
        // an ACME site decides the listener's rule as well, and a manual site behind it may take over when ACME fails
        let acme = |auth: &str| {
            cfg(&format!(
                r#"{{"sites":[{{"port":443,"host":"a.example.com","tls":{{"acme":{{"email":"a@example.com","directory":"d","storage":"s"}}{auth}}}}},{{"port":443,"host":"b.test","tls":{{"cert":"c.pem","key":"k.pem"}}}}]}}"#
            ))
        };
        let ca = r#","clientAuth":{"ca":"a.pem"}"#;
        assert!(detect_cold_changes_in(&acme(ca), &acme(ca), None).is_empty());
        let one = vec!["listeners[443].tls.clientAuth".to_string()];
        assert_eq!(detect_cold_changes_in(&acme(""), &acme(ca), None), one);
        assert_eq!(detect_cold_changes_in(&acme(ca), &acme(""), None), one);
    }

    #[test]
    fn a_second_https_host_with_its_own_certificate_is_a_hot_reload() {
        // only the first site with a certificate decides the listener's client-certificate rule
        let one = cfg(
            r#"{"sites":[{"port":443,"host":"a.test","tls":{"cert":"a.pem","key":"a.key","clientAuth":{"ca":"ca.pem"}}}]}"#,
        );
        let two = cfg(
            r#"{"sites":[{"port":443,"host":"a.test","tls":{"cert":"a.pem","key":"a.key","clientAuth":{"ca":"ca.pem"}}},{"port":443,"host":"b.test","tls":{"cert":"b.pem","key":"b.key"}}]}"#,
        );
        assert!(detect_cold_changes_in(&one, &two, None).is_empty());
        assert!(detect_cold_changes_in(&two, &one, None).is_empty());
    }

    fn acme_site(email: &str, directory: &str, storage: &str) -> crate::config::schema::AppConfig {
        cfg(&format!(
            r#"{{"sites":[{{"port":443,"host":"a.example.com","tls":{{"acme":{{"email":"{email}","directory":"{directory}","storage":"{storage}"}}}}}}]}}"#
        ))
    }

    #[test]
    fn every_acme_field_that_is_read_only_at_start_up_is_cold() {
        let base = acme_site("ops@example.com", "https://ca.test/dir", "./certs");
        assert!(detect_cold_changes(
            &base,
            &acme_site("ops@example.com", "https://ca.test/dir", "./certs")
        )
        .is_empty());
        for (new, field) in [
            (
                acme_site("other@example.com", "https://ca.test/dir", "./certs"),
                "email",
            ),
            (
                acme_site("ops@example.com", "https://other.test/dir", "./certs"),
                "directory",
            ),
            (
                acme_site("ops@example.com", "https://ca.test/dir", "./elsewhere"),
                "storage",
            ),
            (
                cfg(
                    r#"{"sites":[{"port":443,"host":"a.example.com","tls":{"acme":{"email":"ops@example.com","directory":"https://ca.test/dir","storage":"./certs","profile":"shortlived"}}}]}"#,
                ),
                "profile",
            ),
        ] {
            let cold = detect_cold_changes(&base, &new);
            assert_eq!(
                cold,
                vec![format!("sites[a.example.com].tls.acme.{field}")],
                "{field}"
            );
        }
    }

    #[test]
    fn adding_or_removing_acme_on_a_site_is_cold() {
        let with = acme_site("ops@example.com", "https://ca.test/dir", "./certs");
        let without = cfg(
            r#"{"sites":[{"port":443,"host":"a.example.com","tls":{"cert":"c.pem","key":"k.pem"}}]}"#,
        );
        let cold = detect_cold_changes(&without, &with);
        assert!(
            cold.iter().any(|f| f == "sites[a.example.com].tls.acme"),
            "{cold:?}"
        );
        let cold = detect_cold_changes(&with, &without);
        assert!(
            cold.iter().any(|f| f == "sites[a.example.com].tls.acme"),
            "{cold:?}"
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
    fn shutdown_timeout_change_is_cold() {
        let old = cfg(r#"{"global":{"shutdownTimeoutSecs":10},"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"global":{"shutdownTimeoutSecs":20},"sites":[{"port":8080}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("shutdownTimeoutSecs")),
            "shutdownTimeoutSecs change must be cold: {cold:?}"
        );
    }

    #[test]
    fn backlog_change_is_not_cold() {
        let old = cfg(r#"{"global":{"backlog":128},"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"global":{"backlog":256},"sites":[{"port":8080}]}"#);
        assert!(detect_cold_changes(&old, &new).is_empty());
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
    fn adding_a_site_on_a_new_port_is_cold() {
        let old = cfg(r#"{"sites":[{"port":8080}]}"#);
        let new = cfg(r#"{"sites":[{"port":8080},{"port":9090}]}"#);
        let cold = detect_cold_changes(&old, &new);
        assert!(
            cold.iter().any(|f| f.contains("9090")),
            "a new listener port needs a restart: {cold:?}"
        );
    }

    /// Issue #494 review: sites are matched by listener, not position. A Kubernetes list is sorted by
    /// name, so adding, removing or reordering a site on an already-bound port must stay hot.
    #[test]
    fn adding_removing_or_reordering_sites_on_bound_ports_is_hot() {
        let two = cfg(r#"{"sites":[{"host":"a.test","port":8080},{"host":"c.test","port":8080}]}"#);
        let inserted = cfg(
            r#"{"sites":[{"host":"a.test","port":8080},{"host":"b.test","port":8080},{"host":"c.test","port":8080}]}"#,
        );
        let reordered =
            cfg(r#"{"sites":[{"host":"c.test","port":8080},{"host":"a.test","port":8080}]}"#);
        let one = cfg(r#"{"sites":[{"host":"a.test","port":8080}]}"#);
        for (old, new, what) in [
            (&two, &inserted, "insert"),
            (&inserted, &two, "delete"),
            (&two, &reordered, "reorder"),
            (&two, &one, "delete one of two"),
        ] {
            let cold = detect_cold_changes(old, new);
            assert!(
                cold.is_empty(),
                "{what} on a bound port must be hot: {cold:?}"
            );
        }
    }

    /// A plain site listed before a TLS site on the same port does not hide the TLS site's cert/key:
    /// the listener serves the first complete pair, so editing it is cold and removing the plain site
    /// is hot.
    /// Issue #376: active probes were only planned for the `proxy` map; a `routes[]` entry and a
    /// route whose targets live in `groups` got `healthCheck` settings that nothing acted on.
    #[cfg(feature = "proxy")]
    #[test]
    fn health_check_routes_cover_the_map_the_routes_array_and_groups() {
        let config = cfg(r#"{"sites":[{"port":8080,
                "proxy":{"/m":{"targets":["http://map:1"],"healthCheck":{"intervalSecs":5}}},
                "routes":[
                  {"match":{"path":"/r/**"},
                   "proxy":{"targets":["http://arr:1"],"healthCheck":{"intervalSecs":6}}},
                  {"match":{"path":"/g/**"},
                   "proxy":{"groups":[{"name":"blue","targets":["http://g1:1","http://g2:1"]}],
                            "healthCheck":{"intervalSecs":7}}},
                  {"match":{"path":"/none/**"},
                   "proxy":{"targets":["http://nohc:1"]}}
                ]}]}"#);
        let found: Vec<(Option<u64>, Vec<String>)> = health_check_routes(&config)
            .into_iter()
            .map(|(hc, urls)| (hc.interval_secs, urls))
            .collect();
        assert_eq!(
            found,
            vec![
                (Some(5), vec!["http://map:1".to_owned()]),
                (Some(6), vec!["http://arr:1".to_owned()]),
                (
                    Some(7),
                    vec!["http://g1:1".to_owned(), "http://g2:1".to_owned()]
                ),
            ],
            "the route without healthCheck must not be probed"
        );
    }

    #[test]
    fn a_plain_site_before_a_tls_site_does_not_hide_its_cert() {
        let old = cfg(
            r#"{"sites":[{"host":"a.test","port":8443},{"host":"b.test","port":8443,"tls":{"cert":"old.pem","key":"s.key"}}]}"#,
        );
        let new_cert = cfg(
            r#"{"sites":[{"host":"a.test","port":8443},{"host":"b.test","port":8443,"tls":{"cert":"new.pem","key":"s.key"}}]}"#,
        );
        let cold = detect_cold_changes(&old, &new_cert);
        assert!(
            cold.iter().any(|f| f.contains("tls.cert")),
            "the cert served on the port changed: {cold:?}"
        );
        let plain_removed = cfg(
            r#"{"sites":[{"host":"b.test","port":8443,"tls":{"cert":"old.pem","key":"s.key"}}]}"#,
        );
        assert!(
            detect_cold_changes(&old, &plain_removed).is_empty(),
            "removing the plain site leaves the listener's cert unchanged"
        );
    }

    #[test]
    fn removing_the_only_site_on_a_port_is_cold() {
        let old = cfg(r#"{"sites":[{"port":8080},{"port":9090}]}"#);
        let new = cfg(r#"{"sites":[{"port":8080}]}"#);
        assert!(detect_cold_changes(&old, &new)
            .iter()
            .any(|f| f.contains("9090")));
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
