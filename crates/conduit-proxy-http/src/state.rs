//! Per-request proxy-routing state (issue #143 — PR A1, issue #418, grouped
//! the 14 proxy-specific fields that used to live directly on `RequestCtx`
//! into [`ProxyReqState`]; PR B, issue #143 itself, moved this module here
//! from the root crate's `src/proxy/routing/state.rs`).
//!
//! `RequestCtx` (root crate, `src/proxy/ctx.rs`) embeds [`ProxyReqState`] as
//! a single `pub proxy: ProxyReqState` field via a facade re-export.
//!
//! Mirrors the `RequestCtx` sub-struct pattern already established by
//! `conduit_limits::LimitsReqState` (`crates/conduit-limits/src/ctx.rs`),
//! `conduit_cache::CacheReqState`, and `conduit_auth_jwt::guard::JwtReqState`
//! — see `CLAUDE.md` architectural decision #30. Unlike those three (which
//! are either always-on-but-crate-owned or `#[cfg(feature = "...")]`-gated),
//! [`ProxyReqState`] is unconditional: even though this crate now has a
//! `proxy` Cargo feature (issue #144), the state struct stays always-compiled
//! — the root crate's `RequestCtx` embeds it in every build, it is plain data
//! with no third-party dependencies, and gating it would churn ~117 call
//! sites for no footprint gain (see #144's design plan).

use std::time::Instant;

use conduit_cache::CacheConfig;
use conduit_ratelimit::RateLimitConfig;

use crate::config::{ConnectionPoolConfig, ProxyTimeout};

/// Per-route rate limit plus the bucket-key fragment identifying the route
/// it came from. Populated at routing time by whichever matcher matched
/// (`proxy` map or `routes[]`), so enforcement (`request_phase.rs::
/// enforce_route_rate_limit`) can never disagree with the routing decision —
/// see issue #360, which found the previous approach (a *second*,
/// post-routing path matcher scanning only `site.proxy`) could apply the
/// wrong route's rate limit to a request that actually resolved via
/// `site.routes[]`.
#[derive(Debug, Clone)]
pub struct RouteRateLimit {
    pub config: RateLimitConfig,
    /// `proxy` map key (e.g. `/api`) or `routes[{i}]`.
    pub route_key: String,
}

/// Per-request retry state for proxy routes that have `retry` configured.
///
/// The URL list is rotated so that `urls[0]` is the round-robin starting
/// target for this particular request.  Subsequent retries advance through
/// `urls[1 % len]`, `urls[2 % len]`, etc.
#[derive(Debug)]
pub struct RetryState {
    /// All target URLs for the route, rotated to start at the RR position.
    pub urls: Vec<String>,
    /// Number of times `upstream_peer()` has been called so far (0 = first call).
    pub attempt: usize,
    /// Total attempts allowed including the initial one (e.g. `attempts: 3` ⇒ 3 tries).
    pub max_attempts: usize,
    /// Error conditions that should trigger a retry.
    /// Valid values: `"connection_error"` | `"5xx"` | `"timeout"`.
    pub conditions: Vec<String>,
    /// Optional delay between retries in milliseconds.
    pub backoff_ms: Option<u64>,
    /// When `true`, jitter ±50% is applied to `backoff_ms` to avoid retry storms.
    pub backoff_jitter: bool,
    /// Maximum percentage of in-flight requests that may be retries (0.0–100.0).
    ///
    /// Prevents retry storms: when many requests fail simultaneously, an
    /// unconstrained retry budget multiplies load by `1 + attempts`.
    /// `None` means unlimited retries are allowed (legacy behaviour).
    pub budget_percent: Option<f64>,
    /// Set to `true` once this request has been promoted to a retry.
    ///
    /// The `logging()` hook reads this flag to decrement `AppState.retry_inflight`
    /// after the retry response is delivered.
    pub is_retrying: bool,
    /// `healthCheck.maxConnectionsPerUpstream` for this route, captured at
    /// routing time (#216 part 2). `None` = no cap.
    ///
    /// Stored here rather than re-read from config inside `upstream_peer`
    /// so every attempt of one request evaluates capacity against the SAME
    /// config snapshot that produced `urls` -- the routing-vs-helper
    /// TOCTOU discipline established by PR #92.
    pub max_conns_per_upstream: Option<u64>,
    /// `true` when this route acquires a real `conn_count` slot per retry
    /// attempt (#216 part 2): `is_least_conn || circuit_tracking` at
    /// routing time, mirroring the same condition that decided
    /// `RouteResolution.upstream_conn_slot` for the first attempt.
    ///
    /// Only consulted for attempt 1+ -- the first attempt's slot is
    /// whatever routing already acquired before `upstream_peer` was ever
    /// called, untouched by the retry machinery.
    pub tracks_conn_slot: bool,
}

