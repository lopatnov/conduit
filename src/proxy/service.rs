use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
#[cfg(feature = "cache")]
use pingora_cache::storage::Storage as CacheStorage;
use pingora_cache::{CacheKey, NoCacheReason, RespCacheable};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use prometheus::{CounterVec, HistogramVec};

use crate::config::schema::{
    ApiKeyConfig, AppConfig, BasicAuthConfig, ConnectionPoolConfig, CorsConfig, HealthCheckConfig,
    IpFilterConfig, LimitsConfig, MiddlewareEntry, ProxyTimeout, RateLimitConfig,
};
use crate::filter::chain::AllowedHostsGuard;
#[cfg(feature = "consumers")]
use crate::filter::chain::ConsumersGuard;
#[cfg(feature = "fault-injection")]
use crate::filter::chain::FaultInjectionGuard;
#[cfg(feature = "forward-auth")]
use crate::filter::chain::ForwardAuthGuard;
use crate::filter::chain::{
    ApiKeyGuard, BasicAuthGuard, CorsPreflight, FilterChain, FilterContext, HealthBypass, IpGuard,
    LimitsGuard, MiddlewareGuard, RateLimitGuard, RedirectGuard, XRequestIdGuard,
};
use crate::filter::rate_limit::{self, RateLimiter};
#[cfg(feature = "redis")]
use crate::filter::rate_limit_redis::RedisRateLimiter;
use crate::filter::{compression, cors, logging, redirects, response_time, security_headers};
#[cfg(feature = "acme")]
use crate::handler::acme_challenge as acme_handler;
use crate::handler::response;
use crate::handler::{
    fallback, health, hot_reload as hot_reload_handler, metrics as metrics_handler, static_files,
    LocalHandlerImpl,
};
use crate::proxy::cache as proxy_cache;
#[cfg(feature = "cache")]
use crate::proxy::cache_disk;
#[cfg(feature = "redis")]
use crate::proxy::cache_redis;
use crate::proxy::ctx::{AcceptEncoding, LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::{router, upstream};
use crate::util::log_writer::LogWriter;

// ── Prometheus metrics (registered once per process) ─────────────────────────

static METRICS: OnceLock<Arc<ConduitMetrics>> = OnceLock::new();

pub struct ConduitMetrics {
    pub requests_total: CounterVec,
    pub request_duration_seconds: HistogramVec,
    /// Incremented for every proxy response served from the cache (phase = Hit).
    pub cache_hits_total: CounterVec,
    /// Incremented for every proxy cache miss (phase = Miss or Expired).
    pub cache_misses_total: CounterVec,
    /// Gauge: number of HTTP requests currently being processed.
    ///
    /// Tracks inflight requests using Prometheus rather than a plain AtomicUsize so
    /// the value is visible in the `/metrics` endpoint alongside other counters.
    pub active_connections: prometheus::Gauge,
    /// Incremented every time an upstream returns a 5xx status.
    ///
    /// Labels: `route` (matched route prefix), `status` (e.g. "500").
    pub upstream_errors_total: CounterVec,
    /// Incremented on every retry attempt triggered by `retry.conditions`.
    ///
    /// Labels: `route`, `condition` ("5xx" | "connection_error" | "timeout").
    pub retry_attempts_total: CounterVec,
    /// Incremented when a request is rejected by the rate limiter (429).
    ///
    /// Labels: `site` (host:port or "*").
    pub rate_limit_rejected_total: CounterVec,
    /// Total requests proxied to each upstream URL (including retries).
    ///
    /// Labels: `upstream` (full URL), `status` (e.g. "200", "502", "0" for
    /// connection errors).
    pub upstream_requests_total: CounterVec,
    /// Upstream response latency histogram (seconds from request sent to response
    /// received), keyed by upstream URL.
    ///
    /// Label: `upstream` (full URL).
    pub upstream_latency_seconds: HistogramVec,
    /// Current active (in-flight) connections to each upstream URL.
    ///
    /// Incremented when a request is forwarded to an upstream; decremented in
    /// `logging()`.  Label: `upstream` (full URL).
    pub upstream_active_connections: prometheus::GaugeVec,
    /// Mean task-poll duration in milliseconds for the admin/background Tokio
    /// runtime, updated every second.  A rising value indicates event-loop
    /// saturation (CPU starvation or I/O stall).
    ///
    /// Present only when compiled with `--features tokio-metrics`.
    #[cfg(feature = "tokio-metrics")]
    pub eventloop_lag_ms: prometheus::Gauge,
}

impl ConduitMetrics {
    pub fn global() -> Arc<Self> {
        METRICS
            .get_or_init(|| {
                let requests_total = prometheus::register_counter_vec!(
                    "conduit_requests_total",
                    "Total number of HTTP requests handled",
                    &["method", "status"]
                )
                .expect("register conduit_requests_total");

                let request_duration_seconds = prometheus::register_histogram_vec!(
                    "conduit_request_duration_seconds",
                    "HTTP request duration in seconds",
                    &["method", "status"],
                    vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]
                )
                .expect("register conduit_request_duration_seconds");

                let cache_hits_total = prometheus::register_counter_vec!(
                    "conduit_cache_hits_total",
                    "Number of proxy responses served from the cache",
                    &["route"]
                )
                .expect("register conduit_cache_hits_total");

                let cache_misses_total = prometheus::register_counter_vec!(
                    "conduit_cache_misses_total",
                    "Number of proxy cache misses (upstream was contacted)",
                    &["route"]
                )
                .expect("register conduit_cache_misses_total");

                let active_connections = prometheus::register_gauge!(
                    "conduit_active_connections",
                    "Number of HTTP requests currently being processed"
                )
                .expect("register conduit_active_connections");

                let upstream_errors_total = prometheus::register_counter_vec!(
                    "conduit_upstream_errors_total",
                    "Number of upstream 5xx responses",
                    &["route", "status"]
                )
                .expect("register conduit_upstream_errors_total");

                let retry_attempts_total = prometheus::register_counter_vec!(
                    "conduit_retry_attempts_total",
                    "Number of upstream retry attempts",
                    &["route", "condition"]
                )
                .expect("register conduit_retry_attempts_total");

                let rate_limit_rejected_total = prometheus::register_counter_vec!(
                    "conduit_rate_limit_rejected_total",
                    "Number of requests rejected by rate limiting (429)",
                    &["site"]
                )
                .expect("register conduit_rate_limit_rejected_total");

                let upstream_requests_total = prometheus::register_counter_vec!(
                    "conduit_upstream_requests_total",
                    "Total requests forwarded to each upstream URL",
                    &["upstream", "status"]
                )
                .expect("register conduit_upstream_requests_total");

                let upstream_latency_seconds = prometheus::register_histogram_vec!(
                    "conduit_upstream_latency_seconds",
                    "Upstream response latency in seconds (request sent → response received)",
                    &["upstream"],
                    vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]
                )
                .expect("register conduit_upstream_latency_seconds");

                let upstream_active_connections = prometheus::register_gauge_vec!(
                    "conduit_upstream_active_connections",
                    "Current number of in-flight requests to each upstream URL",
                    &["upstream"]
                )
                .expect("register conduit_upstream_active_connections");

                #[cfg(feature = "tokio-metrics")]
                let eventloop_lag_ms = prometheus::register_gauge!(
                    "conduit_eventloop_lag_ms",
                    "Mean task-poll duration in milliseconds for the background/admin Tokio \
                     runtime (proxy for event-loop saturation). Updated every second."
                )
                .expect("register conduit_eventloop_lag_ms");

                Arc::new(Self {
                    requests_total,
                    request_duration_seconds,
                    cache_hits_total,
                    cache_misses_total,
                    active_connections,
                    upstream_errors_total,
                    retry_attempts_total,
                    rate_limit_rejected_total,
                    upstream_requests_total,
                    upstream_latency_seconds,
                    upstream_active_connections,
                    #[cfg(feature = "tokio-metrics")]
                    eventloop_lag_ms,
                })
            })
            .clone()
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

/// Shared state for the entire Conduit process.
///
/// `AppState` lives behind an `Arc` that is cloned once per Pingora worker
/// thread at startup.  Every field must be `Send + Sync`.
///
/// ## Hot-reload
/// The config field is an [`ArcSwap`] — `POST /reload` atomically swaps in a
/// new `AppConfig` while in-flight requests finish with the old snapshot.
/// Rate-limiter and round-robin counters are cleared on reload; all other state
/// (health registry, inflight counter, log writer) persists across reloads.
pub struct AppState {
    pub config: Arc<ArcSwap<AppConfig>>,
    pub inflight: Arc<AtomicUsize>,
    /// Per-route round-robin counters shared across all request threads.
    pub round_robin: Arc<DashMap<String, AtomicUsize>>,
    /// Token-bucket rate-limiter state, keyed by client IP or header value.
    pub rate_limiter: Arc<RateLimiter>,
    /// Prometheus metrics counters and histograms.
    pub metrics: Arc<ConduitMetrics>,
    /// Access-log writer — shared across all worker threads.
    pub log_writer: Arc<LogWriter>,
    /// Per-upstream health state and least-conn inflight counts.
    pub upstream_health: Arc<UpstreamRegistry>,
    /// Path to the config file — used by `POST /reload` to re-read and hot-swap.
    pub config_path: PathBuf,
    /// Active ACME HTTP-01 challenge tokens: `token → key_authorization`.
    ///
    /// Populated by the ACME flow during certificate procurement/renewal;
    /// served to the CA via the `/.well-known/acme-challenge/{token}` handler.
    pub acme_challenges: Arc<DashMap<String, String>>,
    /// Loopback address of the Axum upload server, or `None` when no site
    /// has an `upload` block.  Populated before Pingora starts so the router
    /// can forward matching requests without a config look-up.
    pub upload_addr: Option<SocketAddr>,
    /// Broadcast channel for hot-reload browser signals.
    ///
    /// When the file watcher detects a change in a watched directory, it sends
    /// `()` on this channel.  All active SSE connections (`/__hot-reload__`)
    /// subscribe and forward a `data: reload` event to the browser.
    pub hot_reload_tx: tokio::sync::broadcast::Sender<()>,
    /// Redis-backed rate limiter, instantiated when any site configures
    /// `rateLimit.store: "redis://..."`.  `None` when no site uses Redis
    /// rate limiting.  Requires `--features redis`.
    #[cfg(feature = "redis")]
    pub redis_rate_limiter: Option<Arc<RedisRateLimiter>>,
    /// Number of requests currently in a retry state.
    ///
    /// Used to enforce `retry.budgetPercent`: before allowing a retry the
    /// handler checks `retry_inflight * 100 / inflight ≤ budget_percent`.
    /// Incremented when a retry is approved, decremented in `logging()`.
    pub retry_inflight: Arc<AtomicUsize>,
    /// Dynamically managed IP deny-list, editable via Admin API
    /// `POST /ip-deny` and `DELETE /ip-deny` without a config reload.
    ///
    /// Checked by `IpGuard` in addition to `ipFilter.deny` from the config.
    /// Entries are plain CIDR strings (e.g. `"1.2.3.0/24"`).
    pub dynamic_deny: Arc<std::sync::RwLock<Vec<String>>>,
    /// Per-client-IP concurrent in-flight request counts.
    ///
    /// Used by `LimitsGuard` when `limits.maxConnectionsPerIp` is set.
    /// Incremented at request entry, decremented in `logging()`.
    /// Key: client IP string.  Value: active concurrent request count.
    ///
    /// nginx `ngx_http_limit_conn_module` pattern — limits simultaneous open
    /// requests from a single IP, complementing the per-second rate limit.
    pub ip_conn_counts: Arc<DashMap<String, AtomicUsize>>,
}

impl AppState {
    pub fn new(config: AppConfig, config_path: PathBuf, upload_addr: Option<SocketAddr>) -> Self {
        Self::new_inner(config, config_path, upload_addr)
    }

    /// Create AppState with an optional Redis rate limiter.
    /// Only available when compiled with `--features redis`.
    #[cfg(feature = "redis")]
    pub fn new_with_redis(
        config: AppConfig,
        config_path: PathBuf,
        upload_addr: Option<SocketAddr>,
        redis_rate_limiter: Option<Arc<RedisRateLimiter>>,
    ) -> Self {
        let mut state = Self::new_inner(config, config_path, upload_addr);
        state.redis_rate_limiter = redis_rate_limiter;
        state
    }

    fn new_inner(config: AppConfig, config_path: PathBuf, upload_addr: Option<SocketAddr>) -> Self {
        let (hot_reload_tx, _) = tokio::sync::broadcast::channel(16);
        Self {
            config: Arc::new(ArcSwap::new(Arc::new(config))),
            inflight: Arc::new(AtomicUsize::new(0)),
            round_robin: Arc::new(DashMap::new()),
            rate_limiter: Arc::new(DashMap::new()),
            metrics: ConduitMetrics::global(),
            log_writer: Arc::new(LogWriter::new()),
            upstream_health: Arc::new(UpstreamRegistry::new()),
            config_path,
            acme_challenges: Arc::new(DashMap::new()),
            upload_addr,
            hot_reload_tx,
            #[cfg(feature = "redis")]
            redis_rate_limiter: None,
            retry_inflight: Arc::new(AtomicUsize::new(0)),
            dynamic_deny: Arc::new(std::sync::RwLock::new(Vec::new())),
            ip_conn_counts: Arc::new(DashMap::new()),
        }
    }
}

// ── ConduitProxy ──────────────────────────────────────────────────────────────

/// The Pingora [`ProxyHttp`] implementation that processes every HTTP request.
///
/// `ConduitProxy` is the central routing and middleware engine:
///
/// 1. **`request_filter`** — increments inflight counter, builds `RequestCtx`,
///    runs the `FilterChain` (IP guard → CORS → limits → rate-limit → auth →
///    forward-auth → redirect → scripts/WASM), applies priority shedding.
/// 2. **`upstream_request_filter`** — appends forwarding headers, rewrites paths,
///    fires mirrors, expands JWT header templates.
/// 3. **`upstream_response_filter`** — runs the `ResponseFilterChain` (CRLF strip
///    → inject headers → response transform → response time → retry-on-error →
///    error-mask → middleware).
/// 4. **`logging`** — decrements inflight, updates EWMA/outlier-detection state,
///    writes access log, records Prometheus metrics.
///
/// One `ConduitProxy` is shared (behind `Arc`) across all Pingora worker threads.
pub struct ConduitProxy {
    pub state: Arc<AppState>,
}

impl ConduitProxy {
    /// Check the retry budget and increment `retry_inflight` if a retry is allowed.
    ///
    /// Returns `true` when the retry may proceed, `false` when the budget is
    /// exhausted and the retry should be suppressed.
    ///
    /// Concurrent requests may race past this check, so the budget is a *soft*
    /// limit — occasional over-budget retries are acceptable.
    fn retry_budget_allows(&self, retry: &mut crate::proxy::ctx::RetryState) -> bool {
        if let Some(budget_pct) = retry.budget_percent {
            let inflight = self.state.inflight.load(Ordering::Relaxed).max(1) as f64;
            let current_retries = self.state.retry_inflight.load(Ordering::Relaxed) as f64;
            let limit = (inflight * budget_pct / 100.0).ceil() as usize;
            let current = current_retries as usize;
            if current >= limit {
                tracing::debug!(
                    budget_pct,
                    current_retries,
                    inflight,
                    "retry budget exhausted — suppressing retry"
                );
                return false;
            }
        }
        self.state.retry_inflight.fetch_add(1, Ordering::Relaxed);
        retry.is_retrying = true;
        true
    }

    /// Core request-processing pipeline.
    ///
    /// Called by Pingora's [`ProxyHttp::request_filter`] hook.  Returns
    /// `Ok(true)` when a response has already been written (request handled
    /// locally or rejected), `Ok(false)` to continue to the upstream proxy path.
    ///
    /// Pipeline order:
    /// 1. Inflight counter increment + OTel span start.
    /// 2. Route the request → populate `RequestCtx` with upstream, retry, cache cfg.
    /// 3. Extract per-site guards from config.
    /// 4. Run `FilterChain`: XRequestId → IP filter → CORS preflight → health bypass
    ///    → AllowedHosts → limits → rate-limit → consumers → basic-auth → API-key
    ///    → JWT → forward-auth → redirect → fault injection → middleware.
    /// 5. Per-route rate limit check.
    /// 6. Priority-based load shedding.
    /// 7. JWT claims extraction for header template expansion.
    /// 8. Dispatch to local handler or proxy upstream.
    async fn do_request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Option<RequestCtx>,
    ) -> Result<bool> {
        self.state.inflight.fetch_add(1, Ordering::Relaxed);
        self.state.metrics.active_connections.inc();

        // ── OpenTelemetry: start span for this request ────────────────────────
        // The span is stored in RequestCtx and ended in logging() after the
        // response is sent.  When otlp feature is disabled this block compiles
        // to nothing.
        #[cfg(feature = "otlp")]
        let otel_span_start = {
            use opentelemetry::global;
            use opentelemetry::trace::{SpanKind, Tracer};
            let method = session.req_header().method.as_str().to_owned();
            let path = session.req_header().uri.path().to_owned();
            let name = format!("{method} {path}");
            let tracer = global::tracer("conduit");
            let span = tracer
                .span_builder(name)
                .with_kind(SpanKind::Server)
                .start(&tracer);
            Some(span)
        };

        // ── Gather request metadata before borrowing the config ───────────────
        let request_origin = cors::request_origin(session);
        let is_cors_preflight = cors::is_preflight(session);

        // ── Load config once — extract all per-site filters ───────────────────
        let (
            mut req_ctx,
            ip_cfg,
            limits_cfg,
            rate_limit_cfg,
            basic_auth_cfg,
            api_key_cfg,
            cors_cfg,
            security_cfg,
            redirect_result,
            custom_headers,
            middleware,
            script_method,
            script_path_str,
            script_query,
            script_headers,
            extracted_client_ip,
            fault_injection_cfg,
            jwt_auth_cfg,
            forward_auth_cfg,
            consumers_cfg,
            site_label,
        ) = {
            let config = self.state.config.load();
            let host = extract_host(session);
            let path_and_query = session
                .req_header()
                .uri
                .path_and_query()
                .map(|pq| pq.as_str().to_owned())
                .unwrap_or_else(|| session.req_header().uri.path().to_owned());
            let path = session.req_header().uri.path().to_owned();

            let client_ip = session
                .client_addr()
                .and_then(|a| a.as_inet())
                .map(|a| a.ip().to_string())
                .unwrap_or_default();

            let method = session.req_header().method.as_str().to_owned();
            let query = session.req_header().uri.query().map(str::to_owned);

            // Extract the local port so that port-differentiated virtual hosts
            // (e.g. port 8080 public site vs. port 8081 admin site) are routed
            // to the correct SiteConfig even when no explicit `host` is set.
            // Pingora's SocketAddr wraps std::net::SocketAddr; use as_inet() to
            // reach the standard type and its .port() method.
            let server_port: u16 = session
                .as_ref()
                .server_addr()
                .and_then(|a| a.as_inet())
                .map(|a| a.port())
                .unwrap_or(80);

            let req_ctx = router::route_request(
                &config,
                &host,
                &path,
                &method,
                &session.req_header().headers,
                query.as_deref(),
                &client_ip,
                server_port,
                &self.state.round_robin,
                &self.state.upstream_health,
                self.state.upload_addr,
            );
            let site = config.sites.get(req_ctx.site_idx);

            let ip_cfg = site.and_then(|s| s.ip_filter.clone());
            let limits_cfg = site.and_then(|s| s.limits.clone());
            let rate_limit_cfg = site.and_then(|s| s.rate_limit.clone());
            let basic_auth_cfg = site.and_then(|s| s.basic_auth.clone());
            let api_key_cfg = site.and_then(|s| s.api_key.clone());
            let cors_cfg = site.and_then(|s| s.cors.clone());
            let security_cfg = site.and_then(|s| s.security_headers.clone());
            let redirect_result = site
                .and_then(|s| s.redirects.as_deref())
                .and_then(|rules| redirects::apply_redirects(rules, &path_and_query));
            // Custom response headers defined in site.headers — applied to every response.
            let custom_headers: Vec<(String, String)> = site
                .and_then(|s| s.headers.as_ref())
                .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            // Middleware chain entries for this site.
            let middleware: Vec<MiddlewareEntry> = site
                .and_then(|s| s.middleware.as_ref())
                .cloned()
                .unwrap_or_default();
            // Collect request headers for Rhai scripts / WASM plugins (lower-cased keys).
            // Built lazily: only when at least one middleware entry is configured.
            // On the hot path (no middleware) this avoids O(n_headers) allocations
            // that would otherwise happen on every single request.
            let req_headers_for_script: std::collections::HashMap<String, String> =
                if middleware.is_empty() {
                    Default::default()
                } else {
                    session
                        .req_header()
                        .headers
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|vs| (k.as_str().to_ascii_lowercase(), vs.to_owned()))
                        })
                        .collect()
                };
            // Site label for Prometheus metrics.
            let site_label: String = match site {
                Some(s) => {
                    let host = s.host.as_deref().unwrap_or("*");
                    let port = s.port.unwrap_or(80);
                    format!("{host}:{port}")
                }
                None => "*".to_owned(),
            };

            (
                req_ctx,
                ip_cfg,
                limits_cfg,
                rate_limit_cfg,
                basic_auth_cfg,
                api_key_cfg,
                cors_cfg,
                security_cfg,
                redirect_result,
                custom_headers,
                middleware,
                method,
                path,
                query.unwrap_or_default(),
                req_headers_for_script,
                client_ip,
                site.and_then(|s| s.fault_injection.clone()),
                site.and_then(|s| s.jwt_auth.clone()),
                site.and_then(|s| s.forward_auth.clone()),
                site.and_then(|s| s.consumers.clone()),
                site_label,
            )
        };

        // Parse Accept-Encoding header once and store it in the request context.
        let ae_str = session
            .req_header()
            .headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        req_ctx.accept_enc = AcceptEncoding::parse(ae_str);

        // Compute security headers once; reused both in the full extra_headers set
        // and in the preflight path that injects security headers without CORS headers.
        let sec_only: Vec<(String, String)> = security_cfg
            .as_ref()
            .map(security_headers::header_entries)
            .unwrap_or_default();

        // ── Build planned response headers (CORS + security + custom) ────────
        // These are injected into every response written for this request.
        {
            let cors_hdrs = cors_cfg
                .as_ref()
                .map(|c| cors::response_headers(c, request_origin.as_deref()))
                .unwrap_or_default();
            req_ctx.extra_headers = cors_hdrs
                .into_iter()
                .chain(sec_only.iter().cloned())
                .chain(custom_headers)
                .collect();
        }

        let handler_kind = handler_kind_of(&req_ctx.upstream);

        // ── Guard filters (ip, cors, limits, auth, redirects, scripts) ──────────
        // Extract Host header once for AllowedHostsGuard.
        let incoming_host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let guards = GuardCtx {
            ip_cfg,
            limits_cfg,
            security_cfg: security_cfg.clone(),
            host: incoming_host,
            rate_limit_cfg,
            basic_auth_cfg,
            api_key_cfg,
            cors_cfg,
            redirect_result,
            middleware,
            handler_kind: handler_kind.clone(),
            is_preflight: is_cors_preflight,
            sec_only,
            origin: request_origin,
            extra_headers: req_ctx.extra_headers.clone(),
            script_method,
            script_path: script_path_str,
            script_query,
            script_headers,
            client_ip: extracted_client_ip,
            fault_injection_cfg,
            jwt_auth_cfg: jwt_auth_cfg.clone(), // clone — jwt_cfg needed below for claim extraction
            forward_auth_cfg,
            consumers_cfg,
            site_label,
        };
        if self.run_guard_filters(session, guards).await? {
            return Ok(true);
        }

        // If per-IP connection limiting is configured and the request was allowed,
        // store the client IP so logging() can decrement the counter on completion.
        {
            let config = self.state.config.load();
            if let Some(site) = config.sites.get(req_ctx.site_idx) {
                if site
                    .limits
                    .as_ref()
                    .and_then(|l| l.max_connections_per_ip)
                    .is_some()
                {
                    let ip = session
                        .client_addr()
                        .and_then(|a| a.as_inet())
                        .map(|a| a.ip().to_string())
                        .unwrap_or_default();
                    if !ip.is_empty() {
                        req_ctx.client_ip_for_conn_limit = Some(ip);
                    }
                }
            }
        }

        // ── Per-route rate limiting (applied after site-level guard chain) ──────
        // Checked here — after routing — so we know which route was matched.
        {
            let config = self.state.config.load();
            let path = session.req_header().uri.path().to_owned();
            if let Some(site) = config.sites.get(req_ctx.site_idx) {
                if let Some((rl_cfg, route_key)) = router::find_route_rate_limit(site, &path) {
                    let key = format!(
                        "route:{route_key}:{}",
                        rate_limit::extract_client_key(&rl_cfg, session)
                    );
                    let allowed = {
                        self.state
                            .rate_limiter
                            .entry(key)
                            .or_insert_with(|| {
                                rate_limit::TokenBucket::new(
                                    rl_cfg.limit,
                                    rl_cfg.burst.unwrap_or(0),
                                    rl_cfg.window_secs,
                                )
                            })
                            .try_consume()
                    };
                    if !allowed {
                        let extra = req_ctx.extra_headers.clone();
                        self.state
                            .metrics
                            .rate_limit_rejected_total
                            .with_label_values(&[&format!("route:{}", &route_key)])
                            .inc();
                        response::write_response(
                            session,
                            429,
                            "text/plain",
                            bytes::Bytes::from_static(b"Too Many Requests"),
                            &extra,
                        )
                        .await?;
                        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                        self.state.metrics.active_connections.dec();
                        return Ok(true);
                    }
                }
            }
        }

        // ── Priority-based load shedding (post-routing) ───────────────────────
        // When the site is above its priority threshold, low-priority routes
        // are shed with 503.  Priority is determined solely by the route config
        // (proxy.*.priority).
        //
        // SECURITY: We intentionally do NOT trust the `X-Priority` header from
        // downstream clients — an attacker could send `X-Priority: 100` to
        // bypass load shedding entirely.  The header is stripped below to
        // prevent it from leaking to the upstream as well.
        {
            // Strip X-Priority from the incoming request so it cannot be used
            // by the upstream to grant itself elevated priority on retries.
            let _ = session.req_header_mut().remove_header("x-priority");

            let config = self.state.config.load();
            let path = session.req_header().uri.path().to_owned();
            if let Some(site) = config.sites.get(req_ctx.site_idx) {
                if let Some(limits) = &site.limits {
                    if let (Some(max_inflight), Some(threshold)) =
                        (limits.max_inflight_requests, limits.priority_threshold)
                    {
                        let current = self.state.inflight.load(Ordering::Relaxed) as f64;
                        let load_fraction = current / max_inflight as f64;
                        if load_fraction >= threshold {
                            // Base priority from route config, optionally elevated
                            // by the RFC 9218 standard `Priority: u=<N>` header.
                            // Browsers and CDNs set this header; Conduit maps
                            // urgency 0–7 to 100–2 and takes the maximum so that
                            // clients can signal high urgency but not bypass
                            // server-assigned priority.
                            let route_priority =
                                router::find_route_priority(site, &path).unwrap_or(50);
                            let rfc9218_priority = session
                                .req_header()
                                .headers
                                .get("priority")
                                .and_then(|v| v.to_str().ok())
                                .and_then(router::parse_rfc9218_priority);
                            let effective_priority =
                                rfc9218_priority.map_or(route_priority, |p| route_priority.max(p));
                            if effective_priority < 50 {
                                let extra = req_ctx.extra_headers.clone();
                                response::write_response(
                                    session,
                                    503,
                                    "application/json",
                                    bytes::Bytes::from_static(
                                        b"{\"error\":\"Service Unavailable\",\"reason\":\"load shedding\",\"status\":503}",
                                    ),
                                    &extra,
                                )
                                .await?;
                                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                                self.state.metrics.active_connections.dec();
                                return Ok(true);
                            }
                        }
                    }
                }
            }
        }

        // ── JWT claims extraction for header template substitution ─────────────
        // Only available when compiled with --features jwt.
        #[cfg(feature = "jwt")]
        if let Some(ref jwt_cfg) = jwt_auth_cfg {
            if let Some(auth_hdr) = session
                .req_header()
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
            {
                if let Some(token) = auth_hdr.strip_prefix("Bearer ").map(str::trim) {
                    req_ctx.jwt_claims = crate::filter::jwt::extract_claims(token, jwt_cfg);
                }
            }
        }

        // ── Attach OTel span to request context ───────────────────────────────
        #[cfg(feature = "otlp")]
        {
            req_ctx.otel_span = otel_span_start;
        }

        // ── Dispatch ──────────────────────────────────────────────────────────
        *ctx = Some(req_ctx);
        self.dispatch_local(session, ctx, handler_kind).await
    }

    /// Run all guard filters in pipeline order.
    ///
    /// Returns `Ok(true)` when a filter has already written a response and
    /// decremented the inflight counter (caller must return `Ok(true)` too).
    /// Returns `Ok(false)` to continue to the dispatcher.
    /// Run the guard filter chain for this request.
    ///
    /// Builds a [`FilterChain`] from the pre-computed [`GuardCtx`] and runs it.
    /// Each filter is independent — adding a new guard means implementing
    /// [`crate::filter::chain::RequestFilter`] and pushing it into the chain here,
    /// with no other changes required in this file.
    ///
    /// Returns `Ok(true)` when a filter wrote a rejection response (Pingora
    /// should stop the pipeline), `Ok(false)` to continue with normal dispatch.
    async fn run_guard_filters(&self, session: &mut Session, guards: GuardCtx) -> Result<bool> {
        let is_bypass = matches!(
            guards.handler_kind,
            HandlerKind::Health
                | HandlerKind::AcmeChallenge
                | HandlerKind::HotReloadSse
                | HandlerKind::HotReloadJs
        );

        // Build the chain.  Filters run in the order they are pushed.
        let mut chain = FilterChain::new();

        // 0. X-Request-ID — inject before any other processing so the ID is
        //    available to upstream and all downstream filters.
        chain = chain.push(XRequestIdGuard);

        // 1. IP filter — always pushed so the runtime deny-list (POST /ip-deny)
        //    works even when no static `ipFilter` config is present.
        //    Uses the default empty config when not configured (all IPs pass
        //    unless the dynamic_deny list has entries).
        chain = chain.push(IpGuard {
            cfg: guards.ip_cfg.unwrap_or_default(),
            dynamic_deny: self.state.dynamic_deny.clone(),
        });

        // 2. CORS preflight — runs before auth; browsers send OPTIONS without credentials.
        if let Some(cfg) = guards.cors_cfg {
            chain = chain.push(CorsPreflight {
                cfg,
                is_preflight: guards.is_preflight,
                origin: guards.origin,
                sec_headers: guards.sec_only,
            });
        }

        // 3. Health / ACME / hot-reload bypass — skips all remaining guards.
        chain = chain.push(HealthBypass { bypass: is_bypass });

        // 3a. AllowedHosts: Host header allowlist (after bypass so health is exempt).
        chain = chain.push(AllowedHostsGuard {
            security_cfg: guards.security_cfg.clone(),
            host: guards.host.clone(),
        });

        // 4. Request size / header limits.
        if let Some(cfg) = guards.limits_cfg {
            chain = chain.push(LimitsGuard { cfg });
        }

        // 5. Token-bucket rate limiting.
        if let Some(cfg) = guards.rate_limit_cfg {
            chain = chain.push(RateLimitGuard {
                cfg,
                site_label: guards.site_label.clone(),
            });
        }

        // 6. Consumer model auth (identifies consumer, injects X-Consumer-ID).
        #[cfg(feature = "consumers")]
        if let Some(cfg) = guards.consumers_cfg {
            chain = chain.push(ConsumersGuard {
                cfg,
                path: guards.script_path.clone(),
            });
        }

        // 6a. Basic Auth.
        if let Some(cfg) = guards.basic_auth_cfg {
            chain = chain.push(BasicAuthGuard { cfg });
        }

        // 6b. API-key Auth.
        if let Some(cfg) = guards.api_key_cfg {
            chain = chain.push(ApiKeyGuard { cfg });
        }

        // 6c. JWT Bearer-token Auth.
        #[cfg(feature = "jwt")]
        if let Some(cfg) = guards.jwt_auth_cfg {
            {
                use crate::filter::chain::JwtGuard;
                chain = chain.push(JwtGuard {
                    cfg,
                    path: guards.script_path.clone(),
                });
            }
        }

        // 6d. Forward Auth — delegate to external auth service.
        #[cfg(feature = "forward-auth")]
        if let Some(cfg) = guards.forward_auth_cfg {
            chain = chain.push(ForwardAuthGuard {
                cfg,
                path: guards.script_path.clone(),
            });
        }

        // 7. Redirects.
        chain = chain.push(RedirectGuard {
            result: guards.redirect_result,
        });

        // 8. Fault injection (chaos testing — disabled in production).
        #[cfg(feature = "fault-injection")]
        if let Some(cfg) = guards.fault_injection_cfg {
            chain = chain.push(FaultInjectionGuard { cfg });
        }

        // 9. Middleware pipeline: Rhai scripts + WASM plugins in declared order.
        chain = chain.push(MiddlewareGuard {
            middleware: guards.middleware,
            req_path: guards.script_path,
            method: guards.script_method,
            query: guards.script_query,
            headers: guards.script_headers,
            client_ip: guards.client_ip.clone(),
        });

        let mut ctx = FilterContext {
            session,
            extra_headers: &guards.extra_headers,
            inflight: &self.state.inflight,
            rate_limiter: &self.state.rate_limiter,
            #[cfg(feature = "redis")]
            redis_rate_limiter: self.state.redis_rate_limiter.as_ref(),
            ip_conn_counts: &self.state.ip_conn_counts,
            client_ip: guards.client_ip,
        };

        chain.run(&mut ctx).await
    }

    /// Determine whether the request is allowed by the rate limiter.
    ///
    /// Uses Redis when configured and available; falls back to the in-memory
    /// token-bucket limiter if the Redis connection was not established at startup.
    /// Dispatch a request to the appropriate local handler.
    ///
    /// Returns `Ok(true)` for local handlers (response fully written) or
    /// `Ok(false)` for proxy/upload targets (Pingora continues the pipeline).
    ///
    /// Adding a new local handler: implement [`LocalHandlerImpl`] in its module,
    /// then add one arm to [`Self::build_handler`] — this function stays unchanged.
    async fn dispatch_local(
        &self,
        session: &mut Session,
        ctx: &mut Option<RequestCtx>,
        handler_kind: HandlerKind,
    ) -> Result<bool> {
        // Inject X-Response-Time before building the handler so it is included
        // in the extra_headers that every handler receives.
        if let Some(req_ctx) = ctx.as_mut() {
            let config = self.state.config.load();
            let site = config.sites.get(req_ctx.site_idx);
            let rt_cfg = site.and_then(|s| s.response_time.as_ref());
            if response_time::is_enabled(rt_cfg) {
                let digits = response_time::decimal_digits(rt_cfg);
                let elapsed = req_ctx.start_time.elapsed();
                let value = response_time::format_elapsed(elapsed, digits);
                req_ctx
                    .extra_headers
                    .push(("x-response-time".to_owned(), value));
            }
        }

        let Some(mut handler) = self.build_handler(handler_kind, ctx) else {
            return Ok(false); // HandlerKind::Proxy — let Pingora continue
        };
        handler.handle(session).await?;
        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Build the concrete [`LocalHandlerImpl`] for `kind`, extracting all
    /// required data from `ctx` and `self.state` up-front.
    ///
    /// Returns `None` for [`HandlerKind::Proxy`] (no local handling needed).
    /// To add a new handler, add one arm here — `dispatch_local` is unchanged.
    fn build_handler(
        &self,
        kind: HandlerKind,
        ctx: &Option<RequestCtx>,
    ) -> Option<Box<dyn LocalHandlerImpl>> {
        let extra = ctx
            .as_ref()
            .map(|c| c.extra_headers.clone())
            .unwrap_or_default();

        match kind {
            HandlerKind::Proxy => None,

            HandlerKind::AcmeChallenge => {
                #[cfg(feature = "acme")]
                {
                    let token = if let Some(RequestCtx {
                        upstream: UpstreamTarget::Local(LocalHandler::AcmeChallenge { token }),
                        ..
                    }) = ctx.as_ref()
                    {
                        token.clone()
                    } else {
                        unreachable!()
                    };
                    Some(Box::new(acme_handler::AcmeChallengeHandler {
                        token,
                        challenges: self.state.acme_challenges.clone(),
                        extra_headers: extra,
                    }))
                }
                #[cfg(not(feature = "acme"))]
                None
            }

            HandlerKind::Health => {
                let upstream_infos = self.collect_upstream_infos(ctx);
                Some(Box::new(health::HealthHandler {
                    extra_headers: extra,
                    upstream_infos,
                }))
            }

            HandlerKind::Metrics => {
                let token = if let Some(RequestCtx {
                    upstream: UpstreamTarget::Local(LocalHandler::Metrics { token }),
                    ..
                }) = ctx.as_ref()
                {
                    token.as_deref().map(str::to_owned)
                } else {
                    unreachable!()
                };
                Some(Box::new(metrics_handler::MetricsHandler {
                    token,
                    extra_headers: extra,
                }))
            }

            HandlerKind::StaticFile => {
                let config = self.state.config.load();
                let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                let compress_opts = config
                    .sites
                    .get(site_idx)
                    .and_then(|s| s.compression.as_ref())
                    .and_then(compression::effective);
                let fallback_site = config.sites.get(site_idx).cloned();
                let accept_enc = ctx
                    .as_ref()
                    .map(|c| c.accept_enc.clone())
                    .unwrap_or_default();
                let (roots, options, strip_prefix) = if let Some(RequestCtx {
                    upstream:
                        UpstreamTarget::Local(LocalHandler::StaticFile {
                            roots,
                            options,
                            strip_prefix,
                        }),
                    ..
                }) = ctx.as_ref()
                {
                    (roots.clone(), options.clone(), strip_prefix.clone())
                } else {
                    unreachable!()
                };
                Some(Box::new(static_files::StaticFileHandler {
                    roots,
                    options,
                    strip_prefix,
                    extra_headers: extra,
                    compress_opts,
                    accept_enc,
                    fallback_site,
                }))
            }

            HandlerKind::Fallback => {
                let config = self.state.config.load();
                let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                let site = config.sites.get(site_idx).cloned();
                Some(Box::new(fallback::FallbackHandler {
                    site,
                    extra_headers: extra,
                }))
            }

            HandlerKind::HotReloadJs => Some(Box::new(hot_reload_handler::HotReloadJsHandler {
                extra_headers: extra,
            })),

            HandlerKind::HotReloadSse => {
                let rx = self.state.hot_reload_tx.subscribe();
                Some(Box::new(hot_reload_handler::HotReloadSseHandler {
                    extra_headers: extra,
                    rx: Some(rx),
                }))
            }

            HandlerKind::Overloaded => Some(Box::new(OverloadedHandler {
                extra_headers: extra,
            })),
        }
    }

    /// Collect `(url, is_healthy)` pairs for the health endpoint when
    /// `healthCheck.includeUpstreams` is enabled.
    /// Collect per-upstream health info for the health-check handler.
    ///
    /// Returns an empty vec when `healthCheck.includeUpstreams` is not set.
    /// When enabled, returns extended data per upstream (healthy, latency,
    /// ejection status, consecutive 5xx) drawn from the UpstreamRegistry.
    fn collect_upstream_infos(
        &self,
        ctx: &Option<RequestCtx>,
    ) -> Vec<crate::handler::health::UpstreamHealthInfo> {
        let req_ctx = match ctx.as_ref() {
            Some(c) => c,
            None => return vec![],
        };
        let config = self.state.config.load();
        let site = match config.sites.get(req_ctx.site_idx) {
            Some(s) => s,
            None => return vec![],
        };
        let include = site
            .health_check
            .as_ref()
            .and_then(|hc| match hc {
                HealthCheckConfig::Options(opts) => opts.include_upstreams,
                _ => None,
            })
            .unwrap_or(false);
        if !include {
            return vec![];
        }
        use crate::handler::health::UpstreamHealthInfo;
        use crate::proxy::upstream as us;
        let mut urls: Vec<String> = Vec::new();
        if let Some(proxy) = &site.proxy {
            urls.extend(us::target_urls_from_proxy(proxy));
        }
        if let Some(routes) = &site.routes {
            for rc in routes {
                if let Some(rt) = &rc.proxy {
                    urls.extend(us::target_urls(rt));
                }
            }
        }
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        urls.into_iter()
            .map(|url| {
                let entry = self.state.upstream_health.statuses.get(&url);
                let healthy = entry.as_ref().map(|e| e.healthy).unwrap_or(true);
                let latency_ms = entry.as_ref().and_then(|e| e.latency_ms);
                let ejected = entry
                    .as_ref()
                    .and_then(|e| e.ejected_until_secs)
                    .map(|until| until > now_secs)
                    .unwrap_or(false);
                let consecutive_5xx = entry.as_ref().map(|e| e.consecutive_5xx).unwrap_or(0);
                UpstreamHealthInfo {
                    url,
                    healthy,
                    latency_ms,
                    ejected,
                    consecutive_5xx,
                }
            })
            .collect()
    }

    /// Evaluate whether a connect-phase error should trigger a retry.
    fn try_retry_connect_error(
        &self,
        session: &Session,
        retry: &mut crate::proxy::ctx::RetryState,
        e: &mut Box<pingora_core::Error>,
    ) {
        use pingora_core::ErrorType::*;
        let is_conn_err = matches!(
            e.etype(),
            ConnectRefused
                | ConnectNoRoute
                | ConnectError
                | ConnectProxyFailure
                | BindError
                | SocketError
        );
        let is_timeout = matches!(e.etype(), ConnectTimedout);
        let condition = if is_conn_err {
            "connection_error"
        } else {
            "timeout"
        };
        // Only retry safe/idempotent HTTP methods — RFC 7231 § 4.2.2.
        let method = session.req_header().method.as_str();
        if is_safe_http_method(method)
            && ((is_conn_err && retry.has_condition("connection_error"))
                || (is_timeout && retry.has_condition("timeout")))
            && self.retry_budget_allows(retry)
        {
            e.set_retry(true);
            self.state
                .metrics
                .retry_attempts_total
                .with_label_values(&["<connect>", condition])
                .inc();
        }
    }

    /// Evaluate whether a proxy-phase error should trigger a retry.
    fn try_retry_proxy_error(
        &self,
        session: &Session,
        retry: &mut crate::proxy::ctx::RetryState,
        e: &mut Box<pingora_core::Error>,
    ) {
        use pingora_core::ErrorType::*;
        let is_timeout = matches!(e.etype(), ReadTimedout | WriteTimedout);
        let is_5xx_retry = matches!(e.etype(), Custom("5xx_retry"));
        let condition = if is_timeout { "timeout" } else { "5xx" };
        // Only retry safe/idempotent methods.
        let method = session.req_header().method.as_str();
        if is_safe_http_method(method)
            && ((is_timeout && retry.has_condition("timeout"))
                || (is_5xx_retry && retry.has_condition("5xx")))
            && self.retry_budget_allows(retry)
        {
            e.set_retry(true);
            let route = session.req_header().uri.path().to_owned();
            self.state
                .metrics
                .retry_attempts_total
                .with_label_values(&[&route, condition])
                .inc();
        }
    }
}

