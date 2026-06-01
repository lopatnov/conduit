use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

// ── Constants ──────────────────────────────────────────────────────────────

pub const CONFIG_VERSION: u32 = 1;

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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OtlpConfig {
    /// OTLP gRPC endpoint.  Examples:
    /// - `"http://localhost:4317"` (local collector)
    /// - `"https://api.honeycomb.io:443"` (Honeycomb)
    pub endpoint: String,
    /// Service name reported in traces.  Defaults to `"conduit"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Fraction of traces to export (0.0 = none, 1.0 = all).
    /// Defaults to `1.0` (100 %).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
    /// Export timeout in milliseconds.  Defaults to `5000`.
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

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
    /// Each consumer has its own credentials (API key or Basic Auth) and
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AcmeConfig {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Http2Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_streams: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_window_size: Option<u32>,
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
}

// ── Compression ────────────────────────────────────────────────────────────

/// `false` | `true` | `{ "algorithms": ["br", "gzip"], "level": 6, "minBytes": 1024 }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CompressionConfig {
    Enabled(bool),
    Options(CompressionOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompressionOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithms: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_bytes: Option<u64>,
}

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

/// `false` | `true` | `{ "hstsMaxAgeSecs": 31536000, ... }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SecurityHeadersConfig {
    Enabled(bool),
    Options(SecurityHeadersOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecurityHeadersOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hsts_max_age_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_frame_options: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer_policy: Option<String>,
}

// ── CORS ───────────────────────────────────────────────────────────────────

/// `false` | `true` | `{ "origins": [...], "methods": [...] }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CorsConfig {
    Enabled(bool),
    Options(CorsOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CorsOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origins: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub methods: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_headers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_age_secs: Option<u64>,
}

// ── Hot reload ─────────────────────────────────────────────────────────────

/// `false` | `true` | `{ "extensions": [".html", ".css"] }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum HotReloadConfig {
    Enabled(bool),
    Options(HotReloadOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HotReloadOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitConfig {
    pub window_secs: u64,
    pub limit: u64,
    /// Optional burst capacity on top of `limit`.
    ///
    /// The token bucket starts with `limit + burst` tokens and refills at
    /// `limit / windowSecs` per second.  This allows short traffic spikes up to
    /// `limit + burst` requests without being rate-limited, while the sustained
    /// throughput is still capped at `limit / windowSecs` requests per second.
    ///
    /// Example: `limit: 60, windowSecs: 60, burst: 20` → allows up to 80 requests
    /// in a burst, sustained at 1 req/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub burst: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    /// Backend store for the rate limiter.
    ///
    /// - `"memory"` (default) — in-process `DashMap<String, TokenBucket>`.
    /// - `"redis://host:port"` — Redis-backed, with automatic failover to the
    ///   in-memory bucket when Redis is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

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

/// Named-consumer authentication: credentials and per-consumer policies stored
/// per-consumer rather than per-route.
///
/// When a request matches a consumer's credentials:
/// 1. The consumer's username is injected as `X-Consumer-ID` (or `idHeader`)
///    into the upstream request.
/// 2. Any per-consumer `headers` are also injected.
/// 3. Per-consumer `rateLimit` is applied (independent of the site rate limit).
///
/// Requests that don't match any consumer receive 401 Unauthorized.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsumersConfig {
    /// The list of named consumers (evaluated in order; first match wins).
    #[serde(default)]
    pub consumers: Vec<Consumer>,
    /// Header name used to inject the consumer's username into the upstream
    /// request.  Defaults to `"x-consumer-id"`.
    #[serde(rename = "idHeader", skip_serializing_if = "Option::is_none")]
    pub id_header: Option<String>,
    /// Header name used to read the API key from the request.
    /// Defaults to `"x-api-key"`.  Only relevant for consumers that use
    /// `apiKey` credentials.
    #[serde(rename = "apiKeyHeader", skip_serializing_if = "Option::is_none")]
    pub api_key_header: Option<String>,
    /// Paths that bypass consumers authentication entirely.
    /// Same glob syntax as `basicAuth.skipPaths`.
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    /// Shared JWT configuration for V3 consumer identification.
    ///
    /// When set, the Bearer token is validated once against the shared
    /// JWKS / secret, and the consumer is identified by matching the
    /// configured `usernameClaim` (default: `"sub"`) against
    /// `consumer.username`.
    ///
    /// This is the canonical Auth0 / Cognito / Keycloak pattern: the identity
    /// provider issues tokens with `sub = user-id`, and consumers are the list
    /// of allowed user IDs with per-user policies.
    ///
    /// Checked **before** per-consumer credentials (api_key / basicAuth / jwt).
    ///
    /// ```yaml
    /// consumers:
    ///   sharedJwt:
    ///     jwksUrl: "https://auth0.example.com/.well-known/jwks.json"
    ///     audience: ["my-api"]
    ///     issuer:   "https://auth0.example.com"
    ///   consumers:
    ///     - username: user-abc   # identified when jwt.sub == "user-abc"
    /// ```
    #[serde(rename = "sharedJwt", skip_serializing_if = "Option::is_none")]
    pub shared_jwt: Option<ConsumersSharedJwtConfig>,
}

/// Shared JWT configuration for V3 consumer identification.
///
/// All consumers in the list share one JWKS endpoint (or HS256 secret).
/// After token validation the `usernameClaim` value is matched against
/// `consumer.username` to determine which consumer made the request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsumersSharedJwtConfig {
    /// Remote JWKS URL for RS256 / ES256 tokens.  Mutually exclusive with `secret`.
    #[serde(rename = "jwksUrl", skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    /// HS256 shared secret.  Mutually exclusive with `jwks_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Expected `aud` claim values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    /// Expected `iss` claim value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// JWT claim whose value is matched against `consumer.username`.
    ///
    /// Defaults to `"sub"` (the standard subject claim).  Use a different
    /// claim name when the identity provider stores the user identifier
    /// in a non-standard field (e.g., `"email"`, `"preferred_username"`).
    #[serde(rename = "usernameClaim", skip_serializing_if = "Option::is_none")]
    pub username_claim: Option<String>,
}

/// A single named API consumer — a client with its own credentials and limits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Consumer {
    /// Unique name injected as `X-Consumer-ID` after identification.
    pub username: String,
    /// API key credential.  The consumer is identified when the request
    /// carries this value in the `apiKeyHeader` (default: `x-api-key`).
    #[serde(rename = "apiKey", skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// HTTP Basic Auth credential.  The consumer is identified when the
    /// request carries `Authorization: Basic <base64(username:password)>` where
    /// the username matches `Consumer.username` and the password matches this.
    #[serde(rename = "basicAuth", skip_serializing_if = "Option::is_none")]
    pub basic_auth: Option<ConsumerBasicAuth>,
    /// JWT bearer-token credential (V2).
    ///
    /// The consumer is identified when the request carries a valid
    /// `Authorization: Bearer <token>` whose signature and claims are accepted
    /// by the configured secret / JWKS endpoint.
    ///
    /// Unlike `jwtAuth` at site level, this credential is checked
    /// independently inside `ConsumersGuard` — no separate `jwtAuth` block is
    /// required.
    ///
    /// ```yaml
    /// - username: service-a
    ///   jwt:
    ///     secret: "$SERVICE_A_SECRET"
    ///     issuer:  "https://auth.example.com"
    /// ```
    #[serde(rename = "jwt", skip_serializing_if = "Option::is_none")]
    pub jwt: Option<ConsumerJwtConfig>,
    /// Per-consumer rate limit, evaluated after identification.
    /// Independent of the site-level `rateLimit`.
    /// Key: `"consumer:{username}"` (global across all IPs for this consumer).
    #[serde(rename = "rateLimit", skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitConfig>,
    /// Additional request headers to inject into the upstream request for this
    /// consumer (e.g., `X-Tier: premium`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
}

