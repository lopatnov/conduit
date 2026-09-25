//! Reverse-proxy config: routes/targets, rewrite, timeouts, connection pool, response cache,
//! retry, and the raw TCP proxy.

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

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::ProxyTimeout` keeps resolving
/// to the same type at the same location for every existing call site/test.
pub use conduit_proxy_http::config::ProxyTimeout;

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::ConnectionPoolConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_proxy_http::config::ConnectionPoolConfig;

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