// ── ProxyHttp trait implementation ────────────────────────────────────────────

#[async_trait]
impl ProxyHttp for ConduitProxy {
    type CTX = Option<RequestCtx>;

    fn new_ctx(&self) -> Self::CTX {
        None
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        self.do_request_filter(session, ctx).await
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>>
    where
        Self::CTX: Send + Sync,
    {
        let req_ctx = ctx.as_mut().expect("ctx set in request_filter");

        if let Some(ref retry) = req_ctx.retry {
            apply_backoff(retry).await;
        }

        let (addr_str, tls, sni) = resolve_peer_addr(req_ctx)?;

        // For retry attempts (#47 companion fix): restore proxy_upstream_url to
        // the URL for THIS attempt so that logging(), access log, and EWMA
        // tracking all reflect the *actual* upstream that served the response.
        //
        // resolve_peer_addr() already incremented retry.attempt, so the URL
        // used in this call is urls[(attempt - 1) % len].
        if let Some(ref retry) = req_ctx.retry {
            if retry.attempt > 1 {
                // This is a retry (attempt was >0 before incrementing).
                let idx = (retry.attempt - 1) % retry.urls.len();
                req_ctx.proxy_upstream_url = Some(retry.urls[idx].clone());
            }
        }

        let socket_addr: SocketAddr = addr_str.parse().map_err(|_| {
            pingora_core::Error::explain(
                pingora_core::ErrorType::ConnectProxyFailure,
                format!("invalid upstream address: {addr_str}"),
            )
        })?;
        let mut peer = HttpPeer::new(socket_addr, tls, sni);

        // Negotiate HTTP/2 with the upstream when the route sets `http2: true`.
        if req_ctx.proxy_http2 {
            peer.options.alpn = pingora_core::upstreams::peer::ALPN::H2H1;
        }

        // Apply upstream TLS settings (cert verification, custom server name).
        if let UpstreamTarget::Proxy {
            ref upstream_tls,
            ref sni,
            ..
        } = req_ctx.upstream
        {
            if let Some(tls_cfg) = upstream_tls {
                if let Some(verify) = tls_cfg.verify {
                    peer.options.verify_cert = verify;
                    peer.options.verify_hostname = verify;
                }
                if let Some(ref server_name) = tls_cfg.server_name {
                    peer.options.alternative_cn = Some(server_name.clone());
                }
            }
            // Always set the SNI for TLS connections (already done by HttpPeer::new
            // but explicit here for clarity).
            let _ = sni; // sni already used in HttpPeer::new above
        }

        // Derive fallback timeout from `limits.timeoutSecs` on the matched site.
        let limits_timeout_secs = {
            let cfg = self.state.config.load();
            cfg.sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.timeout_secs)
        };

        apply_peer_options(
            &mut peer,
            req_ctx.proxy_timeout.as_ref(),
            req_ctx.proxy_pool.as_ref(),
            limits_timeout_secs,
        );

        Ok(Box::new(peer))
    }