/// Basic Auth password for a `Consumer`.  The username comes from
/// `Consumer.username`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerBasicAuth {
    pub password: String,
}

/// JWT credential for a `Consumer`.
///
/// A simplified subset of [`JwtAuthConfig`] without `skip_paths` or
/// `jwks_refresh_secs` — those concerns belong to the site-level JWT guard,
/// not to the per-consumer credential.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerJwtConfig {
    /// HMAC-SHA256 secret for HS256 tokens.  Mutually exclusive with `jwks_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Remote JWKS URL for RS256 / ES256 tokens.  Mutually exclusive with `secret`.
    #[serde(rename = "jwksUrl", skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    /// Expected `aud` claim values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    /// Expected `iss` claim value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
}

// ── JWT auth ───────────────────────────────────────────────────────────────

/// JWT bearer-token validation configuration.
///
/// At least one of `secret` or `jwks_url` must be present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct JwtAuthConfig {
    /// HMAC-SHA256 secret for HS256-signed tokens.  Mutually exclusive with
    /// `jwks_url`.  Stored as a plain string (use `$ENV_VAR` for security).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Remote JWKS URL for RS256 / ES256 tokens (e.g. Auth0, Google, Cognito).
    /// Keys are fetched at startup and refreshed every `jwksRefreshSecs` seconds.
    #[serde(rename = "jwksUrl", skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    /// How often to re-fetch the JWKS (seconds).  Default: 3600 (1 hour).
    #[serde(rename = "jwksRefreshSecs", skip_serializing_if = "Option::is_none")]
    pub jwks_refresh_secs: Option<u64>,
    /// Expected `aud` claim.  When set, tokens with a different audience are
    /// rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    /// Expected `iss` claim.  When set, tokens from a different issuer are
    /// rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// Paths that bypass JWT validation (same glob syntax as `basicAuth.skipPaths`).
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

