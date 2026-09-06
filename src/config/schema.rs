use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

// ── Constants ──────────────────────────────────────────────────────────────

pub use conduit_config_core::parse::CONFIG_VERSION;

// ── Top-level entry point ──────────────────────────────────────────────────

/// Serde tries variants top-to-bottom. Order is critical — do not reorder.
/// Single is a catch-all because all SiteConfig fields are Option.
/// SiteConfig is large (~1.7 KiB), so Single is boxed to keep the enum small.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ConfigFile {
    Full(AppConfig),         // { "global": {...}, "sites": [...] }
    Sites(Vec<SiteConfig>),  // [{...}, {...}]
    Single(Box<SiteConfig>), // { "port": 8080, ... }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<GlobalConfig>,
    pub sites: Vec<SiteConfig>,
}

// ── Global config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlog: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<AdminConfig>,
    /// OpenTelemetry OTLP tracing configuration.
    ///
    /// When set, Conduit exports distributed traces to the configured OTLP
    /// endpoint (Grafana Tempo, Jaeger, Honeycomb, OpenTelemetry Collector).
    /// Requires `--features otlp` at compile time.
    ///
    /// ```json
    /// { "global": { "otlp": { "endpoint": "http://otel-collector:4317",
    ///                         "serviceName": "conduit-gateway" } } }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otlp: Option<OtlpConfig>,
    // Reserved for future service-discovery providers (Consul, etcd, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<serde_json::Value>,
}

/// OpenTelemetry OTLP exporter configuration.
///
/// Requires `--features otlp`.  When the `otlp` feature is disabled the
/// config field is still accepted (parsed without error) but silently ignored.
///
/// Extracted into `crates/conduit-otlp` (issue #114/#129) — this is a facade
/// re-export so `crate::config::schema::OtlpConfig` keeps resolving to the
/// same type at the same location for every existing call site/test.
pub use conduit_otlp::OtlpConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdminConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    /// Optional token for Admin API authentication.
    ///
    /// When set, every Admin API request must include
    /// `Authorization: Bearer <token>`.  Requests without the correct
    /// token receive `401 Unauthorized`.
    ///
    /// Useful in cloud/Kubernetes environments where the admin API is
    /// exposed beyond loopback (not recommended — prefer loopback + VPN).
    ///
    /// ```json
    /// { "admin": { "bind": "0.0.0.0:2019", "token": "$ADMIN_TOKEN" } }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

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

// ── TLS ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_redirect_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ciphers: Option<Vec<String>>,
    // Auto-TLS via Let's Encrypt (Phase 3.1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acme: Option<AcmeConfig>,
    /// Mutual TLS — require and verify client certificates.
    ///
    /// When set, every TLS connection must present a certificate signed by
    /// the configured CA.  Clients without a valid certificate are rejected
    /// at the TLS handshake (before any HTTP processing).
    ///
    /// ```yaml
    /// tls:
    ///   cert: ./server.crt
    ///   key:  ./server.key
    ///   clientAuth:
    ///     ca: ./ca.crt       # PEM file containing the CA that signs client certs
    ///     optional: false    # true = request cert but don't require it
    /// ```
    #[serde(rename = "clientAuth", skip_serializing_if = "Option::is_none")]
    pub client_auth: Option<TlsClientAuth>,
}

/// mTLS client certificate verification configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TlsClientAuth {
    /// Path to the CA certificate file (PEM format) used to verify client certs.
    pub ca: String,
    /// When `true`, client certificates are requested but not required
    /// (equivalent to nginx `ssl_verify_client optional`).
    /// When `false` (default), clients without a valid cert are rejected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

// Extracted into crates/conduit-acme (#114/#130) — always compiled (like
// `conduit_otlp::OtlpConfig`) so `tls.acme` stays parseable in every build.
pub use conduit_acme::AcmeConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Http2Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_streams: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_window_size: Option<u32>,
    /// Allow HTTP/2 upgrade on plaintext (cleartext) connections — h2c.
    ///
    /// When `true`, a client connecting on a plain HTTP port can negotiate
    /// HTTP/2 without TLS.  Useful for internal gRPC traffic or when TLS is
    /// handled by an upstream load-balancer.
    ///
    /// **Does not affect TLS ports** — those always negotiate HTTP/2 via ALPN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h2c: Option<bool>,
}

