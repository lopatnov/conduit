use serde::{Deserialize, Serialize};

/// Rate-limit configuration — shared shape used at site level
/// (`sites[].rateLimit`), route level (`proxy.*.routes[].rateLimit`), and
/// consumer level (`consumers.consumers[].rateLimit`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A known field still deserializes fine — the positive control for
    /// `unknown_field_is_rejected` below (confirms the test fixture itself
    /// is a valid `RateLimitConfig`, not just that any input is rejected).
    #[test]
    fn known_fields_deserialize() {
        let json = r#"{"windowSecs": 60, "limit": 10, "dryRun": true}"#;
        let cfg: RateLimitConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.window_secs, 60);
        assert_eq!(cfg.limit, 10);
        assert_eq!(cfg.dry_run, Some(true));
    }

    /// Issue found on PR #152 review: without `deny_unknown_fields`, a typo'd
    /// or misplaced field (e.g. `dryRun` accidentally nested one level too
    /// deep, or a config author expecting a field this struct doesn't have)
    /// was silently accepted and ignored — the rate limit still enforced
    /// (429s) with no indication the extra field did nothing. Now it's a
    /// hard parse error instead of a silent no-op.
    #[test]
    fn unknown_field_is_rejected() {
        let json = r#"{"windowSecs": 60, "limit": 10, "unknownField": "oops"}"#;
        let err = serde_json::from_str::<RateLimitConfig>(json).unwrap_err();
        assert!(
            err.to_string().contains("unknownField") || err.to_string().contains("unknown field"),
            "expected an unknown-field error, got: {err}"
        );
    }
}