    /// Buffer request body chunks for retry replay (stale-if-error pattern).
    ///
    /// Only buffers when:
    ///   1. The route has `retry` configured.
    ///   2. The body is within `limits.maxBodyBufferBytes` (default 1 MiB).
    ///   3. `body_too_large` flag is not already set.
    ///
    /// Uses the linkerd2-proxy ReplayBody pattern: accumulate `Bytes` chunks
    /// (cheap reference-counted clones) into `RequestCtx.body_buffer`.  On
    /// overflow the buffer is discarded and `body_too_large` is set — retries
    /// still happen but without body replay.
    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<bytes::Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()>
    where
        Self::CTX: Send + Sync,
    {
        let Some(req_ctx) = ctx.as_mut() else {
            return Ok(());
        };

        let chunk_len = body.as_ref().map(|c| c.len()).unwrap_or(0);
        req_ctx.actual_body_bytes += chunk_len as u64;

        // Enforce maxBodyBytes on the ACTUAL received bytes.
        // The LimitsGuard only checks the Content-Length header; chunked clients bypass it.
        if chunk_len > 0 {
            let max_body = {
                let config = self.state.config.load();
                config
                    .sites
                    .get(req_ctx.site_idx)
                    .and_then(|s| s.limits.as_ref())
                    .and_then(|l| l.max_body_bytes)
            };
            if enforce_max_body_bytes(req_ctx, body, chunk_len, max_body) {
                return Ok(());
            }

            // Slow-loris upload defense: leaky-bucket minimum-rate check (#51).
            //
            // Pattern: freenginx `ngx_http_request_body.c` commit b85480cc
            //          (client_body_min_rate).
            //
            // Algorithm:
            //   `excess += chunk_bytes − min_rate × elapsed_secs`
            //
            // "Excess" is the surplus above the minimum rate (positive =
            // client sending fast, negative = client is behind).
            // Surplus is capped at one second's worth (min_rate bytes) to
            // prevent unlimited credit from fast initial bursts.
            // When the client falls more than one second behind
            // (excess < -min_rate) the connection is closed with 408.
            let min_rate_opt = {
                let config = self.state.config.load();
                config
                    .sites
                    .get(req_ctx.site_idx)
                    .and_then(|s| s.limits.as_ref())
                    .and_then(|l| l.min_upload_rate_bytes_per_sec)
                    .filter(|&r| r > 0)
            };
            if let Some(min_rate) = min_rate_opt {
                let now = std::time::Instant::now();
                if let Some(last_chunk) = req_ctx.upload_last_chunk {
                    let elapsed_secs = now.duration_since(last_chunk).as_secs_f64();
                    if upload_rate_step(
                        &mut req_ctx.upload_excess_bytes,
                        chunk_len,
                        min_rate,
                        elapsed_secs,
                    ) {
                        tracing::debug!(
                            min_rate,
                            excess = req_ctx.upload_excess_bytes,
                            "upload rate below minimum — closing connection (408)"
                        );
                        *body = None;
                        return Err(pingora_core::Error::explain(
                            pingora_core::ErrorType::HTTPStatus(408),
                            "upload rate below minUploadRateBytesPerSec",
                        ));
                    }
                }
                // Record timestamp; skip rate check on the first chunk since
                // we have no reference point for elapsed time.
                req_ctx.upload_last_chunk = Some(now);
            }
        }

        // Retry body buffering (separate from size enforcement).
        // Only buffer when retry is configured (otherwise wasteful).
        if req_ctx.retry.is_none() || req_ctx.body_too_large {
            return Ok(());
        }

        if let Some(chunk) = body.as_ref() {
            let max_bytes = {
                let config = self.state.config.load();
                config
                    .sites
                    .get(req_ctx.site_idx)
                    .and_then(|s| s.limits.as_ref())
                    .and_then(|l| l.max_body_buffer_bytes)
                    .unwrap_or(1_048_576) // default 1 MiB
            };
            buffer_body_chunk(req_ctx, chunk, max_bytes);
        }
        Ok(())
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // Record the moment we start forwarding to the upstream so that
        // `logging()` can compute upstream_response_time_ms.
        // Also increment the per-upstream active-connections gauge.
        if let Some(req_ctx) = ctx.as_mut() {
            req_ctx.upstream_start = Some(std::time::Instant::now());
            if let Some(url) = req_ctx.proxy_upstream_url.as_deref() {
                self.state
                    .metrics
                    .upstream_active_connections
                    .with_label_values(&[url])
                    .inc();
                // Per-upstream selection counters (#40).
                crate::proxy::health::record_upstream_selected(&self.state.upstream_health, url);
            }
        }

        append_forwarded_headers(session, upstream_request, &self.state, ctx)?;
        apply_upstream_path_transforms(upstream_request, ctx)?;

        // Request header transformation with optional JWT template substitution.
        {
            let config = self.state.config.load();
            if let Some(req_ctx) = ctx.as_ref() {
                let site = config.sites.get(req_ctx.site_idx);
                if let Some(transform) = site.and_then(|s| s.request_transform.as_ref()) {
                    apply_header_transform_request_with_claims(
                        upstream_request,
                        transform,
                        &req_ctx.jwt_claims,
                    )?;
                }
            }
        }

        // Traffic mirroring: fire-and-forget copy to the mirror backend.
        if let Some(req_ctx) = ctx.as_ref() {
            if let UpstreamTarget::Proxy {
                mirror_url: Some(ref mirror),
                ..
            } = req_ctx.upstream
            {
                fire_mirror_request(mirror, session, upstream_request);
            }
        }

        Ok(())
    }