// ── Logging ────────────────────────────────────────────────────────────────

/// `false` | `true` | `"dev"` | `{ "format": "json", "file": "..." }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LoggingConfig {
    Enabled(bool),
    Format(LogFormat),
    Options(LoggingOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Combined,
    Common,
    Dev,
    Short,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct LoggingOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<LogFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Paths to exclude from access logs.
    ///
    /// Requests whose path matches any entry are silently skipped — useful to
    /// suppress noisy health-check and metrics traffic from access logs.
    /// Supports exact paths and `/**` glob suffixes.
    ///
    /// ```json
    /// { "format": "json", "skipPaths": ["/__health__", "/__metrics__"] }
    /// ```
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    /// Strip the query string from logged request paths.
    ///
    /// When `true`, the access log records only the path component (e.g.
    /// `/api/login`) rather than the full `path?query` string.  This prevents
    /// API tokens or session IDs passed as query parameters from appearing in
    /// plaintext log files.
    ///
    /// Default: `false` (query string is logged, matching standard access-log
    /// behaviour).
    ///
    /// ```json
    /// { "format": "json", "stripQuery": true }
    /// ```
    #[serde(rename = "stripQuery", skip_serializing_if = "Option::is_none")]
    pub strip_query: Option<bool>,
}

// ── Compression ────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-compression` (issue #114/#138) — this is a
/// facade re-export so `crate::config::schema::{CompressionConfig,
/// CompressionOptions}` keep resolving to the same types at the same
/// location for every existing call site/test.
///
/// `false` | `true` | `{ "algorithms": ["br", "gzip"], "level": 6, "minBytes": 1024 }`
pub use conduit_compression::{CompressionConfig, CompressionOptions};

// ── Response time ──────────────────────────────────────────────────────────

/// `false` | `true` | `{ "digits": 3 }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ResponseTimeConfig {
    Enabled(bool),
    Options(ResponseTimeOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResponseTimeOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digits: Option<u8>,
}

// ── Security headers ───────────────────────────────────────────────────────

/// Extracted into `crates/conduit-security-headers` (issue #114/#136) — this
/// is a facade re-export so `crate::config::schema::SecurityHeadersConfig`
/// keeps resolving to the same type at the same location for every existing
/// call site/test.
pub use conduit_security_headers::SecurityHeadersConfig;
/// Extracted into `crates/conduit-security-headers` (issue #114/#136) — see
/// the [`SecurityHeadersConfig`] re-export above.
pub use conduit_security_headers::SecurityHeadersOptions;

// ── CORS ───────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-cors` (issue #114/#136) — this is a facade
/// re-export so `crate::config::schema::CorsConfig` keeps resolving to the
/// same type at the same location for every existing call site/test.
pub use conduit_cors::CorsConfig;
/// Extracted into `crates/conduit-cors` (issue #114/#136) — see the
/// [`CorsConfig`] re-export above.
pub use conduit_cors::CorsOptions;

// ── Hot reload ─────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-hotreload` (issue #114/#140) — this is a
/// facade re-export so `crate::config::schema::{HotReloadConfig,
/// HotReloadOptions}` keep resolving to the same types at the same location
/// for every existing call site/test.
pub use conduit_hotreload::HotReloadConfig;
/// Extracted into `crates/conduit-hotreload` (issue #114/#140) — see the
/// [`HotReloadConfig`] re-export above.
pub use conduit_hotreload::HotReloadOptions;

// ── Health check (site-level endpoint) ────────────────────────────────────

/// `false` | `true` | `{ "path": "/__health__", "includeUpstreams": true }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum HealthCheckConfig {
    Enabled(bool),
    Options(HealthCheckOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_upstreams: Option<bool>,
}