impl RetryState {
    /// Returns `true` when there are retries left (i.e. we have not yet exhausted
    /// `max_attempts`).  Call this *after* `attempt` has been incremented by
    /// `upstream_peer()`.
    pub fn has_attempts_left(&self) -> bool {
        self.attempt < self.max_attempts
    }

    pub fn has_condition(&self, cond: &str) -> bool {
        self.conditions.iter().any(|c| c == cond)
    }
}

/// Per-request proxy-routing state, threaded through the request pipeline.
///
/// Embedded in `RequestCtx` as `pub proxy: ProxyReqState` (`src/proxy/ctx.rs`).
#[derive(Debug, Default)]
pub struct ProxyReqState {
    /// Populated when the matched route has a `retry` configuration.
    pub retry: Option<RetryState>,
    /// Per-route proxy connection timeouts (from `proxy.*.timeout`).
    pub proxy_timeout: Option<ProxyTimeout>,
    /// Per-route connection pool settings (from `proxy.*.pool`).
    pub proxy_pool: Option<ConnectionPoolConfig>,
    /// When `true`, negotiate HTTP/2 with the upstream (ALPN H2H1).
    /// Derived from `proxy.*.http2: true` in the route config.
    pub proxy_http2: bool,
    /// The upstream URL that was selected for this request.
    ///
    /// `Some` for every proxied request (not just `least-conn`/circuit-breaker
    /// routes) so that Peak EWMA, Outlier Detection, per-peer response stats,
    /// and the per-upstream Prometheus gauges can attribute this request no
    /// matter which load-balancing strategy picked it. Whether this request
    /// also holds a `conn_count` slot that must be released is tracked
    /// separately by [`upstream_conn_slot`](Self::upstream_conn_slot) — the
    /// two must not be conflated, since `conn_count` is keyed by URL alone and
    /// a phantom decrement from an attribution-only request would corrupt the
    /// slot count for a *different* route sharing the same upstream.
    pub proxy_upstream_url: Option<String>,
    /// `true` when routing acquired a `conn_count` slot for
    /// `proxy_upstream_url` (via `conn_inc` / least-conn selection) that this
    /// request is responsible for releasing via `conn_dec`.
    ///
    /// `false` when `proxy_upstream_url` is populated for passive-health
    /// attribution only (no slot was acquired) — e.g. any non-least-conn
    /// route with no `maxConnectionsPerUpstream` configured.
    ///
    /// **Invariant (#216):** `upstream_conn_slot == true` ⟺ this request
    /// holds exactly one outstanding `conn_inc` on the URL currently in
    /// `proxy_upstream_url`. Every mutation of `proxy_upstream_url` must
    /// therefore be preceded by the root crate's `request_phase::
    /// release_conn_slot` (which releases any slot held on the *old* value
    /// and clears both fields) — never assign `proxy_upstream_url` or this
    /// field directly. Use `request_phase::acquire_conn_slot` to point at a
    /// new URL afterward (both live in the root crate's
    /// `src/proxy/request_phase.rs`, out of this crate's reach — not an
    /// intra-doc link for that reason). Before #216 this was violated by
    /// `upstream_peer`'s retry-restore path, which overwrote
    /// `proxy_upstream_url` for the next retry attempt without releasing
    /// the previous value's slot on two of the three retry-failure paths
    /// (connect-phase and proxy-phase-timeout — only the 5xx path, via
    /// `record_failed_upstream_for_retry`, released correctly) — a real,
    /// unbounded leak: `conn_count` would rise monotonically until the
    /// affected upstream was permanently excluded by `Capacity::evaluate`.
    pub upstream_conn_slot: bool,
    /// Cache configuration for this route (`proxy.*.cache`), if caching is enabled.
    ///
    /// `None` means the route has no cache config and caching is disabled for
    /// this request.
    pub proxy_cache_cfg: Option<CacheConfig>,
    /// Timestamp recorded at the start of `upstream_request_filter` — i.e. the
    /// moment the proxied request was forwarded to the upstream.
    ///
    /// Used to compute `upstream_response_time`: the duration between sending
    /// the request and receiving the first byte of the upstream response.
    /// `None` for local handlers (health, static, metrics, …).
    pub upstream_start: Option<Instant>,
    /// Passive health check: HTTP status codes that count as upstream failures.
    ///
    /// Populated from `healthCheck.unhealthyStatus` during routing.
    /// If the response status matches, `consecutive_5xx` is incremented.
    /// Default (empty) falls back to the standard 5xx-only detection.
    pub passive_unhealthy_status: Vec<u16>,
    /// Passive health check: latency threshold in milliseconds.
    ///
    /// Populated from `healthCheck.unhealthyLatencyMs` during routing.
    /// If the upstream response time exceeds this, it counts as a failure.
    pub passive_unhealthy_latency_ms: Option<u64>,
    /// Whether this route explicitly allows WebSocket upgrades.
    ///
    /// Set from `proxy.*.websocket: true` in the route config.  When `false`
    /// (the default), any `101 Switching Protocols` response from upstream is
    /// rejected with `502 Bad Gateway` to prevent unexpected protocol tunnelling.
    pub websocket_allowed: bool,
    /// Sticky-session cookie to set on the response when HMAC signing is enabled.
    ///
    /// Populated during routing when `sticky.secret` is configured.
    /// Format: `(cookie_name, hmac_signed_value)`.  The `upstream_response_filter`
    /// injects the corresponding `Set-Cookie` header.
    pub sticky_set_cookie: Option<(String, String)>,
    /// Per-route rate limit selected during routing (issue #360), together
    /// with the bucket-key fragment identifying which route it came from.
    ///
    /// Populated by whichever matcher actually resolved this request — the
    /// legacy `proxy` map (`router.rs::resolve_proxy_routes`) or the newer
    /// `routes[]` array (`routes.rs::match_routes`) — so
    /// `request_phase.rs::enforce_route_rate_limit` can enforce exactly the
    /// matched route's limit instead of re-deriving it from `site.proxy`
    /// after the fact, which could silently apply a *different* route's
    /// limit (or none at all) when the request actually resolved via
    /// `site.routes[]`. `None` when the matched route has no `rateLimit`
    /// configured.
    pub route_rate_limit: Option<RouteRateLimit>,
    /// Effective route priority (`proxy.*.priority` / `routes[].proxy.priority`)
    /// selected during routing (issue #360), for post-routing load shedding
    /// (`request_phase.rs::shed_low_priority_request`). `None` when the
    /// matched route has no `priority` configured.
    pub route_priority: Option<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_retry(attempt: usize, max: usize, conditions: &[&str]) -> RetryState {
        RetryState {
            urls: vec!["http://a:4000".to_string()],
            attempt,
            max_attempts: max,
            conditions: conditions.iter().map(|s| s.to_string()).collect(),
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None,
            is_retrying: false,
            max_conns_per_upstream: None,
            tracks_conn_slot: false,
        }
    }

