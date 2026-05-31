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
    // Reserved for future service-discovery providers (Consul, etcd, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdminConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
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

// ── IP filter ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct IpFilterConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
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
}

/// Configuration for cookie-based sticky sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StickyConfig {
    /// Name of the cookie to use as the session affinity key.
    pub cookie: String,
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