// ── Auth & rate limiting ───────────────────────────────────────────────────

/// Rate-limit config — moved to `crates/conduit-ratelimit` (issue #114/#137,
/// slice 1), re-exported here so every existing `crate::config::schema::
/// RateLimitConfig` path keeps resolving. Shared, byte-identical shape with
/// `conduit_auth_consumers::RateLimitConfig` — as of #137 slice 1 they're the
/// *same* type, not just field-compatible duplicates (issue #114/#134's
/// SonarCloud duplication finding is resolved by this re-export).
pub use conduit_ratelimit::RateLimitConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BasicAuthConfig {
    pub users: IndexMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyConfig {
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

// ── Consumer model ─────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-auth-consumers` (issue #114/#134) — this
/// is a facade re-export so `crate::config::schema::{ConsumersConfig,
/// ConsumersSharedJwtConfig, Consumer, ConsumerBasicAuth, ConsumerJwtConfig}`
/// keep resolving to the same types at the same location for every existing
/// call site/test.
///
/// Named-consumer authentication: credentials and per-consumer policies
/// stored per-consumer rather than per-route. When a request matches a
/// consumer's credentials:
/// 1. The consumer's username is injected as `X-Consumer-ID` (or `idHeader`)
///    into the upstream request.
/// 2. Any per-consumer `headers` are also injected.
/// 3. Per-consumer `rateLimit` is applied (independent of the site rate limit).
///
/// Requests that don't match any consumer receive 401 Unauthorized.
///
/// **Note:** unlike every sibling facade in this file, `ConsumersGuard`
/// itself is *not* re-exported here — it stays in this crate's own
/// `src/filter/chain.rs` (see `conduit_auth_consumers`'s own `src/lib.rs`
/// doc comment for why: `ConsumersGuard` is a `Session`-coupled request-chain
/// guard, same category as `IpGuard`/`CorsPreflight` staying out of their
/// own Layer-0 crates — chain assembly and guard ordering stay in the root
/// crate per `CLAUDE.md` decision #20, regardless of where the *types* it
/// carries live. As of #114/#137 slice 1, `RateLimiter` itself now lives in
/// `conduit-ratelimit`, re-exported via `crate::filter::rate_limit`). Only
/// the config types and the pure `identify::identify_consumer`
/// identification logic moved to `conduit-auth-consumers`.
pub use conduit_auth_consumers::{
    Consumer, ConsumerBasicAuth, ConsumerJwtConfig, ConsumersConfig, ConsumersSharedJwtConfig,
};

// ── JWT auth ───────────────────────────────────────────────────────────────

/// JWT bearer-token validation configuration.
///
/// At least one of `secret` or `jwks_url` must be present.
///
/// Extracted into `crates/conduit-auth-jwt` (issue #114/#133) — this is a
/// facade re-export so `crate::config::schema::JwtAuthConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_auth_jwt::JwtAuthConfig;

// ── Forward Auth ──────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-auth-forward` (issue #114/#134) — this is
/// a facade re-export so `crate::config::schema::ForwardAuthConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
///
/// External authentication service integration. The request is forwarded to
/// the auth URL before reaching the upstream. The auth service communicates
/// its decision via HTTP status:
/// - 2xx → allow; copy `responseHeaders` to upstream request
/// - 4xx / 5xx → deny; return the auth service's status to the client
pub use conduit_auth_forward::ForwardAuthConfig;

// ── Header transform ───────────────────────────────────────────────────────

/// Static header injection / removal for requests or responses.
///
/// Applied unconditionally to every request (request transform) or every
/// upstream response (response transform) for the site.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HeaderTransformConfig {
    /// Headers to add or overwrite.  The value is the literal string to set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_headers: Option<IndexMap<String, String>>,
    /// Header names to remove.  Case-insensitive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_headers: Option<Vec<String>>,
}

// ── IP filter ──────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-ipfilter` (issue #114/#136) — this is a
/// facade re-export so `crate::config::schema::IpFilterConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_ipfilter::IpFilterConfig;

// ── Request limits ─────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-limits` (issue #114/#137) — this is a
/// facade re-export so `crate::config::schema::LimitsConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_limits::LimitsConfig;

// ── Redirects ──────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-redirects` (issue #114/#140) — this is a
/// facade re-export so `crate::config::schema::RedirectRule` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_redirects::RedirectRule;

// ── Middleware chain ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareEntry {
    // `type` is a Rust keyword; r# prefix lets us use it as an identifier
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// File path to the script / WASM module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Pipeline phase to run this entry in.
    ///
    /// - `"request"` (default) — run during the request phase (before upstream)
    /// - `"response"` — run during the response phase (after upstream responds)
    ///
    /// WASM plugins do not need this field: if the module exports `on_response`,
    /// it is called automatically in both phases (request AND response).
    /// For Rhai scripts, set `phase: "response"` to run on the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

// ── Static files ───────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-static` (issue #114/#139) — this is a
/// facade re-export so `crate::config::schema::{StaticConfig,
/// StaticOptions}` keep resolving to the same types at the same location
/// for every existing call site/test.
///
/// `"./dist"` | `["./a", "./b"]` | `{ "/": "./dist", "/docs": "./docs-dist" }`
pub use conduit_static::{StaticConfig, StaticOptions};

// ── Proxy ──────────────────────────────────────────────────────────────────

/// `"http://upstream:4000"` | `{ "/api": ..., "/ws": ... }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ProxyConfig {
    Single(String),
    Routes(IndexMap<String, ProxyRouteTarget>),
}

