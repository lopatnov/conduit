use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::config::schema::{
    CacheConfig, ConnectionPoolConfig, HeaderTransformConfig, ProxyTimeout, RewriteRule,
    StaticOptions, UpstreamTlsConfig as UpstreamTlsCfg,
};

#[derive(Debug)]
pub struct RequestCtx {
    pub site_idx: usize,
    pub upstream: UpstreamTarget,
    pub start_time: Instant,
    pub accept_enc: AcceptEncoding,
    /// Populated when the matched route has a `retry` configuration.
    pub retry: Option<RetryState>,
    /// CORS + security headers to inject into every response for this request.
    /// Computed once in `request_filter` and reused for all write paths.
    pub extra_headers: Vec<(String, String)>,
    /// Per-route proxy connection timeouts (from `proxy.*.timeout`).
    pub proxy_timeout: Option<ProxyTimeout>,
    /// Per-route connection pool settings (from `proxy.*.pool`).
    pub proxy_pool: Option<ConnectionPoolConfig>,
    /// When `true`, negotiate HTTP/2 with the upstream (ALPN H2H1).
    /// Derived from `proxy.*.http2: true` in the route config.
    pub proxy_http2: bool,
    /// The upstream URL that was selected for this request.
    ///
    /// `Some` for every proxied request (not just `least-conn`/circuit-breaker
    /// routes) so that Peak EWMA, Outlier Detection, per-peer response stats,
    /// and the per-upstream Prometheus gauges can attribute this request no
    /// matter which load-balancing strategy picked it. Whether this request
    /// also holds a `conn_count` slot that must be released is tracked
    /// separately by [`upstream_conn_slot`](Self::upstream_conn_slot) — the
    /// two must not be conflated, since `conn_count` is keyed by URL alone and
    /// a phantom decrement from an attribution-only request would corrupt the
    /// slot count for a *different* route sharing the same upstream.
    pub proxy_upstream_url: Option<String>,
    /// `true` when routing acquired a `conn_count` slot for
    /// `proxy_upstream_url` (via `conn_inc` / least-conn selection) that this
    /// request is responsible for releasing via `conn_dec`.
    ///
    /// `false` when `proxy_upstream_url` is populated for passive-health
    /// attribution only (no slot was acquired) — e.g. any non-least-conn
    /// route with no `maxConnectionsPerUpstream` configured. Reset to `false`
    /// whenever `proxy_upstream_url` is replaced without a matching
    /// `conn_inc` (see `record_failed_upstream_for_retry` /
    /// `upstream_peer`'s retry-restore path).
    pub upstream_conn_slot: bool,
    /// Cache configuration for this route (`proxy.*.cache`), if caching is enabled.
    ///
    /// `None` means the route has no cache config and caching is disabled for
    /// this request.
    pub proxy_cache_cfg: Option<CacheConfig>,
    /// Set to `true` by `upstream_response_filter` when the upstream returns a
    /// 5xx status and the site has `maskErrors: true`.  The
    /// `upstream_response_body_filter` hook replaces the body with a generic
    /// JSON error so internal stack traces don't leak to clients.
    pub mask_upstream_body: bool,
    /// Static header transform applied to every upstream response.
    /// Populated from `SiteConfig.response_transform`.
    pub response_transform: Option<HeaderTransformConfig>,
    /// JWT claims extracted by `JwtGuard` — available for template substitution
    /// in `requestTransform.setHeaders` values using `{{ jwt.<claim> }}` syntax.
    ///
    /// Only populated when `jwtAuth` is configured and a valid token is present.
    pub jwt_claims: Option<std::collections::HashMap<String, serde_json::Value>>,
    /// Active OpenTelemetry span for this request.
    ///
    /// Created at the start of `do_request_filter` and ended in `logging()`.
    /// Only populated when the `otlp` feature is enabled AND `global.otlp` is
    /// configured.  Otherwise `None` (zero overhead).
    #[cfg(feature = "otlp")]
    pub otel_span: Option<opentelemetry::global::BoxedSpan>,
    /// Buffered request body chunks for retry replay (linkerd ReplayBody pattern).
    ///
    /// Populated incrementally by `request_body_filter` when the route has
    /// `retry` configured.  Cloning `Bytes` is cheap (reference-counted), so
    /// accumulation cost is minimal.  Empty when buffering is not needed.
    pub body_buffer: Vec<bytes::Bytes>,
    /// `true` when the accumulated body exceeded `limits.maxBodyBufferBytes`
    /// (default 1 MiB).  Retries are still attempted but without body replay —
    /// only safe for idempotent methods (GET/HEAD) in that case.
    pub body_too_large: bool,
    /// Running tally of actual body bytes received so far.
    ///
    /// Incremented in `request_body_filter` for every chunk regardless of
    /// whether retry buffering is active.  Used to enforce `limits.maxBodyBytes`
    /// against clients that omit `Content-Length` or use chunked encoding.
    pub actual_body_bytes: u64,
    /// Timestamp recorded at the start of `upstream_request_filter` — i.e. the
    /// moment the proxied request was forwarded to the upstream.
    ///
    /// Used to compute `upstream_response_time`: the duration between sending
    /// the request and receiving the first byte of the upstream response.
    /// `None` for local handlers (health, static, metrics, …).
    pub upstream_start: Option<Instant>,

