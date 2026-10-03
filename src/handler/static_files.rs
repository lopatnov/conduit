//! Static-file serving handler facade.
//!
//! Extracted into `crates/conduit-static` (issue #114/#139) — this file is a
//! thin re-export so `crate::handler::static_files::*` call sites
//! (`src/proxy/request_phase.rs`) don't need to change. Compiled only when
//! the `static` feature is enabled — see the `#[cfg(feature = "static")]
//! pub mod static_files;` gate in `src/handler/mod.rs`.
pub use conduit_static::handler::{handle_static, StaticFileHandler};