/// Serde tries variants top-to-bottom.
/// Url → simple string, RoundRobin → string array, Full → object with targets.
/// ProxyRouteConfig is large, so Full is boxed to keep the enum compact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ProxyRouteTarget {
    Url(String),
    RoundRobin(Vec<String>),
    Full(Box<ProxyRouteConfig>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRouteConfig {
    #[serde(default)]
    pub targets: Vec<ProxyTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<LoadBalanceStrategy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http2: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip_prefix: Option<bool>,
    /// Used with ip-hash / consistent-hash: "ip" | "header:X-Key" | "url"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<ProxyTimeout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check: Option<UpstreamHealthCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<ConnectionPoolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
    /// Path rewrite rules applied in order before forwarding to upstream.
    /// Each rule is a regex `from` pattern and a replacement `to` string.
    /// Capture groups (`$1`, `$2`, …) are supported in `to`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<Vec<RewriteRule>>,
    /// Explicit backup (failover) upstream.  Used when all primary `targets`
    /// are unhealthy or when a primary returns a 5xx / connection error and
    /// the retry conditions include `"5xx"` / `"connection_error"`.
    ///
    /// ```json
    /// { "targets": ["http://primary:4000"], "backup": "http://fallback:4000" }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    /// Two-level load balancing: outer strategy picks a group, inner strategy
    /// picks within the group.  Mutually exclusive with `targets` — if
    /// `groups` is set, `targets` is ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<UpstreamGroup>>,
    /// Outer strategy used to pick which group services a request.
    /// Defaults to `round-robin` when `groups` is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_strategy: Option<LoadBalanceStrategy>,
    /// Sticky session via cookie.  When set, the named cookie value is used
    /// as the hash key — the same client always hits the same backend for the
    /// lifetime of the cookie.
    ///
    /// ```json
    /// { "sticky": { "cookie": "srv_id" } }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sticky: Option<StickyConfig>,
    /// Traffic mirror URL.  A fire-and-forget copy of every request is sent to
    /// this backend asynchronously — the mirror response is discarded and the
    /// client receives the primary response as normal.
    ///
    /// Useful for shadow-testing a new service version or capturing live traffic
    /// for analysis without affecting real users.
    ///
    /// **Note:** Only request headers and method/path are mirrored in V1.
    /// Request body mirroring requires body buffering and is deferred to V2.
    ///
    /// ```json
    /// { "targets": ["http://primary:4000"], "mirror": "http://shadow:4000" }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
    /// Upstream TLS settings — applied when targets use `https://` scheme.
    ///
    /// By default Pingora verifies the upstream certificate using the system
    /// CA store.  Use `verify: false` for internal services with self-signed
    /// certificates.
    ///
    /// ```json
    /// { "targets": ["https://backend:4443"],
    ///   "upstreamTls": { "verify": false } }
    /// ```
    #[serde(rename = "upstreamTls", skip_serializing_if = "Option::is_none")]
    pub upstream_tls: Option<UpstreamTlsConfig>,
    /// Per-route rate limiting.  Evaluated **after** the site-level `rateLimit`
    /// (both limits must pass for the request to proceed).
    ///
    /// The rate-limit key is prefixed with the route path so buckets do not
    /// clash across different routes on the same site.
    ///
    /// ```json
    /// { "targets": ["http://api:4000"],
    ///   "rateLimit": { "windowSecs": 60, "limit": 10, "keyBy": "ip" } }
    /// ```
    #[serde(rename = "rateLimit", skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitConfig>,
    /// Request priority for load shedding (0 = lowest, 100 = highest).
    ///
    /// When the site is under load and `limits.priorityThreshold` is set,
    /// requests below the shed threshold are rejected with `503 Load Shedding`
    /// while higher-priority routes continue to be served.
    ///
    /// The effective priority is the **maximum** of this field and the numeric
    /// value of the incoming `X-Priority` header (0–100).
    ///
    /// ```json
    /// { "targets": ["http://critical:4000"], "priority": 80 }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// Allow WebSocket upgrades on this route.
    ///
    /// When `false` (the default) and an upstream returns `101 Switching
    /// Protocols`, Conduit rejects the upgrade and returns `502 Bad Gateway`.
    /// This prevents unexpected protocol tunnelling through the proxy.
    ///
    /// Set to `true` explicitly for routes that proxy WebSocket connections.
    ///
    /// ```json
    /// { "targets": ["http://ws-backend:4000"], "websocket": true }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket: Option<bool>,
}

/// Configuration for cookie-based sticky sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StickyConfig {
    /// Name of the cookie to use as the session affinity key.
    pub cookie: String,
    /// HMAC-SHA256 secret for signing and verifying sticky-session cookies.
    ///
    /// When set, the cookie value is `HMAC-SHA256(upstream_url, secret)` encoded
    /// as URL-safe base64 (no padding).  On incoming requests the HMAC is verified
    /// against every healthy upstream; a forged or mismatched cookie falls through
    /// to normal load-balancing rather than pinning the client to an arbitrary peer.
    ///
    /// **Strongly recommended in production** — without a secret, clients can craft
    /// any cookie value to pin themselves to any upstream (session-pinning attack).
    ///
    /// Supports `$ENV_VAR` interpolation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// When `true`, return `503 Service Unavailable` if the hinted upstream is
    /// unhealthy or ejected, rather than falling back to another peer.
    ///
    /// Default: `false` (fall back to normal load-balancing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Upstream TLS configuration (used with `https://` proxy targets).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamTlsConfig {
    /// Whether to verify the upstream certificate against the system CA store.
    ///
    /// Defaults to `true` (Pingora's default).  Set to `false` for internal
    /// services that use self-signed certificates.  **Only disable in
    /// trusted internal networks** — disabling verification exposes you to
    /// man-in-the-middle attacks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
    /// Override the hostname used for certificate verification.
    ///
    /// When absent, the SNI hostname (derived from the target URL) is used.
    /// Useful when the upstream presents a certificate for a different hostname
    /// than its DNS name.
    #[serde(rename = "serverName", skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
}