    #[test]
    fn has_attempts_left_when_under_max() {
        assert!(make_retry(0, 3, &[]).has_attempts_left());
        assert!(make_retry(2, 3, &[]).has_attempts_left());
    }

    #[test]
    fn no_attempts_left_when_at_max() {
        assert!(!make_retry(3, 3, &[]).has_attempts_left());
        assert!(!make_retry(5, 3, &[]).has_attempts_left());
    }

    #[test]
    fn has_condition_matches_exact_string() {
        let rs = make_retry(0, 3, &["5xx", "connection_error"]);
        assert!(rs.has_condition("5xx"));
        assert!(rs.has_condition("connection_error"));
        assert!(!rs.has_condition("timeout"));
    }

    #[test]
    fn has_condition_empty_list_never_matches() {
        let rs = make_retry(0, 3, &[]);
        assert!(!rs.has_condition("5xx"));
    }

    #[test]
    fn proxy_req_state_default_matches_request_ctx_new_zero_values() {
        // Every hardcoded default in the old `RequestCtx::new` body must be
        // reproduced exactly by `ProxyReqState::default()` (task acceptance
        // criterion for #143 PR A1).
        let s = ProxyReqState::default();
        assert!(s.retry.is_none());
        assert!(s.proxy_timeout.is_none());
        assert!(s.proxy_pool.is_none());
        assert!(!s.proxy_http2);
        assert!(s.proxy_upstream_url.is_none());
        assert!(!s.upstream_conn_slot);
        assert!(s.proxy_cache_cfg.is_none());
        assert!(s.upstream_start.is_none());
        assert!(s.passive_unhealthy_status.is_empty());
        assert!(s.passive_unhealthy_latency_ms.is_none());
        assert!(!s.websocket_allowed);
        assert!(s.sticky_set_cookie.is_none());
        assert!(s.route_rate_limit.is_none());
        assert!(s.route_priority.is_none());
    }
}