    /// RAII guard that releases the per-IP connection slot when this context
    /// is dropped at the end of `logging()`.  `None` when
    /// `limits.maxConnectionsPerIp` is not configured or the request was
    /// rejected before a slot was acquired.
    pub ip_conn_slot: Option<crate::filter::chain::IpConnSlotGuard>,

    /// Passive health check: HTTP status codes that count as upstream failures.
    ///
    /// Populated from `healthCheck.unhealthyStatus` during routing.
    /// If the response status matches, `consecutive_5xx` is incremented.
    /// Default (empty) falls back to the standard 5xx-only detection.
    pub passive_unhealthy_status: Vec<u16>,

    /// Passive health check: latency threshold in milliseconds.
    ///
    /// Populated from `healthCheck.unhealthyLatencyMs` during routing.
    /// If the upstream response time exceeds this, it counts as a failure.
    pub passive_unhealthy_latency_ms: Option<u64>,
    /// Whether this route explicitly allows WebSocket upgrades.
    ///
    /// Set from `proxy.*.websocket: true` in the route config.  When `false`
    /// (the default), any `101 Switching Protocols` response from upstream is
    /// rejected with `502 Bad Gateway` to prevent unexpected protocol tunnelling.
    pub websocket_allowed: bool,
    /// Failed upstream attempts that need EWMA/health tracking after a retry.
    ///
    /// When `RetryOnErrorFilter` fires `RetryUpstream`, the current upstream's
    /// URL and status are pushed here before clearing `proxy_upstream_url` —
    /// the actual latency/ejection recording for each failed attempt happens
    /// inline, at the point of failure, in `record_failed_upstream_for_retry`.
    ///
    /// Currently write-only: nothing reads this field outside its own tests.
    /// It exists for potential future use (e.g. surfacing retry history in
    /// structured access logs) — see issue #218 for the decision on whether
    /// to wire it up or remove it.
    pub failed_upstream_attempts: Vec<(String, u16)>,
    /// Age in seconds to inject as the `Age` response header for cache hits.
    ///
    /// Computed in `upstream_response_filter` from the cached response's `Date`
    /// header (RFC 7234 §5.1): `age = now − date`.  `None` for non-cached
    /// responses or when the cache feature is disabled.
    pub cache_age_secs: Option<u64>,
    /// Sticky-session cookie to set on the response when HMAC signing is enabled.
    ///
    /// Populated during routing when `sticky.secret` is configured.
    /// Format: `(cookie_name, hmac_signed_value)`.  The `upstream_response_filter`
    /// injects the corresponding `Set-Cookie` header.
    pub sticky_set_cookie: Option<(String, String)>,
    /// Slow-loris upload defense: accumulated excess bytes for the leaky-bucket
    /// rate checker in `request_body_filter`.
    ///
    /// Positive excess means the client is sending faster than `minUploadRate`
    /// would allow; negative means the client has headroom.  Set to 0.0 on init.
    pub upload_excess_bytes: f64,
    /// Timestamp of the last body chunk received, used by the upload-rate checker.
    pub upload_last_chunk: Option<std::time::Instant>,
    /// Upstream URL to refresh in the background after this cache-hit response
    /// is served (early refresh, #31).
    ///
    /// Set by `response_filter` when the cache entry's remaining TTL is within
    /// `earlyRefreshSecs`.  `logging()` spawns a fire-and-forget GET task.
    /// `None` when early refresh is not configured or the TTL is not yet close.
    #[cfg(feature = "cache")]
    pub early_refresh_upstream_url: Option<String>,
}

