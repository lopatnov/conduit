use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::config::schema::{
    HeaderTransformConfig, RewriteRule, StaticOptions, UpstreamTlsConfig as UpstreamTlsCfg,
};

// `RetryState`/`RouteRateLimit`/`ProxyReqState` moved into
// `src/proxy/routing/state.rs` (issue #143, PR A1 of a 3-PR plan) — a pure
// same-crate regrouping ahead of the eventual `conduit-proxy-http` crate
// extraction. Re-exported here so every existing `use crate::proxy::ctx::
// {RetryState, ...}` call site keeps resolving unchanged.
pub use crate::proxy::routing::state::{ProxyReqState, RetryState, RouteRateLimit};

#[derive(Debug)]
pub struct RequestCtx {
    pub site_idx: usize,
    pub upstream: UpstreamTarget,
    pub start_time: Instant,
    pub accept_enc: AcceptEncoding,
    /// Per-request proxy-routing state — retry, per-route timeout/pool/http2,
    /// upstream URL + conn-slot bookkeeping, cache cfg, passive-health
    /// thresholds, websocket/sticky/rate-limit/priority routing decisions.
    ///
    /// Grouped into [`ProxyReqState`] ahead of the eventual
    /// `conduit-proxy-http` crate extraction (issue #143) — see
    /// `src/proxy/routing/state.rs`.
    pub proxy: ProxyReqState,
    /// CORS + security headers to inject into every response for this request.
    /// Computed once in `request_filter` and reused for all write paths.
    pub extra_headers: Vec<(String, String)>,
    /// Set to `true` by `upstream_response_filter` when the upstream returns a
    /// 5xx status and the site has `maskErrors: true`.  The
    /// `upstream_response_body_filter` hook replaces the body with a generic
    /// JSON error so internal stack traces don't leak to clients.
    pub mask_upstream_body: bool,
    /// Static header transform applied to every upstream response.
    /// Populated from `SiteConfig.response_transform`.
    pub response_transform: Option<HeaderTransformConfig>,
    /// JWT claims extracted after the guard chain runs — available for
    /// template substitution in `requestTransform.setHeaders` values using
    /// `{{ jwt.<claim> }}` syntax.
    ///
    /// Only populated when `jwtAuth` is configured and a valid token is
    /// present. `#[cfg(feature = "jwt")]`-gated like `otel_span`/
    /// `early_refresh_upstream_url` below (`CLAUDE.md` decision #30) — use
    /// [`RequestCtx::jwt_claims`] to read this from always-compiled call
    /// sites (e.g. header-template expansion) without `#[cfg]`-branching at
    /// the call site itself.
    #[cfg(feature = "jwt")]
    pub jwt: Option<conduit_auth_jwt::guard::JwtReqState>,
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

    /// Per-request limits state — `actual_body_bytes`, `ip_conn_slot`
    /// (RAII per-IP connection slot, released on drop), and the slow-loris
    /// upload-rate leaky-bucket state (`upload_excess_bytes`/
    /// `upload_last_chunk`). Always present — unlike `jwt`/`cache` above,
    /// `limits` is not an optional Cargo feature (`CLAUDE.md` decision #31),
    /// so this field is never `Option`-wrapped or `#[cfg]`-gated. See
    /// [`conduit_limits::LimitsReqState`] / `crates/conduit-limits/src/ctx.rs`.
    pub limits: conduit_limits::LimitsReqState,

    /// Per-request cache state (`Age` header value, early-refresh upstream
    /// URL) — see [`CacheReqState`](conduit_cache::CacheReqState).
    ///
    /// `#[cfg(feature = "cache")]`-gated like `jwt`/`otel_span` above
    /// (`CLAUDE.md` decision #30) — use [`RequestCtx::cache_age_secs`] to
    /// read the `Age`-header value from always-compiled call sites (the
    /// `ResponseCtx` trait impl in `filter/response_chain.rs`) without
    /// `#[cfg]`-branching at the call site itself.
    #[cfg(feature = "cache")]
    pub cache: Option<conduit_cache::CacheReqState>,
}

impl RequestCtx {
    pub fn new(
        site_idx: usize,
        upstream: UpstreamTarget,
        proxy: ProxyReqState,
        response_transform: Option<HeaderTransformConfig>,
    ) -> Self {
        Self {
            site_idx,
            upstream,
            start_time: Instant::now(),
            accept_enc: AcceptEncoding::default(),
            proxy,
            extra_headers: Vec::new(),
            mask_upstream_body: false,
            response_transform,
            body_buffer: Vec::new(),
            body_too_large: false,
            #[cfg(feature = "jwt")]
            jwt: None,
            limits: conduit_limits::LimitsReqState::default(),
            #[cfg(feature = "cache")]
            cache: None,
            #[cfg(feature = "otlp")]
            otel_span: None,
        }
    }

    /// Borrow the JWT claims extracted for this request, if any.
    ///
    /// Used by the root crate's always-compiled `{{ jwt.<claim> }}`
    /// header-template expansion (`apply_header_transform_request_with_claims`
    /// in `request_phase.rs`, backed by `conduit_auth_jwt::template::
    /// expand_jwt_templates`) — that call site is unconditional (a config
    /// can reference the template syntax regardless of whether `--features
    /// jwt` is compiled in), so this accessor exists specifically to keep
    /// the `#[cfg]` branching contained here instead of at every call site.
    /// Returns a static empty reference when the `jwt` feature is off or no
    /// claims were extracted for this request.
    #[cfg(feature = "jwt")]
    pub fn jwt_claims(&self) -> &Option<std::collections::HashMap<String, serde_json::Value>> {
        const NO_CLAIMS: Option<std::collections::HashMap<String, serde_json::Value>> = None;
        self.jwt
            .as_ref()
            .map(|state| &state.claims)
            .unwrap_or(&NO_CLAIMS)
    }

    /// See the `#[cfg(feature = "jwt")]` overload above — stub for builds
    /// without the `jwt` feature, always returning `None` so
    /// `{{ jwt.<claim> }}` templates resolve to `""` (matching the
    /// documented always-compiled behavior of `expand_jwt_templates`).
    #[cfg(not(feature = "jwt"))]
    pub fn jwt_claims(&self) -> &Option<std::collections::HashMap<String, serde_json::Value>> {
        const NO_CLAIMS: Option<std::collections::HashMap<String, serde_json::Value>> = None;
        &NO_CLAIMS
    }

    /// Age in seconds for the `Age` response header on cache hits (RFC 7234
    /// §5.1).
    ///
    /// Backed by [`cache`](Self::cache)'s `CacheReqState` when the `cache`
    /// feature is compiled in; always `None` otherwise. Exists so the
    /// always-compiled `ResponseCtx` trait impl (`filter/response_chain.rs`)
    /// doesn't need `#[cfg]`-branching at its call site — matches the
    /// `jwt_claims()` pattern above (`CLAUDE.md` decision #30).
    #[cfg(feature = "cache")]
    pub fn cache_age_secs(&self) -> Option<u64> {
        self.cache.as_ref().and_then(|c| c.cache_age_secs)
    }

    /// See the `#[cfg(feature = "cache")]` overload above — stub for builds
    /// without the `cache` feature, always returning `None`.
    #[cfg(not(feature = "cache"))]
    pub fn cache_age_secs(&self) -> Option<u64> {
        None
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