/// A named group of upstream targets with its own balancing strategy.
/// Used together with `ProxyRouteConfig.groups` + `group_strategy`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamGroup {
    pub name: String,
    pub targets: Vec<ProxyTarget>,
    /// Intra-group strategy. Defaults to `round-robin`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<LoadBalanceStrategy>,
}

/// A single path rewrite rule: the first match in `rewrite` that matches the
/// request path is applied; subsequent rules are not checked.
///
/// ```json
/// { "from": "^/old/(.+)$", "to": "/new/$1" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RewriteRule {
    /// Regex pattern to match against the request path.
    pub from: String,
    /// Replacement string — capture groups `$1` … `$N` are expanded.
    pub to: String,
}

/// `"http://b1:4000"` | `{ "url": "http://b1:4000", "weight": 3 }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ProxyTarget {
    Simple(String),
    Weighted(WeightedTarget),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WeightedTarget {
    pub url: String,
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LoadBalanceStrategy {
    #[default]
    RoundRobin,
    WeightedRoundRobin,
    Random,
    LeastConn,
    LeastResponseTime,
    IpHash,
    ConsistentHash,
    /// Power of Two Choices: sample 2 random backends, pick the less-loaded one.
    /// O(1) selection; better latency distribution than LeastConn under high load.
    #[serde(rename = "p2c")]
    P2c,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTimeout {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_ms: Option<u64>,
    /// Per-attempt timeout in milliseconds.  When set, each retry attempt gets
    /// its own independent timeout rather than sharing the remaining total
    /// timeout.  Useful for capping latency on individual attempts while still
    /// allowing multiple retries.
    #[serde(rename = "perTryMs", skip_serializing_if = "Option::is_none")]
    pub per_try_ms: Option<u64>,
    /// Maximum time in milliseconds to wait for the upstream to send the first
    /// byte of the response after the request has been forwarded.
    ///
    /// Differs from `readMs` in intent: `firstByteMs` caps the upstream latency
    /// (how long until the server starts responding) while `readMs` caps
    /// individual read-call durations.  Maps to Pingora's `read_timeout` for
    /// the initial response window.  Useful for fail-fast behaviour when
    /// upstreams are slow to start responding.
    ///
    /// ```json
    /// { "timeout": { "firstByteMs": 500, "readMs": 30000 } }
    /// ```
    #[serde(rename = "firstByteMs", skip_serializing_if = "Option::is_none")]
    pub first_byte_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPoolConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_idle: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
}

/// Per-upstream health check for proxy routes (distinct from site-level HealthCheckConfig).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamHealthCheck {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unhealthy_threshold: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healthy_threshold: Option<u32>,
    /// Slow-start ramp-up window in seconds.  After an upstream recovers from
    /// an unhealthy state, its participation probability in each pick rises
    /// linearly from 0 to 100% over this window (issue #157).  Set to 0
    /// (default) to disable.
    ///
    /// Ignored for `ipHash`/`consistentHash` strategies and sticky sessions —
    /// see `docs/configuration.md`'s "Slow start" section for why (a
    /// config-validate-time warning is emitted when both are configured
    /// together on the same route).
    #[serde(rename = "slowStartSecs", skip_serializing_if = "Option::is_none")]
    pub slow_start_secs: Option<u64>,
    /// Maximum number of concurrent in-flight requests to any single upstream
    /// in this route's target pool.
    ///
    /// Enforced for every load-balance strategy, across the legacy `proxy: {}`
    /// map, the `routes[]` array, and `groups`: a request is only routed to an
    /// upstream currently below this cap. When ALL healthy upstreams for the
    /// route are at or above it, Conduit returns `503 Service Unavailable`
    /// immediately (circuit breaker / back-pressure). `IpHash`/`ConsistentHash`
    /// (and sticky sessions) forward-probe to the next ring position instead
    /// of shrinking the hash domain, so only clients whose preferred peer is
    /// currently saturated get relocated.
    ///
    /// This is a **soft** limit: the check-then-acquire isn't atomic, so a
    /// burst of concurrent requests can briefly overshoot it by the number of
    /// simultaneous racers — self-correcting on the next request, and the
    /// same trade-off as `retry.budgetPercent`'s soft enforcement.
    ///
    /// Defaults to unlimited (`None`).
    ///
    /// ```json
    /// { "targets": ["http://backend:4000"],
    ///   "healthCheck": { "maxConnectionsPerUpstream": 50 } }
    /// ```
    #[serde(
        rename = "maxConnectionsPerUpstream",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_connections_per_upstream: Option<u64>,
    /// Number of keepalive connections to pre-establish at server startup.
    ///
    /// Defaults to `0` (disabled).  Values above 8 are clamped to 8.
    #[serde(rename = "prewarmConnections", skip_serializing_if = "Option::is_none")]
    pub prewarm_connections: Option<u8>,

    /// HTTP status codes from real proxy traffic counted as upstream failures.
    ///
    /// **Passive health check** — Caddy `unhealthy_status` pattern.  Each time an
    /// upstream returns one of these status codes, `consecutive_5xx` is incremented
    /// (same counter used by `outlierDetection`).  After `consecutive5xx` failures
    /// the upstream is ejected.
    ///
    /// Default: `[500, 502, 503, 504]`.  Set `[]` to disable.
    ///
    /// ```json
    /// { "unhealthyStatus": [429, 500, 502, 503, 504] }
    /// ```
    #[serde(rename = "unhealthyStatus", skip_serializing_if = "Option::is_none")]
    pub unhealthy_status: Option<Vec<u16>>,

    /// Response latency threshold (ms) above which the request counts as a
    /// passive upstream failure.
    ///
    /// **Passive health check** — Caddy `unhealthy_latency` pattern.  When an
    /// upstream takes longer than this to return the first response byte,
    /// `consecutive_5xx` is incremented.  Use with `outlierDetection` to eject
    /// persistently slow backends.  Default: disabled (`None`).
    ///
    /// ```json
    /// { "unhealthyLatencyMs": 2000 }
    /// ```
    #[serde(rename = "unhealthyLatencyMs", skip_serializing_if = "Option::is_none")]
    pub unhealthy_latency_ms: Option<u64>,
}

// ── Cache ──────────────────────────────────────────────────────────────────

// Extracted into `crates/conduit-cache` (issue #114/#135) — re-exported here
// so `crate::config::schema::CacheConfig` keeps resolving to the same item
// at the same location for backward compatibility. See
// `conduit_cache::config::CacheConfig` for the implementation.
pub use conduit_cache::CacheConfig;

// ── Retry ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RetryConfig {
    pub attempts: u32,
    /// "connection_error" | "5xx" | "timeout"
    pub conditions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_ms: Option<u64>,
    /// When `true`, adds a random ±50 % jitter to `backoffMs` on each retry.
    ///
    /// Prevents retry storms: if many requests fail simultaneously, adding jitter
    /// distributes the retry attempts over time rather than creating a synchronized
    /// wave.  The effective delay is `backoffMs * [0.5, 1.5)`.
    ///
    /// Only meaningful when `backoffMs` is also set.
    #[serde(rename = "backoffJitter", skip_serializing_if = "Option::is_none")]
    pub backoff_jitter: Option<bool>,
    /// Retry budget: maximum percentage of active requests that may be retries.
    ///
    /// Prevents retry storms: when all requests fail simultaneously, without a
    /// budget each request might retry 3 times, multiplying load by 4x.
    /// With `budgetPercent: 20`, at most 20 % of active requests are retries.
    /// Defaults to unlimited (no budget enforced).
    #[serde(rename = "budgetPercent", skip_serializing_if = "Option::is_none")]
    pub budget_percent: Option<f64>,
}

