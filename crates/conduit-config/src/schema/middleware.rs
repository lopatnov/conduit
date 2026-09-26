//! Scripted middleware chain and fault injection.

// ── Middleware chain ───────────────────────────────────────────────────────

/// Extracted into `crates/conduit-middleware` (issue #114/#141) — this is a
/// facade re-export so `crate::config::schema::MiddlewareEntry` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_middleware::MiddlewareEntry;

/// Extracted into `crates/conduit-faults` (issue #114/#132) — this is a
/// facade re-export so `crate::config::schema::{FaultInjectionConfig,
/// FaultAbort, FaultDelay}` keep resolving to the same types at the same
/// location for every existing call site/test.
///
/// ```json
/// {
///   "faultInjection": {
///     "abort": { "percent": 5,  "status": 503 },
///     "delay": { "percent": 10, "ms": 200 }
///   }
/// }
/// ```
pub use conduit_faults::{FaultAbort, FaultDelay, FaultInjectionConfig};