impl RequestCtx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        site_idx: usize,
        upstream: UpstreamTarget,
        retry: Option<RetryState>,
        proxy_timeout: Option<ProxyTimeout>,
        proxy_pool: Option<ConnectionPoolConfig>,
        proxy_http2: bool,
        proxy_upstream_url: Option<String>,
        proxy_cache_cfg: Option<CacheConfig>,
        response_transform: Option<HeaderTransformConfig>,
    ) -> Self {
        Self {
            site_idx,
            upstream,
            start_time: Instant::now(),
            accept_enc: AcceptEncoding::default(),
            retry,
            extra_headers: Vec::new(),
            proxy_timeout,
            proxy_pool,
            proxy_http2,
            proxy_upstream_url,
            upstream_conn_slot: false,
            proxy_cache_cfg,
            mask_upstream_body: false,
            response_transform,
            body_buffer: Vec::new(),
            body_too_large: false,
            actual_body_bytes: 0,
            jwt_claims: None,
            upstream_start: None,
            ip_conn_slot: None,
            passive_unhealthy_status: Vec::new(),
            passive_unhealthy_latency_ms: None,
            websocket_allowed: false,
            failed_upstream_attempts: Vec::new(),
            cache_age_secs: None,
            sticky_set_cookie: None,
            upload_excess_bytes: 0.0,
            upload_last_chunk: None,
            #[cfg(feature = "cache")]
            early_refresh_upstream_url: None,
            #[cfg(feature = "otlp")]
            otel_span: None,
        }
    }
}

/// Per-request retry state for proxy routes that have `retry` configured.
///
/// The URL list is rotated so that `urls[0]` is the round-robin starting
/// target for this particular request.  Subsequent retries advance through
/// `urls[1 % len]`, `urls[2 % len]`, etc.
#[derive(Debug)]
pub struct RetryState {
    /// All target URLs for the route, rotated to start at the RR position.
    pub urls: Vec<String>,
    /// Number of times `upstream_peer()` has been called so far (0 = first call).
    pub attempt: usize,
    /// Total attempts allowed including the initial one (e.g. `attempts: 3` ⇒ 3 tries).
    pub max_attempts: usize,
    /// Error conditions that should trigger a retry.
    /// Valid values: `"connection_error"` | `"5xx"` | `"timeout"`.
    pub conditions: Vec<String>,
    /// Optional delay between retries in milliseconds.
    pub backoff_ms: Option<u64>,
    /// When `true`, jitter ±50% is applied to `backoff_ms` to avoid retry storms.
    pub backoff_jitter: bool,
    /// Maximum percentage of in-flight requests that may be retries (0.0–100.0).
    ///
    /// Prevents retry storms: when many requests fail simultaneously, an
    /// unconstrained retry budget multiplies load by `1 + attempts`.
    /// `None` means unlimited retries are allowed (legacy behaviour).
    pub budget_percent: Option<f64>,
    /// Set to `true` once this request has been promoted to a retry.
    ///
    /// The `logging()` hook reads this flag to decrement `AppState.retry_inflight`
    /// after the retry response is delivered.
    pub is_retrying: bool,
}

