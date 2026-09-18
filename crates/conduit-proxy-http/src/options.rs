//! Config-extraction types feeding proxy-target resolution (issue #143) —
//! moved verbatim out of the root crate's `router.rs` in PR A2 (issue #419),
//! then into this crate in PR B (issue #143 itself).

use std::sync::atomic::AtomicUsize;

use dashmap::DashMap;

use conduit_upstream::health::UpstreamRegistry;

// `RouteOptions` below (and everything it needs) is only read by the gated
// resolution modules — see `lib.rs` (issue #144). `ProxyCtx` stays
// always-compiled: the root crate's router builds one unconditionally.
#[cfg(feature = "proxy")]
use conduit_cache::CacheConfig;
#[cfg(feature = "proxy")]
use conduit_upstream::{LoadBalanceStrategy, UpstreamTlsConfig};

#[cfg(feature = "proxy")]
use crate::config::{
    ConnectionPoolConfig, ProxyRouteTarget, ProxyTimeout, RetryConfig, RewriteRule, StickyConfig,
};

/// Inputs that stay constant while resolving one request's upstream.
///
/// `pub` (not `pub(crate)`) with `pub` fields: the root crate's `router.rs`
/// constructs this via struct-literal syntax before calling into
/// `crate::resolve::resolve_proxy_routes` (which only exists with the
/// `proxy` feature).
pub struct ProxyCtx<'a> {
    pub path: &'a str,
    pub client_ip: &'a str,
    pub req_headers: &'a http::HeaderMap,
    pub counters: &'a DashMap<String, AtomicUsize>,
    pub upstream_health: &'a UpstreamRegistry,
    pub site_label: &'a str,
}

/// Per-route proxy settings, read once from `ProxyRouteTarget::Full`. All
/// fields borrow from the route config; shorthand targets (`Url` /
/// `RoundRobin`) get the documented defaults.
#[cfg(feature = "proxy")]
pub(crate) struct RouteOptions<'a> {
    pub(crate) retry: Option<&'a RetryConfig>,
    pub(crate) timeout: Option<&'a ProxyTimeout>,
    pub(crate) pool: Option<&'a ConnectionPoolConfig>,
    pub(crate) strategy: Option<&'a LoadBalanceStrategy>,
    pub(crate) http2: bool,
    pub(crate) hash_key: &'a str,
    pub(crate) cache: Option<&'a CacheConfig>,
    pub(crate) rewrite: Option<&'a [RewriteRule]>,
    pub(crate) mirror: Option<&'a str>,
    pub(crate) upstream_tls: Option<&'a UpstreamTlsConfig>,
    pub(crate) max_conns_per_upstream: Option<u64>,
    /// `healthCheck.slowStartSecs` (issue #157) — traffic ramp-up window
    /// after an upstream recovers. Ignored for hash-based strategies and
    /// sticky sessions; see `slow_start`'s module doc comment for why.
    pub(crate) slow_start_secs: Option<u64>,
    pub(crate) websocket: bool,
    pub(crate) unhealthy_status: &'a [u16],
    pub(crate) unhealthy_latency_ms: Option<u64>,
    pub(crate) backup: Option<&'a str>,
    pub(crate) sticky: Option<&'a StickyConfig>,
    pub(crate) strip_prefix: bool,
}

#[cfg(feature = "proxy")]
impl<'a> RouteOptions<'a> {
    pub(crate) fn from_target(target: &'a ProxyRouteTarget) -> Self {
        let ProxyRouteTarget::Full(cfg) = target else {
            return Self::shorthand();
        };
        let hc = cfg.health_check.as_ref();
        Self {
            retry: cfg.retry.as_ref(),
            timeout: cfg.timeout.as_ref(),
            pool: cfg.pool.as_ref(),
            strategy: cfg.strategy.as_ref(),
            http2: cfg.http2.unwrap_or(false),
            hash_key: cfg.hash_key.as_deref().unwrap_or("ip"),
            cache: cfg.cache.as_ref(),
            rewrite: cfg.rewrite.as_deref(),
            mirror: cfg.mirror.as_deref(),
            upstream_tls: cfg.upstream_tls.as_ref(),
            max_conns_per_upstream: hc.and_then(|h| h.max_connections_per_upstream),
            slow_start_secs: hc.and_then(|h| h.slow_start_secs),
            websocket: cfg.websocket.unwrap_or(false),
            unhealthy_status: hc
                .and_then(|h| h.unhealthy_status.as_deref())
                .unwrap_or(&[]),
            unhealthy_latency_ms: hc.and_then(|h| h.unhealthy_latency_ms),
            backup: cfg.backup.as_deref(),
            sticky: cfg.sticky.as_ref(),
            strip_prefix: cfg.strip_prefix.unwrap_or(false),
        }
    }

    fn shorthand() -> Self {
        Self {
            retry: None,
            timeout: None,
            pool: None,
            strategy: None,
            http2: false,
            hash_key: "ip",
            cache: None,
            rewrite: None,
            mirror: None,
            upstream_tls: None,
            max_conns_per_upstream: None,
            slow_start_secs: None,
            websocket: false,
            unhealthy_status: &[],
            unhealthy_latency_ms: None,
            backup: None,
            sticky: None,
            strip_prefix: false,
        }
    }
}