    /// Phase-ordered response pipeline.
    ///
    /// Builds a [`ResponseFilterChain`] for this request and runs it against
    /// the upstream response headers.  The chain encapsulates all header-level
    /// concerns in six discrete phases:
    ///
    /// 1. CRLF injection protection
    /// 2. Inject CORS + security + custom site headers
    /// 3. Apply `responseTransform` (set / remove headers)
    /// 4. Inject `X-Response-Time`
    /// 5. Trigger Pingora retry on 5xx (when `retry.conditions` includes "5xx")
    /// 6. Set `mask_upstream_body` flag on 5xx (when `maskErrors: true`)
    ///
    /// Adding a new response-side behaviour: implement `ResponseFilter` +
    /// push into `ResponseFilterChain::build()` — no other changes required.
    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        use crate::filter::response_chain::{ResponseFilterChain, ResponseFilterOutcome};

        // RFC 7234 §5.1 Age header (#49): compute age for cache hits.
        //
        // Must happen before the immutable `req_ctx` borrow below so we can
        // write to `ctx`.  The mutable borrow ends at the closing `}`.
        #[cfg(feature = "cache")]
        {
            use pingora_cache::CachePhase;
            if matches!(
                _session.cache.phase(),
                CachePhase::Hit | CachePhase::Stale | CachePhase::StaleUpdating
            ) {
                if let Some(req_ctx_mut) = ctx.as_mut() {
                    req_ctx_mut.cache_age_secs = Some(compute_response_age(upstream_response));
                }
            }
        }

        let req_ctx = match ctx.as_ref() {
            Some(c) => c,
            None => return Ok(()),
        };

        // Ignore 1xx interim responses from upstream (#50) — except 101.
        //
        // Pingora calls `upstream_response_filter` once per 1xx informational
        // response AND once more for the final response.  Running the full
        // ResponseFilterChain on 103 Early Hints, 100 Continue, etc. would
        // wrongly trigger retry / error-mask / CRLF-strip logic on a non-final
        // status.  We return `Ok(())` to let Pingora forward the 1xx to the
        // client unchanged.
        //
        // Source: freenginx `ngx_http_proxy_module.c` commit `fd953ff4` —
        //   ignore unexpected 1xx and continue parsing the real response.
        let status_u16 = upstream_response.status.as_u16();
        if status_u16 != 101 && upstream_response.status.is_informational() {
            tracing::debug!(
                status = status_u16,
                upstream = ?req_ctx.proxy_upstream_url,
                "skipping ResponseFilterChain for interim 1xx response"
            );
            return Ok(());
        }

        // WebSocket upgrade security (#46): reject unexpected protocol upgrades.
        //
        // A `101 Switching Protocols` response from upstream is only permitted
        // when the route explicitly declares `websocket: true`.  Otherwise the
        // upstream is violating the HTTP protocol contract and we drop the
        // connection with 502 to prevent unexpected tunnelling.
        if status_u16 == 101 && !req_ctx.websocket_allowed {
            tracing::warn!(
                upstream = ?req_ctx.proxy_upstream_url,
                "upstream returned 101 Switching Protocols but websocket is not \
                 enabled for this route — rejecting upgrade"
            );
            return Err(pingora_core::Error::explain(
                pingora_core::ErrorType::HTTPStatus(502),
                "upstream attempted unexpected protocol upgrade (set websocket:true to allow)",
            ));
        }

        let config = self.state.config.load();
        let chain = ResponseFilterChain::build(req_ctx, &config);

        // Clone sticky-cookie data before the chain runs so that subsequent
        // mutable borrows of `ctx` (in MaskBody / RetryUpstream arms) do not
        // conflict with the immutable `req_ctx` reference.
        let sticky_cookie: Option<(String, String)> = req_ctx.sticky_set_cookie.clone();

        // The response chain may execute WASM plugins whose .wasm file is
        // read from disk on first load.  Use block_in_place to signal Tokio
        // that this synchronous chain execution may block.
        let run_result = tokio::task::block_in_place(|| chain.run(upstream_response, req_ctx));

        match run_result? {
            ResponseFilterOutcome::Continue => {}

            ResponseFilterOutcome::RetryUpstream => {
                let status = upstream_response.status.as_u16();

                // Failure propagation fix (#47): record the failed upstream attempt
                // BEFORE returning the retry error so that EWMA, consecutive_5xx,
                // outlier detection, and the conn_count slot are all updated for
                // the upstream that actually failed.  Without this, a successful
                // retry on a different backend would silently clear the failure
                // from the passive health record.
                if let Some(req_ctx_mut) = ctx.as_mut() {
                    if let Some(url) = req_ctx_mut.proxy_upstream_url.clone() {
                        let elapsed_us = req_ctx_mut.start_time.elapsed().as_micros() as u64;
                        // Release the connection slot for the failed upstream
                        // immediately — the next upstream_peer() call will
                        // acquire a new slot for the retry target.
                        self.state.upstream_health.conn_dec(&url);
                        crate::proxy::health::record_request_latency(
                            &self.state.upstream_health,
                            &url,
                            elapsed_us,
                            status,
                        );
                        // Trigger outlier detection for the failed upstream.
                        let site_idx = req_ctx_mut.site_idx;
                        if let Some(od) = config
                            .sites
                            .get(site_idx)
                            .and_then(|s| s.outlier_detection.as_ref())
                        {
                            crate::proxy::health::maybe_eject(
                                &self.state.upstream_health,
                                &url,
                                od,
                            );
                        }
                        // Update Prometheus per-upstream metrics for the failed attempt
                        // so the gauge doesn't leak (it was incremented by
                        // upstream_request_filter when we first forwarded to A).
                        self.state
                            .metrics
                            .upstream_active_connections
                            .with_label_values(&[&url])
                            .dec();
                        self.state
                            .metrics
                            .upstream_requests_total
                            .with_label_values(&[&url, &status.to_string()])
                            .inc();
                        if let Some(upstream_secs) = req_ctx_mut
                            .upstream_start
                            .map(|t| t.elapsed().as_secs_f64())
                        {
                            self.state
                                .metrics
                                .upstream_latency_seconds
                                .with_label_values(&[&url])
                                .observe(upstream_secs);
                        }
                        // Reset upstream_start for the retry attempt.
                        req_ctx_mut.upstream_start = None;
                        // Push to failed list for structured logging / future use.
                        req_ctx_mut.failed_upstream_attempts.push((url, status));
                        // Clear current URL — next upstream_peer() will set a new one.
                        req_ctx_mut.proxy_upstream_url = None;
                    }
                }

                // Use new_up() so ErrorSource::Upstream is set — required for
                // should_serve_stale() to recognise this as an upstream error
                // and serve a stale cached response (#48).
                return Err(pingora_core::Error::new_up(pingora_core::ErrorType::Custom(
                    "5xx_retry",
                ))
                .more_context(format!("upstream returned HTTP {status}")));
            }

            ResponseFilterOutcome::MaskBody => {
                if let Some(req_ctx_mut) = ctx.as_mut() {
                    req_ctx_mut.mask_upstream_body = true;
                    let body_len = b"{\"error\":\"Internal Server Error\",\"status\":500}".len();
                    upstream_response.insert_header("content-type", "application/json")?;
                    upstream_response.insert_header("content-length", body_len.to_string())?;
                    upstream_response.set_status(500)?;
                }
            }
        }

        // Sticky-session Set-Cookie injection (#39): when `sticky.secret` is
        // configured the chosen upstream URL is HMAC-signed and returned to
        // the client so future requests pin to the same backend.
        //
        // This runs after the response chain so it is never clobbered by
        // response transforms.  The value is `HttpOnly; SameSite=Lax` so it is
        // not accessible from JavaScript and provides basic CSRF protection.
        if let Some((cookie_name, cookie_val)) = &sticky_cookie {
            let cookie_header = format!(
                "{}={}; Path=/; HttpOnly; SameSite=Lax",
                cookie_name, cookie_val
            );
            let _ = upstream_response.insert_header("set-cookie", cookie_header);
        }

