//! Extracted into `crates/conduit-redirects` (issue #114/#140) — this is a
//! facade re-export so `crate::filter::redirects::apply_redirects` keeps
//! resolving to the same item at the same location for backward
//! compatibility. See `conduit_redirects::redirects` for the implementation
//! and `conduit_redirects::guard` for the real `RedirectGuard` guard
//! (re-exported at `crate::filter::chain::RedirectGuard`, see that file).
pub use conduit_redirects::redirects::apply_redirects;