// ── Upload ─────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-upload` (issue #114/#131) — this is a
/// facade re-export so `crate::config::schema::UploadConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_upload::UploadConfig;

// ── Metrics ────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-metrics` (issue #114/#140) — this is a
/// facade re-export so `crate::config::schema::MetricsConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_metrics::MetricsConfig;

// ── Fallback ───────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-static` (issue #114/#139) — this is a
/// facade re-export so `crate::config::schema::{FallbackConfig,
/// FallbackRule}` keep resolving to the same types at the same location for
/// every existing call site/test.
pub use conduit_static::{FallbackConfig, FallbackRule};

// ── Routes (Phase 3.6) ─────────────────────────────────────────────────────

/// A single named routing rule.
///
/// `match` describes when the rule applies; the first of `proxy` / `static`
/// that is set describes what to do.  Routes are evaluated in declaration
/// order before the top-level `proxy` / `static` shorthand.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RouteConfig {
    /// Match criteria (path glob, method, headers).
    pub r#match: MatchConfig,
    /// Proxy this request to an upstream when the match succeeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyRouteTarget>,
    /// Serve static files from this path when the match succeeds.
    #[serde(rename = "static", skip_serializing_if = "Option::is_none")]
    pub static_files: Option<StaticConfig>,
}

/// Criteria that must all be satisfied for a [`RouteConfig`] to fire.
///
/// All fields are optional; an absent field matches anything.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MatchConfig {
    /// Glob-style path pattern.
    ///
    /// `*` matches any character sequence within a single path segment.
    /// `**` matches any character sequence including `/` (i.e. any sub-path).
    ///
    /// Examples: `/api/**`, `/blog/*`, `/health`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// HTTP methods that must match (case-insensitive).
    ///
    /// Examples: `["GET"]`, `["POST", "PUT", "PATCH"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<Vec<String>>,
    /// Request header values that must be present and match (exact string or regex).
    ///
    /// All entries must match simultaneously.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// Query parameter values that must be present and match (exact string or regex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<IndexMap<String, String>>,
    /// Cookie values that must be present and match (exact string or regex).
    ///
    /// Reads the `Cookie` request header and matches named cookies against the
    /// given patterns.  All entries must match simultaneously.  Uses the same
    /// regex semantics as `headers` and `query`.
    ///
    /// Example — route canary users:
    /// ```yaml
    /// match:
    ///   cookies:
    ///     beta: "1"
    ///     experiment: "blue|green"
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookies: Option<IndexMap<String, String>>,
}

