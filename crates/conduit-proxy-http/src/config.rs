//! Proxy routing config types (issue #114/#143), extracted from the root
//! crate's `src/config/schema.rs`. The root crate keeps a facade re-export
//! at each type's original location/section, so no existing call site or
//! test changes.
//!
//! - [`ProxyConfig`]/[`ProxyRouteTarget`]/[`ProxyRouteConfig`] — the `proxy`
//!   shorthand/map (`site.proxy`).
//! - [`StickyConfig`] — cookie-based sticky sessions.
//! - [`RewriteRule`] — path rewrite rules.
//! - [`ProxyTimeout`]/[`ConnectionPoolConfig`]/[`RetryConfig`] — per-route
//!   connection/retry settings.
//! - [`RouteConfig`]/[`MatchConfig`] — the `routes[]` array (Phase 3.6).

use std::fmt;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use conduit_cache::CacheConfig;
use conduit_config_core::redact::Redacted;
use conduit_ratelimit::RateLimitConfig;
use conduit_static::StaticConfig;
use conduit_upstream::{
    LoadBalanceStrategy, ProxyTarget, UpstreamGroup, UpstreamHealthCheck, UpstreamTlsConfig,
};

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
#[derive(Clone, Serialize, Deserialize, PartialEq)]
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

impl fmt::Debug for StickyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StickyConfig")
            .field("cookie", &self.cookie)
            .field("secret", &self.secret.as_ref().map(|_| Redacted))
            .field("strict", &self.strict)
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticky_config_secret_is_redacted() {
        let cfg = StickyConfig {
            cookie: "route".to_string(),
            secret: Some("hmac-signing-secret".to_string()),
            strict: Some(true),
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("hmac-signing-secret"), "got: {debug}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("route"),
            "non-secret field must still print: {debug}"
        );
    }

    #[test]
    fn sticky_config_absent_secret_shows_none() {
        let cfg = StickyConfig {
            cookie: "route".to_string(),
            secret: None,
            strict: None,
        };
        let debug = format!("{cfg:?}");
        assert!(
            debug.contains("None"),
            "absent secret must still distinguish None from Some: {debug}"
        );
        assert!(!debug.contains("[REDACTED]"), "got: {debug}");
    }
}
