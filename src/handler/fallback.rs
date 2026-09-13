//! Fallback-response handler facade (404, SPA shell, custom body).
//!
//! Extracted into `crates/conduit-static` (issue #114/#139) — this file is a
//! thin re-export so `crate::handler::fallback::*` call sites
//! (`src/proxy/request_phase.rs`) don't need to change. Compiled only when
//! the `static` feature is enabled — see the `#[cfg(feature = "static")]
//! pub mod fallback;` gate in `src/handler/mod.rs`.
pub use conduit_static::fallback::{handle_fallback, FallbackHandler};
