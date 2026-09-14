use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Marker printed in place of a secret value in a manual `Debug` impl —
/// distinguishing "present" (`Some([REDACTED])`) from "absent" (`None`)
/// without ever printing the actual value (issue #354: every secret-bearing
/// config field used to derive plain `Debug`, so a `{:?}`-formatted print or
/// a panic message that happened to include one would leak it verbatim).
#[derive(Clone)]
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

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

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
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

impl fmt::Debug for AdminConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminConfig")
            .field("bind", &self.bind)
            .field("token", &self.token.as_ref().map(|_| Redacted))
            .finish()
    }
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

#[derive(Clone, Serialize, Deserialize, PartialEq)]
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

impl fmt::Debug for BasicAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Usernames (map keys) aren't secret; passwords (values) are —
        // redact only the values, keeping the user list itself visible.
        let users: IndexMap<&str, Redacted> =
            self.users.keys().map(|k| (k.as_str(), Redacted)).collect();
        f.debug_struct("BasicAuthConfig")
            .field("users", &users)
            .field("challenge", &self.challenge)
            .field("realm", &self.realm)
            .field("skip_paths", &self.skip_paths)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyConfig {
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

impl fmt::Debug for ApiKeyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyConfig")
            .field("keys", &vec![Redacted; self.keys.len()])
            .field("header", &self.header)
            .field("skip_paths", &self.skip_paths)
            .finish()
    }
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

/// Extracted into `crates/conduit-middleware` (issue #114/#141) — this is a
/// facade re-export so `crate::config::schema::MiddlewareEntry` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_middleware::MiddlewareEntry;

// ── Static files ───────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-static` (issue #114/#139) — this is a
/// facade re-export so `crate::config::schema::{StaticConfig,
/// StaticOptions}` keep resolving to the same types at the same location
/// for every existing call site/test.
///
/// `"./dist"` | `["./a", "./b"]` | `{ "/": "./dist", "/docs": "./docs-dist" }`
pub use conduit_static::{StaticConfig, StaticOptions};

// ── Proxy ──────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::{ProxyConfig,
/// ProxyRouteTarget, ProxyRouteConfig}` keep resolving to the same types at
/// the same location for every existing call site/test.
///
/// `"http://upstream:4000"` | `{ "/api": ..., "/ws": ... }`
pub use conduit_proxy_http::config::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget};

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::StickyConfig` keeps resolving
/// to the same type at the same location for every existing call site/test.
/// Configuration for cookie-based sticky sessions.
pub use conduit_proxy_http::config::StickyConfig;

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::{UpstreamTlsConfig,
/// UpstreamGroup}` keep resolving to the same types at the same location for
/// every existing call site/test. `UpstreamGroup` is used together with
/// `ProxyRouteConfig.groups` + `group_strategy` — both now live in
/// `crates/conduit-proxy-http` (issue #143), which resolves the circular
/// dependency this note used to describe.
pub use conduit_upstream::{UpstreamGroup, UpstreamTlsConfig};

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::RewriteRule` keeps resolving
/// to the same type at the same location for every existing call site/test.
/// A single path rewrite rule: the first match in `rewrite` that matches the
/// request path is applied; subsequent rules are not checked.
///
/// ```json
/// { "from": "^/old/(.+)$", "to": "/new/$1" }
/// ```
pub use conduit_proxy_http::config::RewriteRule;

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::{ProxyTarget, WeightedTarget,
/// LoadBalanceStrategy}` keep resolving to the same types at the same
/// location for every existing call site/test.
///
/// `"http://b1:4000"` | `{ "url": "http://b1:4000", "weight": 3 }`
pub use conduit_upstream::{LoadBalanceStrategy, ProxyTarget, WeightedTarget};

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::ProxyTimeout` keeps resolving
/// to the same type at the same location for every existing call site/test.
pub use conduit_proxy_http::config::ProxyTimeout;

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::ConnectionPoolConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_proxy_http::config::ConnectionPoolConfig;

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::UpstreamHealthCheck` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. Per-upstream health check for proxy routes (distinct from
/// site-level `HealthCheckConfig`).
pub use conduit_upstream::UpstreamHealthCheck;

// ── Cache ──────────────────────────────────────────────────────────────────

// Extracted into `crates/conduit-cache` (issue #114/#135) — re-exported here
// so `crate::config::schema::CacheConfig` keeps resolving to the same item
// at the same location for backward compatibility. See
// `conduit_cache::config::CacheConfig` for the implementation.
pub use conduit_cache::CacheConfig;

// ── Retry ──────────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::RetryConfig` keeps resolving
/// to the same type at the same location for every existing call site/test.
pub use conduit_proxy_http::config::RetryConfig;

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

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::{RouteConfig, MatchConfig}`
/// keep resolving to the same types at the same location for every existing
/// call site/test. A single named routing rule (`RouteConfig`) plus the
/// match criteria that decide when it fires (`MatchConfig`) — see
/// `conduit_proxy_http::config`'s own doc comments for the field-level
/// detail.
pub use conduit_proxy_http::config::{MatchConfig, RouteConfig};

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

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::OutlierDetectionConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. Passive health checking via Outlier Detection: ejects
/// upstreams that return consecutive 5xx responses from real proxy traffic,
/// with exponentially growing ejection durations.
pub use conduit_upstream::OutlierDetectionConfig;

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

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn admin_config_token_is_redacted() {
        let cfg = AdminConfig {
            bind: Some("0.0.0.0:2019".to_string()),
            token: Some("super-secret-admin-token".to_string()),
        };
        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("super-secret-admin-token"),
            "token leaked into Debug output: {debug}"
        );
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("0.0.0.0:2019"),
            "non-secret field must still print: {debug}"
        );
    }

    #[test]
    fn admin_config_absent_token_shows_none() {
        let cfg = AdminConfig {
            bind: None,
            token: None,
        };
        let debug = format!("{cfg:?}");
        assert!(
            debug.contains("None"),
            "absent secret must still distinguish None from Some: {debug}"
        );
        assert!(!debug.contains("[REDACTED]"), "got: {debug}");
    }

    // `sticky_config_secret_is_redacted` moved to
    // `crates/conduit-proxy-http/src/config.rs` along with `StickyConfig`
    // itself (issue #114/#143).

    #[test]
    fn basic_auth_config_passwords_are_redacted_but_usernames_are_not() {
        let mut users = IndexMap::new();
        users.insert("alice".to_string(), "alices-plaintext-password".to_string());
        let cfg = BasicAuthConfig {
            users,
            challenge: None,
            realm: None,
            skip_paths: None,
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("alices-plaintext-password"), "got: {debug}");
        assert!(
            debug.contains("alice"),
            "usernames aren't secret and should still print: {debug}"
        );
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
    }

    #[test]
    fn api_key_config_keys_are_redacted() {
        let cfg = ApiKeyConfig {
            keys: vec!["key-one".to_string(), "key-two".to_string()],
            header: Some("x-api-key".to_string()),
            skip_paths: None,
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("key-one"), "got: {debug}");
        assert!(!debug.contains("key-two"), "got: {debug}");
        assert!(
            debug.contains("x-api-key"),
            "non-secret field must still print: {debug}"
        );
    }
}