        Ok(())
    }

    /// Enable the cache for proxy routes that carry a `cache` configuration.
    ///
    /// Called by Pingora after `request_filter`; only reached for upstream-bound
    /// requests (local handlers return `Ok(true)` in `request_filter`).
    /// When `maskErrors` is enabled for the site and the upstream returned a 5xx
    /// status, replace every body chunk with a generic JSON error so that
    /// internal details never reach the client.
    fn upstream_response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>> {
        if let Some(req_ctx) = ctx.as_ref() {
            if req_ctx.mask_upstream_body {
                if end_of_stream {
                    // Replace the entire (or last) body chunk with a generic error.
                    *body = Some(Bytes::from_static(
                        b"{\"error\":\"Internal Server Error\",\"status\":500}",
                    ));
                } else {
                    // Discard intermediate chunks — only send the replacement on eos.
                    *body = None;
                }
            }
        }
        Ok(None)
    }

    /// Detect cache hits that are within the early-refresh window (#31).
    ///
    /// Pingora calls `response_filter` for **all** responses — including those
    /// served directly from the cache.  We check whether:
    ///
    /// 1. The route has `cache.earlyRefreshSecs` configured.
    /// 2. The current response is a cache hit (`CachePhase::Hit`).
    /// 3. The remaining TTL (`fresh_until − now`) is within the refresh window.
    ///
    /// When all three are true, we store the upstream URL in `RequestCtx`.
    /// `logging()` will then spawn a fire-and-forget reqwest GET, causing Pingora
    /// to fetch a fresh copy and store it.  The **current** client request still
    /// receives the cached (still valid) response without any latency penalty.
    ///
    /// Source: h2o `lib/common/cache.c` — `H2O_CACHE_FLAG_EARLY_UPDATE`.
    async fn response_filter(
        &self,
        session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        #[cfg(feature = "cache")]
        {
            use pingora_cache::CachePhase;
            use std::time::SystemTime;

            // Only trigger early refresh for cache hits.
            if !matches!(session.cache.phase(), CachePhase::Hit) {
                return Ok(());
            }

            let Some(req_ctx) = ctx.as_mut() else {
                return Ok(());
            };

            // Only when the route has earlyRefreshSecs configured.
            let early_window_secs = req_ctx
                .proxy_cache_cfg
                .as_ref()
                .and_then(|c| c.early_refresh_secs)
                .unwrap_or(0);
            if early_window_secs == 0 {
                return Ok(());
            }

            // Get the upstream URL for the background refresh task.
            let upstream_url = match &req_ctx.proxy_upstream_url {
                Some(url) => url.clone(),
                None => return Ok(()),
            };

            // Check how much TTL remains.
            let remaining_secs = session
                .cache
                .maybe_cache_meta()
                .and_then(|meta| meta.fresh_until().duration_since(SystemTime::now()).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            if proxy_cache::should_early_refresh(remaining_secs, early_window_secs) {
                tracing::debug!(
                    upstream = %upstream_url,
                    remaining_secs,
                    early_window_secs,
                    "cache TTL within early-refresh window — scheduling background refresh"
                );
                req_ctx.early_refresh_upstream_url = Some(upstream_url);
            }
        }
        #[cfg(not(feature = "cache"))]
        let _ = (session, ctx);
        Ok(())
    }

    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // The full cache implementation is only compiled when --features cache is active.
        #[cfg(feature = "cache")]
        {
            let Some(req_ctx) = ctx.as_ref() else {
                return Ok(());
            };
            let Some(ref cfg) = req_ctx.proxy_cache_cfg else {
                return Ok(());
            };

            // Select storage backend based on store string.
            let storage: &'static (dyn CacheStorage + Sync) = if cfg.store == "memory" {
                proxy_cache::cache_storage()
            } else if cfg.store.starts_with("redis://") || cfg.store.starts_with("rediss://") {
                #[cfg(feature = "redis")]
                {
                    match cache_redis::get_or_create(&cfg.store) {
                        Some(s) => s,
                        None => {
                            tracing::warn!(
                                store = %cfg.store,
                                "Redis cache unavailable — caching disabled for this request"
                            );
                            return Ok(());
                        }
                    }
                }
                #[cfg(not(feature = "redis"))]
                {
                    tracing::warn!(
                        store = %cfg.store,
                        "Redis cache requires --features redis — caching disabled"
                    );
                    return Ok(());
                }
            } else if let Some(dir) = cfg.store.strip_prefix("disk:") {
                cache_disk::get_or_create(dir)
            } else {
                tracing::warn!(
                    store = %cfg.store,
                    "unsupported cache store — caching disabled for this route"
                );
                return Ok(());
            };

            // Check request-side policy (method, cookies, authorization, skip-paths).
            let method = session.req_header().method.as_str();
            let path = session.req_header().uri.path();
            let has_cookie = session.req_header().headers.contains_key("cookie");
            let has_authorization = session.req_header().headers.contains_key("authorization");

            if !proxy_cache::should_cache_request(cfg, method, has_cookie, has_authorization, path)
            {
                return Ok(());
            }

            // Pass the cache-key lock to prevent thundering herd on cache miss:
            // only one request fetches from upstream; concurrent requests wait for
            // the cached response instead of all hitting the upstream at once.
            session
                .cache
                .enable(storage, None, None, Some(proxy_cache::cache_lock()), None);
        }
        // Without --features cache the entire block above is absent and we fall through.
        #[cfg(not(feature = "cache"))]
        let _ = (session, ctx);
        Ok(())
    }

    /// Stale-while-revalidate / stale-if-error policy.
    ///
    /// - When `error` is `None` Pingora is asking whether to serve a stale
    ///   response while background revalidation runs (SWR).  We return `true`
    ///   when the route's `staleWhileRevalidateSecs` is non-zero — Pingora has
    ///   already checked the stale window via `CacheMeta.serve_stale_while_revalidate()`.
    ///
    /// - When `error` is `Some(_)` Pingora is asking whether to serve stale on
    ///   upstream error (stale-if-error).  We return `true` when
    ///   `staleIfErrorSecs` is non-zero and the error comes from upstream.
    fn should_serve_stale(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
        error: Option<&pingora_core::Error>,
    ) -> bool {
        let Some(req_ctx) = ctx.as_ref() else {
            return false;
        };
        let Some(ref cfg) = req_ctx.proxy_cache_cfg else {
            return false;
        };
        match error {
            // SWR: serve stale while revalidating if window is configured.
            None => cfg.stale_while_revalidate_secs.unwrap_or(0) > 0,
            // Stale-if-error: serve stale on upstream failure.
            Some(e) => {
                cfg.stale_if_error_secs.unwrap_or(0) > 0
                    && e.esource() == &pingora_core::ErrorSource::Upstream
            }
        }
    }

    /// Build a deterministic cache key: namespace = Host header, primary = scheme:path[?query].
    fn cache_key_callback(&self, session: &Session, ctx: &mut Self::CTX) -> Result<CacheKey>
    where
        Self::CTX: Send + Sync,
    {
        // Use extract_host() so the port suffix is stripped (e.g. "example.com:8080" → "example.com").
        // This keeps the cache key consistent with how the router matches virtual hosts.
        let host_str = extract_host(session);
        let host = host_str.as_str();

        // Derive scheme from whether the matched site has TLS configured.
        let scheme = {
            let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
            let config = self.state.config.load();
            if config
                .sites
                .get(site_idx)
                .and_then(|s| s.tls.as_ref())
                .is_some()
            {
                "https"
            } else {
                "http"
            }
        };

        let uri = &session.req_header().uri;
        let path = uri.path();
        let query = uri.query().filter(|q| !q.is_empty());

        // Vary-based cache key differentiation: include the specified request
        // header values so that different representations are stored separately.
        let vary_headers = {
            let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
            let config = self.state.config.load();
            config.sites.get(site_idx).and_then(|_s| {
                ctx.as_ref()
                    .and_then(|c| c.proxy_cache_cfg.as_ref())
                    .and_then(|cc| cc.vary_headers.clone())
            })
        };

        Ok(proxy_cache::build_cache_key(
            host,
            scheme,
            path,
            query,
            vary_headers.as_deref(),
            Some(&session.req_header().headers),
        ))
    }

    /// Decide whether an upstream response is cacheable.
    ///
    /// Returns [`RespCacheable::Cacheable`] for `200 OK` responses when the
    /// route has a non-zero `ttl_secs`.  Everything else is uncacheable.
    fn response_cache_filter(
        &self,
        _session: &Session,
        resp: &ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<RespCacheable>
    where
        Self::CTX: Send + Sync,
    {
        let cacheable = ctx
            .as_ref()
            .and_then(|c| c.proxy_cache_cfg.as_ref())
            .map(|cfg| proxy_cache::response_cacheable(cfg, resp))
            .unwrap_or(RespCacheable::Uncacheable(NoCacheReason::Custom(
                "no-cache-cfg",
            )));
        Ok(cacheable)
    }

    fn fail_to_connect(
        &self,
        session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut e: Box<pingora_core::Error>,
    ) -> Box<pingora_core::Error> {
        if let Some(req_ctx) = ctx.as_mut() {
            if let Some(retry) = &mut req_ctx.retry {
                if retry.has_attempts_left() {
                    self.try_retry_connect_error(session, retry, &mut e);
                }
            }
        }
        e
    }

    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<pingora_core::Error>,
        ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<pingora_core::Error> {
        let mut e = e.more_context(format!("Peer: {peer}"));
        e.retry
            .decide_reuse(client_reused && !session.as_ref().retry_buffer_truncated());

        if let Some(req_ctx) = ctx.as_mut() {
            if let Some(retry) = &mut req_ctx.retry {
                if retry.has_attempts_left() {
                    self.try_retry_proxy_error(session, retry, &mut e);
                }
            }
        }
        e
    }

    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        // Decrement inflight for proxy requests (local handlers decrement inline).
        self.state.metrics.active_connections.dec();
        // Release per-IP connection slot (nginx limit_conn pattern).
        if let Some(ip) = ctx
            .as_ref()
            .and_then(|c| c.client_ip_for_conn_limit.as_deref())
        {
            if let Some(counter) = self.state.ip_conn_counts.get(ip) {
                let prev = counter.fetch_sub(1, Ordering::Relaxed);
                if prev == 0 {
                    // Prevent wrap-around if there was a race.
                    counter.store(0, Ordering::Relaxed);
                }
            }
        }
        if let Some(req_ctx) = ctx.as_ref() {
            if !matches!(req_ctx.upstream, UpstreamTarget::Local(_)) {
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                // Decrement retry budget counter if this request was a retry.
                if req_ctx
                    .retry
                    .as_ref()
                    .map(|r| r.is_retrying)
                    .unwrap_or(false)
                {
                    self.state.retry_inflight.fetch_sub(1, Ordering::Relaxed);
                }
                // For least-conn routes, release the per-upstream slot.
                if let Some(ref url) = req_ctx.proxy_upstream_url {
                    self.state.upstream_health.conn_dec(url);

                    // Update Peak EWMA latency and passive health tracking.
                    let elapsed_us = req_ctx.start_time.elapsed().as_micros() as u64;
                    let status = session
                        .response_written()
                        .map(|h| h.status.as_u16())
                        .unwrap_or(0);

                    // Passive health check (Caddy pattern):
                    // Count response as failure when it matches unhealthyStatus or
                    // exceeds unhealthyLatencyMs — even if the HTTP status is 2xx.
                    // We signal a failure to record_request_latency by passing 503.
                    let effective_status = {
                        let latency_ms = elapsed_us / 1000;
                        let unhealthy_by_status = !req_ctx.passive_unhealthy_status.is_empty()
                            && req_ctx.passive_unhealthy_status.contains(&status);
                        let unhealthy_by_latency = req_ctx
                            .passive_unhealthy_latency_ms
                            .map(|t| latency_ms > t)
                            .unwrap_or(false);
                        if (unhealthy_by_status || unhealthy_by_latency) && status < 500 {
                            // Override: treat as server error for consecutive_5xx tracking.
                            tracing::debug!(
                                upstream = %url, status, latency_ms,
                                "passive health: counting response as failure \
                                 (unhealthyStatus={unhealthy_by_status}, \
                                  unhealthyLatency={unhealthy_by_latency})"
                            );
                            503
                        } else {
                            status
                        }
                    };

                    crate::proxy::health::record_request_latency(
                        &self.state.upstream_health,
                        url,
                        elapsed_us,
                        effective_status,
                    );

                    // Outlier Detection: eject upstream after threshold 5xx responses.
                    {
                        let config = self.state.config.load();
                        let site = config.sites.get(req_ctx.site_idx);
                        let od_cfg = site.and_then(|s| s.outlier_detection.as_ref());
                        if let Some(od) = od_cfg {
                            crate::proxy::health::maybe_eject(&self.state.upstream_health, url, od);
                        }
                    }
                }
            }
        }

        // Write access log entry.
        let start_time = ctx
            .as_ref()
            .map(|c| c.start_time)
            .unwrap_or_else(std::time::Instant::now);
        {
            let config = self.state.config.load();
            let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
            let site = config.sites.get(site_idx);
            let request_id = session
                .req_header()
                .headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok());
            let upstream_addr = ctx.as_ref().and_then(|c| c.proxy_upstream_url.as_deref());
            let upstream_ms = ctx
                .as_ref()
                .and_then(|c| c.upstream_start)
                .map(|t| t.elapsed().as_millis() as u64);
            logging::write_access_log(
                session,
                start_time,
                site,
                &self.state.log_writer,
                &logging::AccessLogContext {
                    request_id,
                    upstream_addr,
                    upstream_ms,
                },
            );
        }

        // Record Prometheus metrics.
        let method = session.req_header().method.as_str().to_owned();
        let status = session
            .response_written()
            .map(|h| h.status.as_u16().to_string())
            .unwrap_or_else(|| "0".to_owned());
        let elapsed = ctx
            .as_ref()
            .map(|c| c.start_time.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        self.state
            .metrics
            .requests_total
            .with_label_values(&[&method, &status])
            .inc();
        self.state
            .metrics
            .request_duration_seconds
            .with_label_values(&[&method, &status])
            .observe(elapsed);

        // Upstream error counter (5xx status codes from upstream).
        {
            let status_u16 = status.parse::<u16>().unwrap_or(0);
            if status_u16 >= 500 {
                let route = session.req_header().uri.path().to_owned();
                self.state
                    .metrics
                    .upstream_errors_total
                    .with_label_values(&[&route, &status])
                    .inc();
            }
        }

        // Per-upstream metrics: active_connections decrement, requests_total, latency_seconds.
        if let Some(url) = ctx.as_ref().and_then(|c| c.proxy_upstream_url.as_deref()) {
            // Decrement the active-connections gauge now that this request finished.
            self.state
                .metrics
                .upstream_active_connections
                .with_label_values(&[url])
                .dec();

            self.state
                .metrics
                .upstream_requests_total
                .with_label_values(&[url, &status])
                .inc();

            // Per-peer response code breakdown (#40).
            let status_u16 = status.parse::<u16>().unwrap_or(0);
            crate::proxy::health::record_response_status(
                &self.state.upstream_health,
                url,
                status_u16,
            );
            if let Some(upstream_secs) = ctx
                .as_ref()
                .and_then(|c| c.upstream_start)
                .map(|t| t.elapsed().as_secs_f64())
            {
                self.state
                    .metrics
                    .upstream_latency_seconds
                    .with_label_values(&[url])
                    .observe(upstream_secs);
            }
        }

        // Cache hit / miss counters (only for proxy requests with caching enabled).
        if ctx
            .as_ref()
            .and_then(|c| c.proxy_cache_cfg.as_ref())
            .is_some()
        {
            use pingora_cache::CachePhase;
            let route = session.req_header().uri.path().to_owned();
            match session.cache.phase() {
                CachePhase::Hit => {
                    self.state
                        .metrics
                        .cache_hits_total
                        .with_label_values(&[&route])
                        .inc();
                }
                CachePhase::Miss | CachePhase::Expired => {
                    self.state
                        .metrics
                        .cache_misses_total
                        .with_label_values(&[&route])
                        .inc();
                }
                _ => {}
            }
        }

        // ── Early cache refresh (#31) ─────────────────────────────────────────
        // When `response_filter` detected that the cached entry is within the
        // early-refresh window, spawn a fire-and-forget GET to the upstream URL
        // so the cache is refreshed before clients see a stale entry.
        //
        // We intentionally do this in `logging()` rather than `response_filter`
        // so the task is spawned after the response is fully sent to the client,
        // not while the main request is still in flight.
        #[cfg(feature = "cache")]
        if let Some(early_url) = ctx
            .as_ref()
            .and_then(|c| c.early_refresh_upstream_url.as_deref().map(str::to_owned))
        {
            let path = session
                .req_header()
                .uri
                .path_and_query()
                .map(|pq| pq.as_str().to_owned())
                .unwrap_or_default();
            tracing::debug!(upstream = %early_url, path = %path, "spawning early cache refresh");
            tokio::spawn(async move {
                fire_early_refresh(&early_url, &path).await;
            });
        }

        // ── OpenTelemetry: finish span with all request attributes ────────────
        #[cfg(feature = "otlp")]
        if let Some(req_ctx) = ctx.as_mut() {
            if let Some(mut span) = req_ctx.otel_span.take() {
                use opentelemetry::{trace::Span, KeyValue};
                let status_u16 = status.parse::<u16>().unwrap_or(0);
                span.set_attribute(KeyValue::new("http.method", method.clone()));
                span.set_attribute(KeyValue::new(
                    "http.path",
                    session.req_header().uri.path().to_owned(),
                ));
                span.set_attribute(KeyValue::new("http.status_code", status_u16 as i64));
                span.set_attribute(KeyValue::new("http.duration_ms", (elapsed * 1000.0) as i64));
                if let Some(ref url) = req_ctx.proxy_upstream_url {
                    span.set_attribute(KeyValue::new("upstream.url", url.clone()));
                }
                // Attach the X-Request-ID so the trace is correlatable with logs.
                if let Some(rid) = session
                    .req_header()
                    .headers
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                {
                    span.set_attribute(KeyValue::new("request.id", rid.to_owned()));
                }
                // Mark 5xx as errors in the trace.
                if status_u16 >= 500 {
                    span.set_status(opentelemetry::trace::Status::error("upstream returned 5xx"));
                }
                span.end();
            }
        }
    }
}

// ── upstream_peer helpers ─────────────────────────────────────────────────────

/// Apply ±50 % jitter to a backoff duration.
///
/// Uses splitmix64 seeded from current nanoseconds — the same fast RNG used
/// elsewhere in the proxy.  Returns a value in `[ms/2, ms*3/2)`.
pub(crate) fn jitter_backoff_ms(ms: u64) -> u64 {
    if ms == 0 {
        return 0;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let mut x = seed
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(0x6c62272e07bb0142);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    // jitter ∈ [0, ms) → result ∈ [ms/2, ms/2 + ms) = [ms/2, 3ms/2)
    let jitter = x % ms;
    ms / 2 + jitter
}

/// Sleep for the configured backoff duration when this is a retry attempt (not the first try).
///
/// When `retry.backoff_jitter` is `true`, applies ±50 % randomness to spread
/// retries in time and avoid synchronized thundering herds.
/// Returns `true` for HTTP methods that are safe to retry.
///
/// RFC 7231 § 4.2.2 defines **idempotent** methods: a request is idempotent
/// when repeating it has the same effect as sending it once.  We only retry
/// these to prevent double-mutations (double charges, double emails, etc.).
///
/// `PUT` and `DELETE` are technically idempotent but are excluded here because
/// in practice applications often treat them as non-idempotent.  Operators who
/// want to retry them can configure `retry.conditions: ["connection_error"]`
/// without worrying — this function is the default safety gate.
///
/// Safe to retry: GET, HEAD, OPTIONS, TRACE.
fn is_safe_http_method(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "TRACE"
    )
}