/// Extracted into `crates/conduit-faults` (issue #114/#132) — this is a
/// facade re-export so `crate::config::schema::{FaultInjectionConfig,
/// FaultAbort, FaultDelay}` keep resolving to the same types at the same
/// location for every existing call site/test.
///
/// ```json
/// {
///   "faultInjection": {
///     "abort": { "percent": 5,  "status": 503 },
///     "delay": { "percent": 10, "ms": 200 }
///   }
/// }
/// ```
pub use conduit_faults::{FaultAbort, FaultDelay, FaultInjectionConfig};

/// Passive health checking via Outlier Detection.
///
/// Ejects upstreams that return consecutive 5xx responses from real proxy
/// traffic.  Ejection duration grows exponentially with each ejection cycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OutlierDetectionConfig {
    /// Number of consecutive 5xx responses that trigger ejection (default: 5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consecutive_5xx: Option<u32>,
    /// Base ejection duration in seconds (default: 30).
    /// Actual duration = base × 2^ejection_count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ejection_time_secs: Option<u64>,
    /// Maximum ejection duration in seconds (default: 300 = 5 min).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ejection_time_secs: Option<u64>,
    /// Maximum fraction of upstreams that may be ejected simultaneously (0–100, default: 10).
    /// Prevents all upstreams from being ejected at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ejection_percent: Option<u8>,
}

// ── TCP proxy ──────────────────────────────────────────────────────────────

/// Raw TCP proxy configuration.
///
/// Proxies a raw TCP connection to one of the specified upstream addresses.
/// No HTTP parsing — bytes are forwarded as-is in both directions.
///
/// Extracted into `crates/conduit-tcp` (issue #114/#131) — this is a facade
/// re-export so `crate::config::schema::TcpConfig` keeps resolving to the
/// same type at the same location for every existing call site/test.
pub use conduit_tcp::TcpConfig;