impl RetryState {
    /// Returns `true` when there are retries left (i.e. we have not yet exhausted
    /// `max_attempts`).  Call this *after* `attempt` has been incremented by
    /// `upstream_peer()`.
    pub fn has_attempts_left(&self) -> bool {
        self.attempt < self.max_attempts
    }

    pub fn has_condition(&self, cond: &str) -> bool {
        self.conditions.iter().any(|c| c == cond)
    }
}

#[derive(Debug)]
pub enum UpstreamTarget {
    Local(LocalHandler),
    Proxy {
        /// "host:port" string passed to Pingora's HttpPeer::new.
        addr: String,
        tls: bool,
        sni: String,
        strip_prefix: Option<String>,
        /// Path rewrite rules applied before forwarding — first matching rule wins.
        rewrite: Option<Vec<RewriteRule>>,
        /// Optional traffic mirror URL.  When `Some`, `upstream_request_filter`
        /// fires a fire-and-forget copy of the request to this backend.
        mirror_url: Option<String>,
        /// Per-route upstream TLS settings (cert verification, custom SNI).
        upstream_tls: Option<UpstreamTlsCfg>,
    },
    Upload {
        addr: SocketAddr,
    },
}

#[derive(Debug, Clone)]
pub enum LocalHandler {
    Health,
    Fallback,
    Metrics {
        token: Option<String>,
    },
    StaticFile {
        roots: Vec<PathBuf>,
        options: Arc<StaticOptions>,
        strip_prefix: Option<String>,
    },
    /// HTTP-01 ACME challenge response — served at
    /// `/.well-known/acme-challenge/{token}`.
    AcmeChallenge {
        token: String,
    },
    /// Server-Sent Events stream at `/__hot-reload__`.
    /// Clients subscribe and receive a `data: reload` event on file change.
    HotReloadSse,
    /// Client-side JavaScript served at `/__hot-reload__/client.js`.
    /// Connects to the SSE stream and reloads the page on events.
    HotReloadJs,
    /// All upstream connections for this route are at the configured
    /// `maxConnectionsPerUpstream` limit — circuit open.
    ///
    /// The handler returns `503 Service Unavailable` immediately without
    /// forwarding the request to any upstream.
    Overloaded,
}

// Layer-0 vocabulary (#114/#126).
pub use conduit_core::util::encoding::AcceptEncoding;

#[cfg(test)]
mod tests {
    use super::*;

    // ── AcceptEncoding::parse moved to conduit_core::util::encoding ────────────

    // ── RetryState ────────────────────────────────────────────────────────────

    fn make_retry(attempt: usize, max: usize, conditions: &[&str]) -> RetryState {
        RetryState {
            urls: vec!["http://a:4000".to_string()],
            attempt,
            max_attempts: max,
            conditions: conditions.iter().map(|s| s.to_string()).collect(),
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None,
            is_retrying: false,
        }
    }

    #[test]
    fn has_attempts_left_when_under_max() {
        assert!(make_retry(0, 3, &[]).has_attempts_left());
        assert!(make_retry(2, 3, &[]).has_attempts_left());
    }

    #[test]
    fn no_attempts_left_when_at_max() {
        assert!(!make_retry(3, 3, &[]).has_attempts_left());
        assert!(!make_retry(5, 3, &[]).has_attempts_left());
    }

    #[test]
    fn has_condition_matches_exact_string() {
        let rs = make_retry(0, 3, &["5xx", "connection_error"]);
        assert!(rs.has_condition("5xx"));
        assert!(rs.has_condition("connection_error"));
        assert!(!rs.has_condition("timeout"));
    }

    #[test]
    fn has_condition_empty_list_never_matches() {
        let rs = make_retry(0, 3, &[]);
        assert!(!rs.has_condition("5xx"));
    }
}