// ── Forward Auth ──────────────────────────────────────────────────────────

/// External authentication service integration.
///
/// The request is forwarded to the auth URL before reaching the upstream.
/// The auth service communicates its decision via HTTP status:
/// - 2xx → allow; copy `responseHeaders` to upstream request
/// - 4xx / 5xx → deny; return the auth service's status to the client
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ForwardAuthConfig {
    /// URL of the authentication/authorization service.
    pub url: String,
    /// Request headers to forward to the auth service.
    ///
    /// When absent or empty, only `X-Forwarded-For`, `X-Forwarded-Method`,
    /// and `X-Forwarded-Uri` are sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Vec<String>>,
    /// Auth service response headers to inject into the upstream request.
    ///
    /// For example: `["X-User-ID", "X-Role"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Vec<String>>,
    /// Maximum time to wait for the auth service in milliseconds.
    /// Default: 5000 ms.
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Paths that bypass forward-auth entirely (same glob syntax as `skipPaths`).
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct IpFilterConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
    /// When `true`, read the client IP from `X-Forwarded-For` instead of the
    /// TCP connection address.  Only enable when Conduit is behind a trusted
    /// reverse proxy that sets this header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_proxy: Option<bool>,
}

// ── Request limits ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct LimitsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_body_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_header_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Maximum concurrent requests to this site.  When the number of in-flight
    /// requests reaches this limit, new requests receive `503 Service Unavailable`
    /// immediately rather than queuing.  Defaults to unlimited.
    #[serde(
        rename = "maxInflightRequests",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_inflight_requests: Option<u64>,
    /// Maximum request body size to buffer for retry replay (bytes).
    ///
    /// When a route has `retry` configured, Conduit buffers the request body
    /// up to this limit so it can be replayed on a retry attempt.  If the
    /// body exceeds this limit, retries are still attempted but without body
    /// replay (safe only for GET/HEAD which have no body in practice).
    ///
    /// Defaults to 1 MiB (1_048_576 bytes).  Set to 0 to disable buffering.
    #[serde(rename = "maxBodyBufferBytes", skip_serializing_if = "Option::is_none")]
    pub max_body_buffer_bytes: Option<u64>,
}

// ── Redirects ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedirectRule {
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

// ── Middleware chain ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareEntry {
    // `type` is a Rust keyword; r# prefix lets us use it as an identifier
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    // Only used when type = "script" (Phase 4 Rhai)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

