#![cfg(feature = "upload")]
//! Facade for the real upload server implementation, which lives in
//! `crates/conduit-upload` (issue #114/#131). This file implements that
//! crate's `UploadConfigSource` trait for the root crate's `AppState` and
//! re-exports `UploadService` as a concrete alias bound to `AppState`, so
//! `crate::upload::UploadService` keeps resolving at the same location with
//! the same call-site shape as every existing call site. `run_upload_server`
//! stays generic on re-export — it has no call sites outside this crate, so
//! there's nothing to bind it against yet (recipe rule 1 says preserve the
//! original public shape rather than drop it, not force a concrete binding
//! with no caller to justify it).

use crate::proxy::service::AppState;

impl conduit_upload::server::UploadConfigSource for AppState {
    fn upload_config(&self, site_idx: usize) -> Option<conduit_upload::UploadConfig> {
        self.config
            .load()
            .sites
            .get(site_idx)
            .and_then(|s| s.upload.clone())
    }
}

/// Pingora `BackgroundService` that drives the Axum file-upload server,
/// bound to the root crate's `AppState`. See `conduit_upload::server::UploadService`
/// for the generic implementation.
pub type UploadService = conduit_upload::server::UploadService<AppState>;

// Re-exported for API-shape parity with the pre-extraction module (no call
// sites outside this crate currently use it, but recipe rule 1 says preserve
// the original public shape rather than drop it).
pub use conduit_upload::server::run_upload_server;