/// Leaky-bucket minimum-upload-rate step (#51).
///
/// Updates `excess` (surplus bytes above the minimum rate) and returns
/// `true` when the client has fallen more than one second behind the
/// minimum rate and should be rejected with 408.
///
/// # Arguments
/// - `excess` — running surplus in bytes (positive = fast, negative = slow).
///   Modified in place.
/// - `chunk_len` — bytes received in this chunk.
/// - `min_rate` — minimum acceptable rate in bytes per second.
/// - `elapsed_secs` — seconds elapsed since the previous chunk.
///
/// # Algorithm
/// ```text
/// excess += chunk_len − min_rate × elapsed_secs
/// excess  = min(excess, min_rate)   // cap surplus (no unlimited burst credit)
/// reject  = excess < −min_rate       // more than one second of deficit
/// ```
pub(crate) fn upload_rate_step(
    excess: &mut f64,
    chunk_len: usize,
    min_rate: u64,
    elapsed_secs: f64,
) -> bool {
    *excess += chunk_len as f64 - min_rate as f64 * elapsed_secs;
    *excess = excess.min(min_rate as f64);
    *excess < -(min_rate as f64)
}

/// Enforce the `maxBodyBytes` hard limit on actual received bytes.
///
/// Returns `true` when the limit was exceeded (caller should return early).
/// Mutates `body` to `None` to drop the chunk and sets `body_too_large` on the
/// context.  Logs a warning only on the first violation.
fn enforce_max_body_bytes(
    req_ctx: &mut crate::proxy::ctx::RequestCtx,
    body: &mut Option<bytes::Bytes>,
    chunk_len: usize,
    max_body: Option<u64>,
) -> bool {
    let Some(max) = max_body else {
        return false;
    };
    if req_ctx.actual_body_bytes > max {
        // Drop this chunk — prevents forwarding to upstream.
        *body = None;
        let prev = req_ctx.actual_body_bytes - chunk_len as u64;
        if prev <= max {
            // Log only on first violation.
            tracing::warn!(
                actual = req_ctx.actual_body_bytes,
                max,
                "request body exceeded maxBodyBytes (chunked/no Content-Length) \
                 — body dropped, upstream will receive truncated request"
            );
        }
        req_ctx.body_buffer.clear();
        req_ctx.body_too_large = true;
        return true;
    }
    false
}

/// Buffer a body chunk for retry replay (linkerd ReplayBody pattern).
///
/// Clears the buffer and sets `body_too_large` when adding the chunk would
/// exceed `max_bytes`.
fn buffer_body_chunk(
    req_ctx: &mut crate::proxy::ctx::RequestCtx,
    chunk: &bytes::Bytes,
    max_bytes: u64,
) {
    let current_size: usize = req_ctx.body_buffer.iter().map(|b| b.len()).sum();
    if current_size + chunk.len() > max_bytes as usize {
        // Discard buffer — linkerd pattern: clear on overflow.
        req_ctx.body_buffer.clear();
        req_ctx.body_too_large = true;
        tracing::debug!(
            size = current_size + chunk.len(),
            max = max_bytes,
            "request body exceeded buffer limit — retry will not replay body"
        );
    } else {
        // Cheap clone: Bytes is reference-counted.
        req_ctx.body_buffer.push(chunk.clone());
    }
}

async fn apply_backoff(retry: &RetryState) {
    if retry.attempt > 0 {
        if let Some(ms) = retry.backoff_ms {
            let effective_ms = if retry.backoff_jitter {
                jitter_backoff_ms(ms)
            } else {
                ms
            };
            tokio::time::sleep(Duration::from_millis(effective_ms)).await;
        }
    }
}

/// Resolve the upstream `(addr, tls, sni)` from the request context.
///
/// On a retry the address rotates through the URL list and the attempt counter
/// is incremented.  On the first attempt the values come from `ctx.upstream`.
fn resolve_peer_addr(req_ctx: &mut RequestCtx) -> pingora_core::Result<(String, bool, String)> {
    if let Some(ref mut retry) = req_ctx.retry {
        let url = &retry.urls[retry.attempt % retry.urls.len()];
        let addr = upstream::url_to_host_port(url).ok_or_else(|| {
            pingora_core::Error::explain(
                pingora_core::ErrorType::ConnectProxyFailure,
                format!("invalid upstream address: {url}"),
            )
        })?;
        let tls = upstream::url_is_tls(url);
        let sni = if tls {
            upstream::url_host(url)
        } else {
            String::new()
        };
        retry.attempt += 1;
        Ok((addr, tls, sni))
    } else {
        match &req_ctx.upstream {
            UpstreamTarget::Proxy { addr, tls, sni, .. } => Ok((addr.clone(), *tls, sni.clone())),
            UpstreamTarget::Upload { addr } => Ok((addr.to_string(), false, String::new())),
            UpstreamTarget::Local(_) => Err(pingora_core::Error::explain(
                pingora_core::ErrorType::InternalError,
                "upstream_peer called for local handler",
            )),
        }
    }
}

/// Apply per-route timeout, connection-pool settings, and global limits to an
/// `HttpPeer`.
///
/// Priority (highest → lowest):
/// 1. `proxy.*.timeout.*` — per-route fine-grained timeouts
/// 2. `limits.timeoutSecs` — site-wide fallback timeout
///
/// Compute the RFC 7234 §5.1 `Age` value for a cached response.
///
/// Uses the `Date` header of the stored response to determine how old it is:
/// `age = now − date_header_value`.  This is the "apparent age" formula from
/// RFC 7234 §5.1, which is sufficient for most proxy deployments.
///
/// Returns `0` when the `Date` header is absent or cannot be parsed — the
/// response will still carry an `Age: 0` header, signalling a fresh hit.
#[cfg(feature = "cache")]
fn compute_response_age(resp: &pingora_http::ResponseHeader) -> u64 {
    use std::time::SystemTime;
    resp.headers
        .get("date")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok())
        .and_then(|date| SystemTime::now().duration_since(date).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `limits.timeout_secs` is applied to all three timeout fields only when
/// the corresponding per-route field is absent.
fn apply_peer_options(
    peer: &mut HttpPeer,
    timeout: Option<&ProxyTimeout>,
    pool: Option<&ConnectionPoolConfig>,
    limits_timeout_secs: Option<u64>,
) {
    let fallback_ms = limits_timeout_secs.map(|s| s.saturating_mul(1000));

    // connection_timeout
    peer.options.connection_timeout = timeout
        .and_then(|t| t.connect_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // read_timeout
    peer.options.read_timeout = timeout
        .and_then(|t| t.read_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // write_timeout
    peer.options.write_timeout = timeout
        .and_then(|t| t.send_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // first_byte_timeout — maps to read_timeout when explicitly set, allowing
    // operators to enforce a tight "time to first byte" window independently
    // of per-read I/O timeouts.  Takes precedence over readMs.
    if let Some(first_byte_ms) = timeout.and_then(|t| t.first_byte_ms) {
        peer.options.read_timeout = Some(Duration::from_millis(first_byte_ms));
    }

    if let Some(p) = pool {
        if let Some(secs) = p.idle_timeout_secs {
            peer.options.idle_timeout = Some(Duration::from_secs(secs));
        }
    }
}

// ── Circuit Breaker handler ───────────────────────────────────────────────────

/// Returns `503 Service Unavailable` when all upstreams for the matched route
/// are at the configured `maxConnectionsPerUpstream` limit.
struct OverloadedHandler {
    extra_headers: Vec<(String, String)>,
}

#[async_trait]
impl LocalHandlerImpl for OverloadedHandler {
    async fn handle(&mut self, session: &mut Session) -> pingora_core::Result<()> {
        response::write_response(
            session,
            503,
            "application/json",
            bytes::Bytes::from_static(
                b"{\"error\":\"Service Unavailable\",\"status\":503,\"reason\":\"upstream_overloaded\"}",
            ),
            &self.extra_headers,
        )
        .await
    }
}

#[derive(Clone)]
enum HandlerKind {
    Health,
    AcmeChallenge,
    Metrics,
    StaticFile,
    Fallback,
    HotReloadSse,
    HotReloadJs,
    Proxy,
    /// All upstream connections are at the configured max limit — circuit open.
    /// Returns 503 Service Unavailable without contacting any upstream.
    Overloaded,
}

/// All per-request guard data bundled into one value to keep `run_guard_filters`
/// within clippy's argument-count limit (7).
// Fields that are only read when optional features are compiled in.
// Allow dead_code for the base (no-feature) build — they ARE used with --features full.
#[allow(dead_code)]
struct GuardCtx {
    ip_cfg: Option<IpFilterConfig>,
    limits_cfg: Option<LimitsConfig>,
    /// Security headers config — used by `AllowedHostsGuard`.
    security_cfg: Option<crate::config::schema::SecurityHeadersConfig>,
    /// Incoming `Host` header value — checked against `allowedHosts`.
    host: String,
    rate_limit_cfg: Option<RateLimitConfig>,
    basic_auth_cfg: Option<BasicAuthConfig>,
    api_key_cfg: Option<ApiKeyConfig>,
    cors_cfg: Option<CorsConfig>,
    redirect_result: Option<(String, u16)>,
    /// Middleware chain entries — Rhai `type: "script"` entries are executed
    /// after the built-in filters and redirects.
    middleware: Vec<MiddlewareEntry>,
    handler_kind: HandlerKind,
    is_preflight: bool,
    sec_only: Vec<(String, String)>,
    origin: Option<String>,
    extra_headers: Vec<(String, String)>,
    /// Request info forwarded to Rhai scripts and WASM plugins.
    script_method: String,
    script_path: String,
    script_query: String,
    script_headers: std::collections::HashMap<String, String>,
    /// Remote client IP — used by WASM plugins.
    client_ip: String,
    /// Fault injection config (chaos testing).
    /// Field always present; guard only pushed when `--features fault-injection`.
    fault_injection_cfg: Option<crate::config::schema::FaultInjectionConfig>,
    /// JWT auth config — validated in step 6c.
    jwt_auth_cfg: Option<crate::config::schema::JwtAuthConfig>,
    /// Forward-auth config — validated in step 6d.
    /// Field always present; guard only pushed when `--features forward-auth`.
    forward_auth_cfg: Option<crate::config::schema::ForwardAuthConfig>,
    /// Consumer model auth config.
    /// Field always present; guard only pushed when `--features consumers`.
    consumers_cfg: Option<crate::config::schema::ConsumersConfig>,
    /// Site label for Prometheus metrics (`host:port` or `"*"`).
    site_label: String,
}

/// Classify a request's upstream target into a `HandlerKind` for filter routing.
fn handler_kind_of(upstream: &UpstreamTarget) -> HandlerKind {
    match upstream {
        UpstreamTarget::Local(LocalHandler::Health) => HandlerKind::Health,
        UpstreamTarget::Local(LocalHandler::AcmeChallenge { .. }) => HandlerKind::AcmeChallenge,
        UpstreamTarget::Local(LocalHandler::Metrics { .. }) => HandlerKind::Metrics,
        UpstreamTarget::Local(LocalHandler::StaticFile { .. }) => HandlerKind::StaticFile,
        UpstreamTarget::Local(LocalHandler::HotReloadSse) => HandlerKind::HotReloadSse,
        UpstreamTarget::Local(LocalHandler::HotReloadJs) => HandlerKind::HotReloadJs,
        UpstreamTarget::Local(LocalHandler::Overloaded) => HandlerKind::Overloaded,
        UpstreamTarget::Local(_) => HandlerKind::Fallback,
        _ => HandlerKind::Proxy,
    }
}

fn extract_host(session: &Session) -> String {
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_owned())
        .unwrap_or_default()
}

/// Append `X-Forwarded-For` and `X-Forwarded-Proto` headers to the upstream request.
fn append_forwarded_headers(
    session: &Session,
    upstream_request: &mut RequestHeader,
    state: &AppState,
    ctx: &Option<RequestCtx>,
) -> Result<()> {
    // X-Forwarded-For: chain or start a new entry.
    if let Some(ip) = session
        .client_addr()
        .and_then(|a| a.as_inet())
        .map(|a| a.ip().to_string())
    {
        let xff = match upstream_request
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            Some(existing) => format!("{existing}, {ip}"),
            None => ip,
        };
        upstream_request.insert_header("x-forwarded-for", xff)?;
    }

    // X-Forwarded-Proto: derive from whether the matched site has TLS.
    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
    let proto = if state
        .config
        .load()
        .sites
        .get(site_idx)
        .and_then(|s| s.tls.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    upstream_request.insert_header("x-forwarded-proto", proto)?;

    // X-Forwarded-Host: original Host header so the upstream can reconstruct URLs.
    if let Some(host) = session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
    {
        upstream_request.insert_header("x-forwarded-host", host)?;
    }

    // Via: RFC 7230 §5.7 — identify the proxy hop.
    // Append to any existing Via header rather than replacing it.
    let via_value = match upstream_request
        .headers
        .get("via")
        .and_then(|v| v.to_str().ok())
    {
        Some(existing) => format!("{existing}, 1.1 conduit"),
        None => "1.1 conduit".to_owned(),
    };
    upstream_request.insert_header("via", via_value)?;

    Ok(())
}

/// Apply strip-prefix and path-rewrite transforms for proxy and upload targets.
fn apply_upstream_path_transforms(
    upstream_request: &mut RequestHeader,
    ctx: &Option<RequestCtx>,
) -> Result<()> {
    let Some(ctx_ref) = ctx.as_ref() else {
        return Ok(());
    };
    match &ctx_ref.upstream {
        UpstreamTarget::Proxy {
            strip_prefix,
            rewrite,
            ..
        } => {
            let original = upstream_request.uri.path();
            let path = apply_path_strip(original, strip_prefix.as_deref());
            let path = apply_path_rewrites(&path, rewrite.as_deref());
            if path != upstream_request.uri.path() {
                let new_uri = rebuild_uri(&upstream_request.uri, &path)?;
                upstream_request.set_uri(new_uri);
            }
        }
        #[cfg(feature = "upload")]
        UpstreamTarget::Upload { .. } => {
            upstream_request.insert_header("x-conduit-site-idx", ctx_ref.site_idx.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

/// Strip `prefix` from `path`, returning `"/"` when stripping leaves an empty string.
fn apply_path_strip(path: &str, prefix: Option<&str>) -> String {
    let Some(pfx) = prefix else {
        return path.to_owned();
    };
    let stripped = path.strip_prefix(pfx).unwrap_or("/");
    if stripped.is_empty() {
        "/".to_owned()
    } else {
        stripped.to_owned()
    }
}

/// Apply the first matching rewrite rule to `path` and return the (possibly unchanged) result.
fn apply_path_rewrites(path: &str, rules: Option<&[crate::config::schema::RewriteRule]>) -> String {
    let Some(rules) = rules else {
        return path.to_owned();
    };
    let mut out = path.to_owned();
    for rule in rules {
        match get_rewrite_regex(&rule.from) {
            Some(re) if re.is_match(&out) => {
                out = re.replacen(&out, 1, rule.to.as_str()).into_owned();
                break;
            }
            None => {
                tracing::warn!(
                    pattern = %rule.from,
                    "rewrite rule regex error: invalid pattern (skipped)"
                );
            }
            _ => {}
        }
    }
    out
}

/// Apply request header transform with optional JWT template substitution.
///
/// Supports `{{ jwt.<claim> }}` syntax in header values — replaced with the
/// corresponding claim from the decoded JWT payload.  Unknown claims resolve
/// to an empty string.  Static values (no `{{`) are passed through unchanged.
///
/// Called from `upstream_request_filter` after claims are extracted by
/// `do_request_filter`.
fn apply_header_transform_request_with_claims(
    req: &mut RequestHeader,
    transform: &crate::config::schema::HeaderTransformConfig,
    jwt_claims: &Option<std::collections::HashMap<String, serde_json::Value>>,
) -> pingora_core::Result<()> {
    if let Some(remove) = &transform.remove_headers {
        for name in remove {
            req.headers.remove(name.as_str());
        }
    }
    if let Some(set) = &transform.set_headers {
        for (name, value) in set {
            let resolved = if value.contains("{{") {
                expand_jwt_templates(value, jwt_claims)
            } else {
                value.clone()
            };
            req.insert_header(name.clone(), resolved)?;
        }
    }
    Ok(())
}

/// Expand `{{ jwt.<claim> }}` templates in a string.
///
/// Replaces all occurrences of `{{ jwt.CLAIM }}` with the corresponding value
/// from the JWT payload.  Unknown claims are replaced with an empty string.
/// Non-string claim values are JSON-serialized (e.g. numbers, arrays).
/// Expand `{{ jwt.<claim> }}` templates — exposed for unit tests via `pub(crate)`.
pub(crate) fn expand_jwt_templates(
    template: &str,
    claims: &Option<std::collections::HashMap<String, serde_json::Value>>,
) -> String {
    static JWT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = JWT_RE.get_or_init(|| {
        regex::Regex::new(r"\{\{\s*jwt\.(\w+)\s*\}\}").expect("jwt template regex")
    });

    re.replace_all(template, |caps: &regex::Captures<'_>| {
        let claim_name = &caps[1];
        match claims {
            Some(map) => match map.get(claim_name) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            },
            None => String::new(),
        }
    })
    .into_owned()
}

/// Fire-and-forget a copy of the current request to a mirror backend.
///
/// The mirror task is detached (spawned with `tokio::spawn`); its response is
/// discarded and any error is silently logged at DEBUG level.  The primary
/// request processing is unaffected by mirror success or failure.
///
/// **V1 limitation:** only the method, path, query, and request headers are
/// mirrored.  The request body is not buffered and is therefore not mirrored.
fn fire_mirror_request(mirror_url: &str, session: &Session, upstream_request: &RequestHeader) {
    // Build the mirror URL: base URL + path + query from the upstream request.
    let path_and_query = upstream_request
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| upstream_request.uri.path());

    let target_url = {
        let base = mirror_url.trim_end_matches('/');
        format!("{base}{path_and_query}")
    };

    // Collect request headers (skip hop-by-hop and host).
    let method = upstream_request.method.clone();
    let mut headers = Vec::new();
    for (name, value) in upstream_request.headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "te"
                | "trailer"
                | "upgrade"
                | "proxy-authorization"
                | "proxy-authenticate"
                | "host"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            headers.push((n, v.to_owned()));
        }
    }
    // Add X-Mirrored-From so the mirror can distinguish shadow traffic.
    let primary_host = session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned();
    headers.push(("x-mirrored-from".to_owned(), primary_host));

    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "mirror: failed to build client");
                return;
            }
        };

        let mut req = client.request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
            &target_url,
        );
        for (name, value) in &headers {
            if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) {
                    req = req.header(header_name, header_value);
                }
            }
        }

        match req.send().await {
            Ok(resp) => {
                tracing::debug!(
                    url = %target_url,
                    status = resp.status().as_u16(),
                    "mirror: response received (discarded)"
                );
            }
            Err(e) => {
                tracing::debug!(url = %target_url, error = %e, "mirror: request failed");
            }
        }
    });
}