// ── Static files ───────────────────────────────────────────────────────────

/// `"./dist"` | `["./a", "./b"]` | `{ "/": "./dist", "/docs": "./docs-dist" }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StaticConfig {
    Single(String),
    Multi(Vec<String>),
    Mapped(IndexMap<String, String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StaticOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<bool>,
    /// Duration string parsed with humantime: "1d", "30m", "1h"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_age: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<Vec<String>>,
    /// "ignore" | "allow" | "deny"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dot_files: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_compressed: Option<bool>,
}

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
}

/// Configuration for cookie-based sticky sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StickyConfig {
    /// Name of the cookie to use as the session affinity key.
    pub cookie: String,
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
    /// an unhealthy state, its effective weight is increased linearly from 0 to
    /// 100 % over this window.  Set to 0 (default) to disable.
    #[serde(rename = "slowStartSecs", skip_serializing_if = "Option::is_none")]
    pub slow_start_secs: Option<u64>,
    /// Maximum number of concurrent in-flight requests to any single upstream
    /// in this route's target pool.
    ///
    /// When ALL healthy upstreams reach this limit Conduit returns
    /// `503 Service Unavailable` immediately (circuit breaker / back-pressure).
    /// A single upstream at max while others are below capacity is handled
    /// normally by the load-balancer (requests go to under-capacity peers).
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
}

// ── Cache ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheConfig {
    /// "memory" | "redis://..." | "disk:./cache"
    pub store: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    /// Serve stale content while revalidating in the background (RFC 5861).
    ///
    /// When set, a cache entry that has expired within the stale window is
    /// returned immediately to the client while Pingora fetches a fresh copy
    /// asynchronously.  The next request after revalidation completes gets
    /// the fresh copy.  Zero perceived latency for the overwhelming majority
    /// of requests.
    ///
    /// ```yaml
    /// cache:
    ///   store: memory
    ///   ttlSecs: 60
    ///   staleWhileRevalidateSecs: 300  # serve stale for up to 5 min after TTL expires
    /// ```
    #[serde(
        rename = "staleWhileRevalidateSecs",
        skip_serializing_if = "Option::is_none"
    )]
    pub stale_while_revalidate_secs: Option<u32>,
    /// Serve stale content when the upstream returns an error (RFC 5861
    /// `stale-if-error`).
    ///
    /// When upstream is unreachable or returns 5xx, Conduit serves the most
    /// recently cached copy for up to `staleIfErrorSecs` seconds instead of
    /// forwarding the error to the client.
    #[serde(rename = "staleIfErrorSecs", skip_serializing_if = "Option::is_none")]
    pub stale_if_error_secs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vary_headers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_if_cookie: Option<bool>,
    /// Default: ["GET", "HEAD"]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub methods: Option<Vec<String>>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UploadConfig {
    pub path: String,
    pub dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_file_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_mime_types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_name: Option<String>,
}

// ── Metrics ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

// ── Fallback ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FallbackConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// Content-negotiated fallback rules keyed by Accept type ("html", "json", "*")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_accept: Option<IndexMap<String, FallbackRule>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FallbackRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
}

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
}

/// Fault injection configuration — inject artificial errors or delays into
/// a percentage of requests.  Useful for chaos engineering and testing
/// circuit-breaker / retry behaviour without a real failing upstream.
///
/// ```json
/// {
///   "faultInjection": {
///     "abort": { "percent": 5,  "status": 503 },
///     "delay": { "percent": 10, "ms": 200 }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FaultInjectionConfig {
    /// Abort a percentage of requests with the given HTTP status code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort: Option<FaultAbort>,
    /// Add an artificial delay to a percentage of requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<FaultDelay>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FaultAbort {
    /// Percentage of requests to abort (0–100).
    pub percent: f64,
    /// HTTP status code to return (default: 503).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Response body text (default: "Fault injected").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FaultDelay {
    /// Percentage of requests to delay (0–100).
    pub percent: f64,
    /// Delay in milliseconds.
    pub ms: u64,
}

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
