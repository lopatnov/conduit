//! Browser hot-reload facade.
//!
//! Extracted into `crates/conduit-hotreload` (issue #114/#140) — this file
//! is a thin re-export so `crate::handler::hot_reload::*` call sites
//! (`src/proxy/request_phase.rs`, `src/admin/api.rs`) don't need to change.
//! Compiled only when the `hotreload` feature is enabled — see the
//! `#[cfg(feature = "hotreload")] pub mod hot_reload;` gate in
//! `src/handler/mod.rs`.
//!
//! `HotReloadConfig`/`HotReloadOptions` (the schema types) moved too, but
//! are re-exported at their *original* location, `crate::config::schema` —
//! see that module for the facade.
pub use conduit_hotreload::handler::{
    handle_client_js, handle_sse, HotReloadJsHandler, HotReloadSseHandler,
};
pub use conduit_hotreload::watcher::{build_watch_config, run_file_watcher};
