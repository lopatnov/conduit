use serde::{Deserialize, Serialize};

/// Rate-limit configuration — shared shape used at site level
/// (`sites[].rateLimit`), route level (`proxy.*.routes[].rateLimit`), and
/// consumer level (`consumers.consumers[].rateLimit`).
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
    /// - `"rediss://host:port"` — same as above, over TLS (for Redis deployments
    ///   that require in-transit encryption, e.g. AWS ElastiCache TLS, Azure
    ///   Cache for Redis).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
    /// Dry-run mode — log violations but allow requests through.
    ///
    /// **nginx `limit_req_dry_run` pattern.**  When `true`, requests that would
    /// normally be rejected with `429 Too Many Requests` are logged as warnings
    /// instead and forwarded to the upstream.  Useful for testing rate-limit
    /// configuration in production without impacting real traffic.
    ///
    /// Default: `false` (enforcement active).
    #[serde(rename = "dryRun", skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}
