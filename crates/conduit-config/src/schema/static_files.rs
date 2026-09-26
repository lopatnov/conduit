//! Static file serving and the site fallback.

// ── Static files ───────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-static` (issue #114/#139) — this is a
/// facade re-export so `crate::config::schema::{StaticConfig,
/// StaticOptions}` keep resolving to the same types at the same location
/// for every existing call site/test.
///
/// `"./dist"` | `["./a", "./b"]` | `{ "/": "./dist", "/docs": "./docs-dist" }`
pub use conduit_static::{StaticConfig, StaticOptions};

// ── Fallback ───────────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-static` (issue #114/#139) — this is a
/// facade re-export so `crate::config::schema::{FallbackConfig,
/// FallbackRule}` keep resolving to the same types at the same location for
/// every existing call site/test.
pub use conduit_static::{FallbackConfig, FallbackRule};