/// Fire-and-forget an early cache refresh GET to an upstream URL (#31).
///
/// Called from `logging()` when the cached entry's remaining TTL is within
/// `earlyRefreshSecs`.  The response is discarded; the purpose is purely to
/// cause Pingora's cache to be refreshed for the next real client request.
///
/// The request is sent directly to the upstream (not via Pingora), so it
/// bypasses the local request pipeline.  A short 10-second timeout prevents
/// slow upstreams from holding the background task open.
///
/// Source: h2o `lib/common/cache.c` — `H2O_CACHE_FLAG_EARLY_UPDATE`.
#[cfg(feature = "cache")]
async fn fire_early_refresh(upstream_url: &str, path_and_query: &str) {
    let base = upstream_url.trim_end_matches('/');
    let target = format!("{base}{path_and_query}");

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "early refresh: failed to build client");
            return;
        }
    };

    match client
        .get(&target)
        .header("X-Early-Refresh", "1")
        .send()
        .await
    {
        Ok(resp) => {
            tracing::debug!(
                url = %target,
                status = resp.status().as_u16(),
                "early refresh: upstream response received"
            );
        }
        Err(e) => {
            tracing::debug!(url = %target, error = %e, "early refresh: request failed");
        }
    }
}

/// Return a compiled [`regex::Regex`] for `pattern`, using a process-wide cache
/// to avoid recompiling the same pattern on every request.
///
/// Rewrite patterns are plain (un-anchored) regexes so that `replacen` can
/// match anywhere in the path.  Invalid patterns are not stored; the caller
/// should log the error and skip the rule.
fn get_rewrite_regex(pattern: &str) -> Option<regex::Regex> {
    static CACHE: OnceLock<DashMap<String, regex::Regex>> = OnceLock::new();
    let cache = CACHE.get_or_init(DashMap::new);
    if let Some(re) = cache.get(pattern) {
        return Some(re.clone());
    }
    let re = regex::Regex::new(pattern).ok()?;
    cache.insert(pattern.to_owned(), re.clone());
    Some(re)
}

