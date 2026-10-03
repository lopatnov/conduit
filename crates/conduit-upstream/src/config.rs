//! Load-balancing strategy and upstream health-check configuration types.
//!
//! Moved out of the root crate's `src/config/schema.rs` (issue #114/#142) —
//! see this crate's `src/lib.rs` doc comment for the overall split rationale.
//! These types are compiled into **every** conduit build, like every other
//! extracted config-always-on crate (`conduit-faults`, `conduit-otlp`, ...) —
//! `ProxyRouteConfig.healthCheck`/`strategy`/`groups`/`upstreamTls` aren't
//! themselves feature-gated, so a config using them must stay parseable
//! regardless of feature selection. This crate does have one `proxy` Cargo
//! feature (issue #144), but it gates only the `reqwest`-backed connection
//! warmup in `health` — upstream selection/health tracking and every type in
//! this module are always compiled, since `SiteConfig` embeds them — see
//! `src/lib.rs`.

use serde::{Deserialize, Serialize};

/// `#[serde(rename_all = "kebab-case")]` load-balancing strategy selector.
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

/// A named group of upstream targets with its own balancing strategy.
/// Used together with `ProxyRouteConfig.groups` + `group_strategy` (both
/// still root-crate-only types — see `src/lib.rs`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamGroup {
    pub name: String,
    pub targets: Vec<ProxyTarget>,
    /// Intra-group strategy. Defaults to `round-robin`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<LoadBalanceStrategy>,
}

/// Upstream TLS configuration (used with `https://` proxy targets).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamTlsConfig {
    /// Whether to verify the upstream certificate against the system CA store.
    ///
    /// Defaults to `true` (Pingora's default). Set to `false` for internal
    /// services that use self-signed certificates. **Only disable in
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
