//! Per-site routing rules and redirects.

// ── Redirects ──────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-redirects` (issue #114/#140) — this is a
/// facade re-export so `crate::config::schema::RedirectRule` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_redirects::RedirectRule;

// ── Routes (Phase 3.6) ─────────────────────────────────────────────────────

/// Extracted into `crates/conduit-proxy-http` (issue #114/#143) — this is a
/// facade re-export so `crate::config::schema::{RouteConfig, MatchConfig}`
/// keep resolving to the same types at the same location for every existing
/// call site/test. A single named routing rule (`RouteConfig`) plus the
/// match criteria that decide when it fires (`MatchConfig`) — see
/// `conduit_proxy_http::config`'s own doc comments for the field-level
/// detail.
pub use conduit_proxy_http::config::{MatchConfig, RouteConfig};
