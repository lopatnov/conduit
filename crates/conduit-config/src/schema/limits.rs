//! Request admission: IP filter and request limits.

// ── IP filter ──────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-ipfilter` (issue #114/#136) — this is a
/// facade re-export so `crate::config::schema::IpFilterConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_ipfilter::IpFilterConfig;

// ── Request limits ─────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-limits` (issue #114/#137) — this is a
/// facade re-export so `crate::config::schema::LimitsConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_limits::LimitsConfig;
