//! Upstream selection and health: groups, upstream TLS, load-balance strategy, targets,
//! active health checks and outlier detection.

// ── Upstreams ──────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::{UpstreamTlsConfig,
/// UpstreamGroup}` keep resolving to the same types at the same location for
/// every existing call site/test. `UpstreamGroup` is used together with
/// `ProxyRouteConfig.groups` + `group_strategy` — both now live in
/// `crates/conduit-proxy-http` (issue #143), which resolves the circular
/// dependency this note used to describe.
pub use conduit_upstream::{UpstreamGroup, UpstreamTlsConfig};

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::{ProxyTarget, WeightedTarget,
/// LoadBalanceStrategy}` keep resolving to the same types at the same
/// location for every existing call site/test.
///
/// `"http://b1:4000"` | `{ "url": "http://b1:4000", "weight": 3 }`
pub use conduit_upstream::{LoadBalanceStrategy, ProxyTarget, WeightedTarget};

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::UpstreamHealthCheck` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. Per-upstream health check for proxy routes (distinct from
/// site-level `HealthCheckConfig`).
pub use conduit_upstream::UpstreamHealthCheck;

/// Extracted into `crates/conduit-upstream` (issue #114/#142) — this is a
/// facade re-export so `crate::config::schema::OutlierDetectionConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. Passive health checking via Outlier Detection: ejects
/// upstreams that return consecutive 5xx responses from real proxy traffic,
/// with exponentially growing ejection durations.
pub use conduit_upstream::OutlierDetectionConfig;
