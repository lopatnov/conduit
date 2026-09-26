//! `SiteConfig` — one virtual host / listener and every feature block it can carry.
//!
//! Field order is the JSON/YAML key order on serialisation (`Serialize` is derived) and
//! `extra` must stay last; do not reorder.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::{
    ApiKeyConfig, BasicAuthConfig, CompressionConfig, ConsumersConfig, CorsConfig, FallbackConfig,
    FaultInjectionConfig, ForwardAuthConfig, HeaderTransformConfig, HealthCheckConfig,
    HotReloadConfig, Http2Config, IpFilterConfig, JwtAuthConfig, LimitsConfig, LoggingConfig,
    MetricsConfig, MiddlewareEntry, OutlierDetectionConfig, ProxyConfig, RateLimitConfig,
    RedirectRule, ResponseTimeConfig, RouteConfig, SecurityHeadersConfig, StaticConfig,
    StaticOptions, TcpConfig, TlsConfig, UploadConfig,
};

// ── Site config ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SiteConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http2: Option<Http2Config>,

    // Bool | string | object shorthand fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<CompressionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_time: Option<ResponseTimeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_headers: Option<SecurityHeadersConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cors: Option<CorsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hot_reload: Option<HotReloadConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheckConfig>,

    // Object-only fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basic_auth: Option<BasicAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKeyConfig>,
    /// Named-consumer authentication model.
    ///
    /// Each consumer has its own credentials (API key, Basic Auth, or
    /// per-consumer JWT — or none, when identified via `sharedJwt`) and
    /// per-consumer policies (rate limit, upstream header injection).
    /// After identification the consumer's username is injected as
    /// `X-Consumer-ID` into the upstream request.
    ///
    /// ```yaml
    /// consumers:
    ///   consumers:
    ///     - username: alice
    ///       apiKey: "key-alice"
    ///       rateLimit: { windowSecs: 60, limit: 100 }
    ///     - username: bob
    ///       basicAuth: { password: "hunter2" }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumers: Option<ConsumersConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_filter: Option<IpFilterConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<LimitsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirects: Option<Vec<RedirectRule>>,
    // Phase 2.x config, Phase 4 Rhai execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub middleware: Option<Vec<MiddlewareEntry>>,

    // "static" is a Rust keyword — use rename to map JSON key → Rust field
    #[serde(rename = "static", skip_serializing_if = "Option::is_none")]
    pub static_files: Option<StaticConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub static_options: Option<StaticOptions>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
    /// Raw TCP proxy — forward bytes to an upstream TCP address without HTTP parsing.
    ///
    /// Useful for non-HTTP protocols: MySQL, PostgreSQL, Redis, SMTP, etc.
    /// Cannot be combined with `proxy`, `static`, or other HTTP features on
    /// the same site.  Set `port` at the site level.
    ///
    /// ```yaml
    /// sites:
    ///   - port: 3306
    ///     tcp:
    ///       targets: ["mysql-primary:3306", "mysql-replica:3306"]
    ///       strategy: round-robin   # optional; default: round-robin
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp: Option<TcpConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<MetricsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackConfig>,
    /// Phase 3.6: advanced per-site routing rules.  Routes are matched in
    /// declaration order; the first match wins.  When present, routes are
    /// evaluated before the top-level `proxy` / `static` shorthand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<RouteConfig>>,
    /// Passive health checking via outlier detection.
    ///
    /// Tracks consecutive 5xx responses from real traffic (not health probes)
    /// and temporarily ejects misbehaving upstreams from the pool.
    ///
    /// ```json
    /// { "outlierDetection": { "consecutive5xx": 5, "baseEjectionTimeSecs": 30 } }
    /// ```
    #[serde(rename = "outlierDetection", skip_serializing_if = "Option::is_none")]
    pub outlier_detection: Option<OutlierDetectionConfig>,
    /// Replace upstream 5xx response bodies with a generic JSON error.
    ///
    /// Prevents internal stack traces and service details from leaking to
    /// clients.  Set to `false` in development environments where you need to
    /// see the real upstream error body.
    ///
    /// ```json
    /// { "maskErrors": true }
    /// ```
    #[serde(rename = "maskErrors", skip_serializing_if = "Option::is_none")]
    pub mask_errors: Option<bool>,
    /// Allow upstream responses with duplicate `Transfer-Encoding: chunked`
    /// headers to pass through unmodified.
    ///
    /// By default (`false`), Conduit deduplicates repeated `chunked` directives
    /// in upstream `Transfer-Encoding` headers — some misconfigured origins emit
    /// `Transfer-Encoding: chunked, chunked` or two separate `Transfer-Encoding`
    /// headers, which confuses strict HTTP clients.
    ///
    /// Set to `true` only for upstreams that deliberately rely on duplicate
    /// chunked headers.
    ///
    /// ```json
    /// { "allowDuplicateChunked": true }
    /// ```
    #[serde(
        rename = "allowDuplicateChunked",
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_duplicate_chunked: Option<bool>,
    /// Emit W3C `Server-Timing` response header for this site.
    ///
    /// When `true`, every proxied response carries a `Server-Timing` header
    /// with two entries:
    ///
    /// - `total;dur=<ms>` — time from request received to upstream response headers
    /// - `upstream;dur=<ms>` — time the upstream took to return response headers
    ///
    /// The header is visible in browser DevTools → Network → Timing panel.
    /// Cached responses include only `total` (no upstream round-trip).
    ///
    /// ```json
    /// { "serverTiming": true }
    /// ```
    #[serde(rename = "serverTiming", skip_serializing_if = "Option::is_none")]
    pub server_timing: Option<bool>,
    /// Fault injection for testing — inject artificial errors or delays.
    /// Should NOT be enabled in production.
    #[serde(rename = "faultInjection", skip_serializing_if = "Option::is_none")]
    pub fault_injection: Option<FaultInjectionConfig>,
    /// JWT bearer-token authentication.
    ///
    /// Validates the `Authorization: Bearer <token>` header on every request
    /// (unless the path is in `skipPaths`).  The token is verified against a
    /// static secret (HS256) or a remote JWKS endpoint (RS256/ES256).
    ///
    /// ```json
    /// { "jwtAuth": { "jwksUrl": "https://accounts.example.com/.well-known/jwks.json",
    ///                "audience": ["my-app"], "issuer": "https://accounts.example.com" } }
    /// ```
    #[serde(rename = "jwtAuth", skip_serializing_if = "Option::is_none")]
    pub jwt_auth: Option<JwtAuthConfig>,
    /// Forward Auth — delegate authentication to an external HTTP service.
    ///
    /// Every request is forwarded to the auth service before reaching the upstream.
    /// If the auth service returns 2xx, the request proceeds and any configured
    /// `responseHeaders` from the auth response are injected into the upstream
    /// request (e.g. `X-User-ID`, `X-Role`).
    /// If the auth service returns 4xx or 5xx, that status is returned directly
    /// to the client.
    ///
    /// ```json
    /// { "forwardAuth": { "url": "http://auth:9000/verify",
    ///                    "requestHeaders": ["Authorization", "Cookie"],
    ///                    "responseHeaders": ["X-User-ID", "X-Role"] } }
    /// ```
    #[serde(rename = "forwardAuth", skip_serializing_if = "Option::is_none")]
    pub forward_auth: Option<ForwardAuthConfig>,
    /// Static header injection / removal applied to every upstream request.
    ///
    /// ```json
    /// { "requestTransform": { "setHeaders": { "X-Service": "my-api" },
    ///                         "removeHeaders": ["X-Internal-Token"] } }
    /// ```
    #[serde(rename = "requestTransform", skip_serializing_if = "Option::is_none")]
    pub request_transform: Option<HeaderTransformConfig>,
    /// Static header injection / removal applied to every upstream response.
    ///
    /// ```json
    /// { "responseTransform": { "setHeaders": { "X-Served-By": "conduit" },
    ///                          "removeHeaders": ["X-Powered-By"] } }
    /// ```
    #[serde(rename = "responseTransform", skip_serializing_if = "Option::is_none")]
    pub response_transform: Option<HeaderTransformConfig>,
    // Phase 5 (optional): pub cgi: Option<CgiConfig>,
    /// Catches any top-level JSON/YAML key that doesn't match a named field
    /// above — either a typo, or (once schema fields become `#[cfg]`-gated
    /// per feature during the Conduit 2.0 workspace migration, #114) a key
    /// belonging to a feature this binary wasn't compiled with. Never
    /// populated by well-formed configs against the current, always-present
    /// field set; see `validate::feature_warnings()` for how it's surfaced.
    #[serde(flatten)]
    pub extra: IndexMap<String, serde_json::Value>,
}
