//! Extracted into `crates/conduit-security-headers` (issue #114/#136) — this
//! is a facade re-export so `crate::filter::security_headers::{header_entries,
//! is_host_allowed}` keep resolving to the same items at the same location
//! for backward compatibility. See `conduit_security_headers::security_headers`
//! for the implementation and `conduit_security_headers::guard` for the real
//! `AllowedHostsGuard` (re-exported at `crate::filter::chain::AllowedHostsGuard`,
//! see that file).
pub use conduit_security_headers::security_headers::{header_entries, is_host_allowed};
