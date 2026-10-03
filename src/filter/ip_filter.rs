//! Extracted into `crates/conduit-ipfilter` (issue #114/#136) — this is a
//! facade re-export so `crate::filter::ip_filter::is_allowed` keeps
//! resolving to the same item at the same location for backward
//! compatibility. See `conduit_ipfilter::ip_filter` for the implementation
//! (the pure allow/deny/CIDR matching logic) and `conduit_ipfilter::guard`
//! for the real `IpGuard` (re-exported at `crate::filter::chain::IpGuard`,
//! see that file).
pub use conduit_ipfilter::ip_filter::is_allowed;
