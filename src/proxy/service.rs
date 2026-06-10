use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_cache::{CacheKey, RespCacheable};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use prometheus::{CounterVec, HistogramVec};

use crate::config::schema::AppConfig;
use crate::filter::rate_limit::RateLimiter;
#[cfg(feature = "redis")]
use crate::filter::rate_limit_redis::RedisRateLimiter;
use crate::proxy::ctx::RequestCtx;
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::{logging_phase, request_phase, response_phase};
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

// ── ProxyHttp trait implementation ────────────────────────────────────────────
//
// Each trait method is a thin delegator into a phase module:
// `request_phase` (request filtering, routing, upstream selection, retries),
// `response_phase` (upstream response processing, cacheability), and
// `logging_phase` (access log + metrics).  The method bodies live there.

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
        request_phase::request_filter(self, session, ctx).await
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>>
    where
        Self::CTX: Send + Sync,
    {
        request_phase::upstream_peer(self, ctx).await
    }

    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()>
    where
        Self::CTX: Send + Sync,
    {
        request_phase::request_body_filter(self, body, ctx).await
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
        request_phase::upstream_request_filter(self, session, upstream_request, ctx).await
    }

    async fn upstream_response_filter(
        &self,
        session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        response_phase::upstream_response_filter(self, session, upstream_response, ctx).await
    }

    fn upstream_response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>> {
        response_phase::upstream_response_body_filter(body, end_of_stream, ctx)
    }

    async fn response_filter(
        &self,
        session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        response_phase::response_filter(session, ctx).await
    }

    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        request_phase::request_cache_filter(self, session, ctx)
    }

    fn should_serve_stale(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
        error: Option<&pingora_core::Error>,
    ) -> bool {
        request_phase::should_serve_stale(ctx, error)
    }

    fn cache_key_callback(&self, session: &Session, ctx: &mut Self::CTX) -> Result<CacheKey>
    where
        Self::CTX: Send + Sync,
    {
        request_phase::cache_key_callback(self, session, ctx)
    }

    fn response_cache_filter(
        &self,
        _session: &Session,
        resp: &ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<RespCacheable>
    where
        Self::CTX: Send + Sync,
    {
        response_phase::response_cache_filter(resp, ctx)
    }

    fn fail_to_connect(
        &self,
        session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        e: Box<pingora_core::Error>,
    ) -> Box<pingora_core::Error> {
        request_phase::fail_to_connect(self, session, ctx, e)
    }

    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<pingora_core::Error>,
        ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<pingora_core::Error> {
        request_phase::error_while_proxy(self, peer, session, e, ctx, client_reused)
    }

    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        logging_phase::logging(self, session, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::Ordering;

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
}
