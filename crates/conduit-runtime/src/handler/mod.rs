pub mod health;

// Paths the moved files use through `crate::handler::…` (facades over Layer-0/1 crates in the root crate).
#[cfg(feature = "acme")]
pub use conduit_acme::challenge as acme_challenge;
pub use conduit_core::handler::response;
pub use conduit_core::handler::LocalHandlerImpl;
pub use conduit_metrics::handler as metrics;
#[cfg(feature = "static")]
pub use conduit_static::fallback;
#[cfg(feature = "static")]
pub use conduit_static::handler as static_files;

#[cfg(feature = "hotreload")]
pub mod hot_reload {
    pub use conduit_hotreload::handler::{
        handle_client_js, handle_sse, HotReloadJsHandler, HotReloadSseHandler,
    };
    pub use conduit_hotreload::watcher::{build_watch_config, run_file_watcher};
}