fn rebuild_uri(original: &http::Uri, new_path: &str) -> Result<http::Uri> {
    let pq = match original.query() {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path.to_string(),
    };
    let mut parts = http::uri::Parts::default();
    parts.scheme = original.scheme().cloned();
    parts.authority = original.authority().cloned();
    parts.path_and_query = Some(pq.parse().map_err(|_| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            "failed to build upstream URI",
        )
    })?);
    http::Uri::from_parts(parts).map_err(|_| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            "failed to build upstream URI",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── jitter_backoff_ms ─────────────────────────────────────────────────────

    #[test]
    fn jitter_result_is_within_50_percent_range() {
        // Run 100 times to reduce flakiness from the time-seeded RNG.
        for _ in 0..100 {
            let ms = 200u64;
            let result = jitter_backoff_ms(ms);
            assert!(
                result >= ms / 2 && result < ms + ms / 2,
                "jitter result {result} must be in [{}, {})",
                ms / 2,
                ms + ms / 2
            );
        }
    }

    #[test]
    fn jitter_zero_ms_returns_zero() {
        assert_eq!(jitter_backoff_ms(0), 0);
    }

    #[test]
    fn jitter_one_ms_returns_zero_or_one() {
        let result = jitter_backoff_ms(1);
        // ms=1 → ms/2 = 0, jitter ∈ [0, 1) → result ∈ {0}
        assert!(result < 2, "result {result} out of range for ms=1");
    }

    // ── is_safe_http_method ───────────────────────────────────────────────────

    #[test]
    fn safe_methods_are_safe() {
        assert!(is_safe_http_method("GET"));
        assert!(is_safe_http_method("HEAD"));
        assert!(is_safe_http_method("OPTIONS"));
        assert!(is_safe_http_method("TRACE"));
    }

    #[test]
    fn unsafe_methods_are_not_safe() {
        assert!(!is_safe_http_method("POST"));
        assert!(!is_safe_http_method("PUT"));
        assert!(!is_safe_http_method("DELETE"));
        assert!(!is_safe_http_method("PATCH"));
    }

    #[test]
    fn safe_method_case_insensitive() {
        assert!(is_safe_http_method("get"));
        assert!(is_safe_http_method("Get"));
    }

    // ── apply_path_strip ──────────────────────────────────────────────────────

    #[test]
    fn apply_path_strip_removes_prefix() {
        assert_eq!(apply_path_strip("/api/v1/users", Some("/api")), "/v1/users");
    }

    #[test]
    fn apply_path_strip_no_prefix_returns_unchanged() {
        assert_eq!(apply_path_strip("/api/v1", None), "/api/v1");
    }

    #[test]
    fn apply_path_strip_exact_match_returns_root() {
        // Stripping the exact path leaves an empty string → normalize to "/".
        assert_eq!(apply_path_strip("/api", Some("/api")), "/");
    }

    #[test]
    fn apply_path_strip_no_match_returns_root() {
        // Prefix doesn't match → `strip_prefix` returns None → "/" returned.
        assert_eq!(apply_path_strip("/other", Some("/api")), "/");
    }

    // ── apply_path_rewrites ───────────────────────────────────────────────────

    #[test]
    fn apply_path_rewrites_no_rules_returns_unchanged() {
        assert_eq!(apply_path_rewrites("/v1/users", None), "/v1/users");
    }

    #[test]
    fn apply_path_rewrites_no_match_returns_unchanged() {
        let rules = vec![crate::config::schema::RewriteRule {
            from: "^/api/(.*)".to_owned(),
            to: "/v2/$1".to_owned(),
        }];
        assert_eq!(
            apply_path_rewrites("/other/path", Some(&rules)),
            "/other/path"
        );
    }

    #[test]
    fn apply_path_rewrites_matching_rule_transforms_path() {
        let rules = vec![crate::config::schema::RewriteRule {
            from: "^/v1/(.*)".to_owned(),
            to: "/v2/$1".to_owned(),
        }];
        assert_eq!(apply_path_rewrites("/v1/users", Some(&rules)), "/v2/users");
    }

    #[test]
    fn apply_path_rewrites_first_match_wins() {
        let rules = vec![
            crate::config::schema::RewriteRule {
                from: "^/v1/(.*)".to_owned(),
                to: "/first/$1".to_owned(),
            },
            crate::config::schema::RewriteRule {
                from: "^/v1/(.*)".to_owned(),
                to: "/second/$1".to_owned(),
            },
        ];
        assert_eq!(
            apply_path_rewrites("/v1/users", Some(&rules)),
            "/first/users"
        );
    }

    // ── expand_jwt_templates (additional cases) ───────────────────────────────

    #[test]
    fn expand_jwt_templates_no_template_unchanged() {
        let claims: std::collections::HashMap<String, serde_json::Value> = Default::default();
        let result = expand_jwt_templates("plain-value", &Some(claims));
        assert_eq!(result, "plain-value");
    }

    #[test]
    fn expand_jwt_templates_null_claims_returns_empty() {
        let result = expand_jwt_templates("{{ jwt.sub }}", &None);
        assert_eq!(result, "");
    }

    // ── resolve_peer_addr ─────────────────────────────────────────────────────

    fn make_ctx(upstream: UpstreamTarget) -> RequestCtx {
        RequestCtx::new(0, upstream, None, None, None, false, None, None, None)
    }

    #[test]
    fn resolve_peer_addr_proxy_returns_addr_tls_sni() {
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        let (addr, tls, sni) = resolve_peer_addr(&mut ctx).unwrap();
        assert_eq!(addr, "backend:4000");
        assert!(!tls);
        assert!(sni.is_empty());
    }

    #[test]
    fn resolve_peer_addr_https_sets_tls_and_sni() {
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "api.example.com:443".to_owned(),
            tls: true,
            sni: "api.example.com".to_owned(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        let (addr, tls, sni) = resolve_peer_addr(&mut ctx).unwrap();
        assert_eq!(addr, "api.example.com:443");
        assert!(tls);
        assert_eq!(sni, "api.example.com");
    }

    #[test]
    fn resolve_peer_addr_local_handler_returns_error() {
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        assert!(
            resolve_peer_addr(&mut ctx).is_err(),
            "local handler must return error"
        );
    }

    // ── apply_peer_options ───────────────────────────────────────────────────

    #[test]
    fn apply_peer_options_sets_timeouts() {
        use crate::config::schema::ProxyTimeout;
        // Use IP address to avoid DNS lookup in tests
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let timeout = ProxyTimeout {
            connect_ms: Some(500),
            read_ms: Some(1000),
            send_ms: Some(2000),
            per_try_ms: None,
            first_byte_ms: None,
        };
        apply_peer_options(&mut peer, Some(&timeout), None, None);
        assert_eq!(
            peer.options.connection_timeout,
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(
            peer.options.read_timeout,
            Some(std::time::Duration::from_millis(1000))
        );
        assert_eq!(
            peer.options.write_timeout,
            Some(std::time::Duration::from_millis(2000))
        );
    }

    #[test]
    fn apply_peer_options_uses_fallback_timeout() {
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        // No per-route timeout, but global limits.timeoutSecs = 5s → 5000ms fallback
        apply_peer_options(&mut peer, None, None, Some(5));
        assert_eq!(
            peer.options.connection_timeout,
            Some(std::time::Duration::from_millis(5000))
        );
    }

    #[test]
    fn apply_peer_options_no_timeout_leaves_defaults() {
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        apply_peer_options(&mut peer, None, None, None);
        // No timeout set → remains None (Pingora's default).
        assert!(peer.options.connection_timeout.is_none());
        assert!(peer.options.read_timeout.is_none());
    }

    #[test]
    fn apply_peer_options_sets_idle_timeout() {
        use crate::config::schema::ConnectionPoolConfig;
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let pool = ConnectionPoolConfig {
            max_idle: None,
            idle_timeout_secs: Some(30),
        };
        apply_peer_options(&mut peer, None, Some(&pool), None);
        assert_eq!(
            peer.options.idle_timeout,
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn apply_peer_options_first_byte_ms_overrides_read_timeout() {
        use crate::config::schema::ProxyTimeout;
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let timeout = ProxyTimeout {
            connect_ms: None,
            read_ms: Some(30_000), // 30s general read timeout
            send_ms: None,
            per_try_ms: None,
            first_byte_ms: Some(500), // 500ms first-byte timeout overrides readMs
        };
        apply_peer_options(&mut peer, Some(&timeout), None, None);
        // first_byte_ms takes precedence over read_ms
        assert_eq!(
            peer.options.read_timeout,
            Some(std::time::Duration::from_millis(500))
        );
    }

    // ── compute_response_age ─────────────────────────────────────────────────

    #[cfg(feature = "cache")]
    #[test]
    fn compute_response_age_from_date_header() {
        use http::StatusCode;
        // Build a response with a Date header set 60 seconds in the past.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let date_str = httpdate::fmt_http_date(past);
        let mut resp = pingora_http::ResponseHeader::build(StatusCode::OK, None).unwrap();
        resp.insert_header("date", date_str).unwrap();
        let age = compute_response_age(&resp);
        // Allow ±2s for test execution time.
        assert!(age >= 58 && age <= 62, "age should be ~60s, got {age}");
    }

    #[cfg(feature = "cache")]
    #[test]
    fn compute_response_age_missing_date_returns_zero() {
        use http::StatusCode;
        let resp = pingora_http::ResponseHeader::build(StatusCode::OK, None).unwrap();
        assert_eq!(compute_response_age(&resp), 0);
    }

    #[cfg(feature = "cache")]
    #[test]
    fn compute_response_age_unparseable_date_returns_zero() {
        use http::StatusCode;
        let mut resp = pingora_http::ResponseHeader::build(StatusCode::OK, None).unwrap();
        resp.insert_header("date", "not-a-date").unwrap();
        assert_eq!(compute_response_age(&resp), 0);
    }

    // ── get_rewrite_regex ─────────────────────────────────────────────────────

    #[test]
    fn get_rewrite_regex_compiles_valid_pattern() {
        let re = get_rewrite_regex("^/v1/(.*)");
        assert!(re.is_some(), "valid regex must compile");
        let re = re.unwrap();
        assert!(re.is_match("/v1/users"));
        assert!(!re.is_match("/v2/users"));
    }

    #[test]
    fn get_rewrite_regex_returns_none_for_invalid() {
        let re = get_rewrite_regex("[invalid");
        assert!(re.is_none(), "invalid regex must return None");
    }

    #[test]
    fn get_rewrite_regex_caches_compiled_pattern() {
        // Second call for the same pattern must return a cached copy.
        let r1 = get_rewrite_regex("^/api/(.*)");
        let r2 = get_rewrite_regex("^/api/(.*)");
        assert!(r1.is_some() && r2.is_some(), "both calls must succeed");
    }

    // ── build_handler ─────────────────────────────────────────────────────────

    #[test]
    fn build_handler_proxy_returns_none() {
        let proxy = make_proxy();
        let result = proxy.build_handler(HandlerKind::Proxy, &None);
        assert!(
            result.is_none(),
            "Proxy handler must return None (uses Pingora path)"
        );
    }

    #[test]
    fn build_handler_health_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Health)));
        let result = proxy.build_handler(HandlerKind::Health, &ctx);
        assert!(result.is_some(), "Health handler must return Some");
    }

    #[test]
    fn build_handler_metrics_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Metrics {
            token: None,
        })));
        let result = proxy.build_handler(HandlerKind::Metrics, &ctx);
        assert!(result.is_some(), "Metrics handler must return Some");
    }

    #[test]
    fn build_handler_hot_reload_js_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::HotReloadJs)));
        let result = proxy.build_handler(HandlerKind::HotReloadJs, &ctx);
        assert!(result.is_some(), "HotReloadJs handler must return Some");
    }

    #[test]
    fn build_handler_hot_reload_sse_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::HotReloadSse)));
        let result = proxy.build_handler(HandlerKind::HotReloadSse, &ctx);
        assert!(result.is_some(), "HotReloadSse handler must return Some");
    }

    #[test]
    fn build_handler_static_file_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::StaticFile {
            roots: vec![std::path::PathBuf::from("./dist")],
            options: std::sync::Arc::new(Default::default()),
            strip_prefix: None,
        })));
        let result = proxy.build_handler(HandlerKind::StaticFile, &ctx);
        assert!(result.is_some(), "StaticFile handler must return Some");
    }

    #[test]
    fn build_handler_fallback_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Fallback)));
        let result = proxy.build_handler(HandlerKind::Fallback, &ctx);
        assert!(result.is_some(), "Fallback handler must return Some");
    }

    #[test]
    fn build_handler_overloaded_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Overloaded)));
        let result = proxy.build_handler(HandlerKind::Overloaded, &ctx);
        assert!(result.is_some(), "Overloaded handler must return Some");
    }

    // ── AppState::new ─────────────────────────────────────────────────────────

    #[test]
    fn app_state_new_initializes_correctly() {
        let config = crate::config::schema::AppConfig::default();
        let state = AppState::new(config, std::path::PathBuf::from("."), None);
        // inflight starts at 0
        assert_eq!(state.inflight.load(Ordering::Relaxed), 0);
        // retry_inflight starts at 0
        assert_eq!(state.retry_inflight.load(Ordering::Relaxed), 0);
        // upload_addr is None
        assert!(state.upload_addr.is_none());
        // dynamic_deny starts empty
        assert!(state.dynamic_deny.read().unwrap().is_empty());
    }

    #[test]
    fn app_state_config_path_stored() {
        let config = crate::config::schema::AppConfig::default();
        let path = std::path::PathBuf::from("/etc/conduit/config.json");
        let state = AppState::new(config, path.clone(), None);
        assert_eq!(state.config_path, path);
    }

    // ── retry_budget_allows ───────────────────────────────────────────────────

    fn make_proxy() -> ConduitProxy {
        let config = crate::config::schema::AppConfig::default();
        let state = AppState::new(config, std::path::PathBuf::from("."), None);
        ConduitProxy {
            state: std::sync::Arc::new(state),
        }
    }

    #[test]
    fn retry_budget_allows_without_budget_config() {
        let proxy = make_proxy();
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None, // no budget limit
            is_retrying: false,
        };
        // No budget configured → always allows retry.
        assert!(proxy.retry_budget_allows(&mut retry));
        assert!(retry.is_retrying);
    }

    #[test]
    fn retry_budget_allows_within_budget() {
        let proxy = make_proxy();
        proxy.state.inflight.store(10, Ordering::Relaxed); // 10 inflight
        proxy.state.retry_inflight.store(0, Ordering::Relaxed); // 0 retries
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: Some(50.0), // 50% budget → up to 5 retries
            is_retrying: false,
        };
        assert!(
            proxy.retry_budget_allows(&mut retry),
            "within budget must allow"
        );
        assert!(retry.is_retrying);
    }

    #[test]
    fn retry_budget_denies_when_exhausted() {
        let proxy = make_proxy();
        proxy.state.inflight.store(10, Ordering::Relaxed); // 10 inflight
        proxy.state.retry_inflight.store(10, Ordering::Relaxed); // already 10 retries = 100% of budget
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: Some(50.0), // 50% → max 5, current=10 → denied
            is_retrying: false,
        };
        assert!(
            !proxy.retry_budget_allows(&mut retry),
            "exhausted budget must deny"
        );
        assert!(!retry.is_retrying);
    }

    // ── collect_upstream_infos ────────────────────────────────────────────────

    #[test]
    fn collect_upstream_infos_none_ctx_returns_empty() {
        let proxy = make_proxy();
        let result = proxy.collect_upstream_infos(&None);
        assert!(result.is_empty(), "None ctx must return empty list");
    }

    #[test]
    fn collect_upstream_infos_no_include_upstreams_returns_empty() {
        let config = crate::config::schema::AppConfig {
            sites: vec![crate::config::schema::SiteConfig::default()],
            ..Default::default()
        };
        let state = AppState::new(config, std::path::PathBuf::from("."), None);
        let proxy = ConduitProxy {
            state: std::sync::Arc::new(state),
        };
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Health)));
        let result = proxy.collect_upstream_infos(&ctx);
        assert!(result.is_empty(), "no include_upstreams → empty list");
    }

    // ── ConduitMetrics::global ────────────────────────────────────────────────

    #[test]
    fn conduit_metrics_global_returns_arc() {
        let metrics = ConduitMetrics::global();
        // Just verify it initializes without panicking and returns the same instance.
        let metrics2 = ConduitMetrics::global();
        assert!(
            std::ptr::eq(metrics.as_ref(), metrics2.as_ref()),
            "global() must return the same Arc"
        );
    }

    // ── apply_upstream_path_transforms ───────────────────────────────────────

    #[test]
    fn apply_upstream_path_transforms_strips_prefix() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api/v1/users", None).unwrap();
        let ctx = Some(make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: Some("/api".to_owned()),
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        }));
        apply_upstream_path_transforms(&mut req, &ctx).unwrap();
        assert_eq!(req.uri.path(), "/v1/users");
    }

    #[test]
    fn apply_upstream_path_transforms_no_prefix_unchanged() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api/v1/users", None).unwrap();
        let ctx = Some(make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        }));
        apply_upstream_path_transforms(&mut req, &ctx).unwrap();
        assert_eq!(req.uri.path(), "/api/v1/users");
    }

    #[test]
    fn apply_upstream_path_transforms_none_ctx_noop() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/original", None).unwrap();
        apply_upstream_path_transforms(&mut req, &None).unwrap();
        assert_eq!(req.uri.path(), "/original");
    }

    // ── handler_kind_of ───────────────────────────────────────────────────────

    #[test]
    fn handler_kind_health() {
        let upstream = UpstreamTarget::Local(LocalHandler::Health);
        assert!(matches!(handler_kind_of(&upstream), HandlerKind::Health));
    }

    #[test]
    fn handler_kind_overloaded() {
        let upstream = UpstreamTarget::Local(LocalHandler::Overloaded);
        assert!(matches!(
            handler_kind_of(&upstream),
            HandlerKind::Overloaded
        ));
    }

    #[test]
    fn handler_kind_proxy() {
        let upstream = UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        };
        assert!(matches!(handler_kind_of(&upstream), HandlerKind::Proxy));
    }

    #[test]
    fn handler_kind_fallback_for_unknown_local() {
        let upstream = UpstreamTarget::Local(LocalHandler::Fallback);
        assert!(matches!(handler_kind_of(&upstream), HandlerKind::Fallback));
    }

    #[test]
    fn handler_kind_hot_reload_sse() {
        let upstream = UpstreamTarget::Local(LocalHandler::HotReloadSse);
        assert!(matches!(
            handler_kind_of(&upstream),
            HandlerKind::HotReloadSse
        ));
    }

    // ── apply_header_transform_request_with_claims ────────────────────────────

    #[test]
    fn header_transform_sets_header() {
        use pingora_http::RequestHeader;
        use std::collections::HashMap;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: Some(
                [("x-env".to_owned(), "production".to_owned())]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            remove_headers: None,
        };
        apply_header_transform_request_with_claims(&mut req, &transform, &None).unwrap();
        assert_eq!(req.headers.get("x-env").unwrap(), "production");
    }

    #[test]
    fn header_transform_removes_header() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        req.insert_header("x-remove", "bye").unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: None,
            remove_headers: Some(vec!["x-remove".to_owned()]),
        };
        apply_header_transform_request_with_claims(&mut req, &transform, &None).unwrap();
        assert!(
            req.headers.get("x-remove").is_none(),
            "header must be removed"
        );
    }

    #[test]
    fn header_transform_with_jwt_template_substitution() {
        use pingora_http::RequestHeader;
        use std::collections::HashMap;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: Some(
                [("x-user".to_owned(), "{{ jwt.sub }}".to_owned())]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            remove_headers: None,
        };
        let mut claims = HashMap::new();
        claims.insert("sub".to_owned(), serde_json::json!("alice"));
        apply_header_transform_request_with_claims(&mut req, &transform, &Some(claims)).unwrap();
        assert_eq!(req.headers.get("x-user").unwrap(), "alice");
    }

    // ── rebuild_uri ───────────────────────────────────────────────────────────

    #[test]
    fn rebuild_uri_replaces_path() {
        let original: http::Uri = "/old/path".parse().unwrap();
        let new_uri = rebuild_uri(&original, "/new/path").unwrap();
        assert_eq!(new_uri.path(), "/new/path");
    }

    #[test]
    fn rebuild_uri_preserves_query() {
        let original: http::Uri = "/old/path?foo=bar".parse().unwrap();
        let new_uri = rebuild_uri(&original, "/new/path").unwrap();
        assert_eq!(new_uri.path(), "/new/path");
        assert_eq!(new_uri.query(), Some("foo=bar"));
    }

    #[test]
    fn rebuild_uri_no_query_keeps_no_query() {
        let original: http::Uri = "/path".parse().unwrap();
        let new_uri = rebuild_uri(&original, "/v2").unwrap();
        assert_eq!(new_uri.path(), "/v2");
        assert!(new_uri.query().is_none());
    }

    // ── resolve_peer_addr ─────────────────────────────────────────────────────

    #[test]
    fn resolve_peer_addr_with_retry_returns_first_url() {
        let retry = RetryState {
            urls: vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None,
            is_retrying: false,
        };
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "original:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        ctx.retry = Some(retry);
        let (addr, _, _) = resolve_peer_addr(&mut ctx).unwrap();
        assert_eq!(addr, "a:4000");
        // Attempt should be incremented.
        assert_eq!(ctx.retry.unwrap().attempt, 1);
    }

    // ── upload_rate_step (leaky-bucket minimum rate, #51) ────────────────────

    #[test]
    fn upload_rate_step_at_exact_min_rate_keeps_excess_zero() {
        let mut excess = 0.0f64;
        let min_rate = 1024u64; // 1 KiB/s
                                // Exactly 1024 bytes in exactly 1 second → excess stays at 0.
        let rejected = upload_rate_step(&mut excess, 1024, min_rate, 1.0);
        assert!(!rejected, "exactly at min rate must not reject");
        assert!(
            (excess - 0.0).abs() < 0.01,
            "excess should be ~0, got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_above_min_rate_accumulates_surplus() {
        let mut excess = 0.0f64;
        let min_rate = 1024u64;
        // 2048 bytes in 1 second (twice the minimum rate) → surplus = 1024.
        let rejected = upload_rate_step(&mut excess, 2048, min_rate, 1.0);
        assert!(!rejected, "above min rate must not reject");
        // Surplus capped at min_rate (1024).
        assert!(
            (excess - 1024.0).abs() < 0.01,
            "surplus capped at min_rate: got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_surplus_is_capped_at_one_second() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // Enormous burst: 1_000_000 bytes in 0.01 seconds.
        upload_rate_step(&mut excess, 1_000_000, min_rate, 0.01);
        // Surplus must be capped at min_rate (1000) — not the raw 999_990.
        assert!(
            excess <= min_rate as f64 + 0.01,
            "surplus must be capped at min_rate: got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_below_min_rate_accumulates_deficit() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // 100 bytes in 1 second (1/10 of min rate) → deficit grows.
        let rejected = upload_rate_step(&mut excess, 100, min_rate, 1.0);
        // deficit = 100 - 1000 = -900; not yet past -1000 threshold.
        assert!(
            !rejected,
            "single slow chunk below min rate but deficit < min_rate"
        );
        assert!(excess < 0.0, "excess must be negative (deficit): {excess}");
    }

    #[test]
    fn upload_rate_step_rejects_when_deficit_exceeds_one_second() {
        let mut excess = -(1000f64 - 1.0); // just below the rejection threshold
        let min_rate = 1000u64;
        // One more tiny chunk with a 1-second gap: excess += 1 - 1000 → -1999+1 = -1999
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 1.0);
        assert!(rejected, "deficit > min_rate must trigger rejection");
        assert!(
            excess < -(min_rate as f64),
            "excess must be below -min_rate: {excess}"
        );
    }

    #[test]
    fn upload_rate_step_carries_over_surplus_for_slow_periods() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // First chunk: big burst that fills the surplus bucket.
        upload_rate_step(&mut excess, 10_000, min_rate, 0.0);
        // Surplus capped at 1000.
        assert!((excess - 1000.0).abs() < 0.01, "surplus capped: {excess}");

        // Second chunk: very slow (1 byte in 1 second).
        // excess += 1 - 1000 → 1000 + 1 - 1000 = 1.
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 1.0);
        assert!(!rejected, "surplus from burst must absorb one slow chunk");
        assert!(excess >= 0.0, "excess should remain non-negative: {excess}");
    }

    #[test]
    fn upload_rate_step_zero_elapsed_never_rejects() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // First call always has elapsed=0 (first chunk in request_body_filter).
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 0.0);
        assert!(!rejected, "first chunk (elapsed=0) must never reject");
    }

    // ── ConduitMetrics (tokio-metrics feature) ────────────────────────────────

    /// Verify that `eventloop_lag_ms` gauge is accessible when the feature is
    /// compiled in.  The gauge starts at 0.0 before any probe fires.
    #[cfg(feature = "tokio-metrics")]
    #[test]
    fn eventloop_lag_ms_gauge_is_registered() {
        let metrics = ConduitMetrics::global();
        // Gauge should start at 0.0 (no probe has fired yet).
        assert_eq!(metrics.eventloop_lag_ms.get(), 0.0);
    }

    /// The gauge can be set and read back correctly.
    #[cfg(feature = "tokio-metrics")]
    #[test]
    fn eventloop_lag_ms_gauge_set_and_get() {
        let metrics = ConduitMetrics::global();
        metrics.eventloop_lag_ms.set(3.14);
        // Value should be >= 3.14 (another test may set it too, but we just
        // verify the set → get round-trip works).
        assert!(metrics.eventloop_lag_ms.get() > 0.0);
        // Reset to avoid affecting other tests.
        metrics.eventloop_lag_ms.set(0.0);
    }

    // ── 1xx interim response guard (#50) ─────────────────────────────────────
    //
    // The logic in `upstream_response_filter` that skips the ResponseFilterChain
    // for 1xx (except 101) uses `StatusCode::is_informational()`.  These tests
    // verify the classification of status codes we care about so that a refactor
    // of the guard condition does not silently regress.

    #[test]
    fn status_100_is_informational_and_not_101() {
        let s = http::StatusCode::CONTINUE;
        assert!(s.is_informational());
        assert_ne!(s.as_u16(), 101);
    }

    #[test]
    fn status_103_early_hints_is_informational_and_not_101() {
        // 103 Early Hints — common source of unexpected 1xx from backends
        // (Spring Boot, Caddy, CDNs).  Must be skipped by the guard.
        let s = http::StatusCode::from_u16(103).unwrap();
        assert!(s.is_informational());
        assert_ne!(s.as_u16(), 101);
    }

    #[test]
    fn status_101_is_informational_but_handled_separately() {
        // 101 is informational but should NOT be skipped — the WebSocket
        // upgrade guard runs for 101 specifically.
        let s = http::StatusCode::SWITCHING_PROTOCOLS;
        assert!(s.is_informational());
        assert_eq!(s.as_u16(), 101);
        // Guard condition: status != 101 AND is_informational() → skip.
        // For 101 this is FALSE — so the WebSocket check runs instead.
        assert!(!(s.as_u16() != 101 && s.is_informational()));
    }

    #[test]
    fn status_200_is_not_informational() {
        let s = http::StatusCode::OK;
        assert!(!s.is_informational());
    }

    #[test]
    fn status_500_is_not_informational() {
        let s = http::StatusCode::INTERNAL_SERVER_ERROR;
        assert!(!s.is_informational());
    }
}
