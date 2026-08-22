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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use conduit_upload::server::UploadConfigSource;
    use conduit_upload::UploadConfig;

    use super::*;
    use crate::config::schema::{AppConfig, SiteConfig};

    fn state_with_sites(sites: Vec<SiteConfig>) -> AppState {
        AppState::new(
            AppConfig {
                sites,
                ..Default::default()
            },
            PathBuf::new(),
            None,
        )
    }

    #[test]
    fn upload_config_returns_site_config_when_present() {
        let upload = UploadConfig {
            path: "/upload".to_string(),
            dir: "./uploads".to_string(),
            max_file_size_bytes: None,
            max_total_size_bytes: None,
            max_files: None,
            allowed_mime_types: None,
            field_name: None,
        };
        let state = state_with_sites(vec![SiteConfig {
            upload: Some(upload.clone()),
            ..Default::default()
        }]);
        assert_eq!(state.upload_config(0), Some(upload));
    }

    #[test]
    fn upload_config_returns_none_when_site_has_no_upload_block() {
        let state = state_with_sites(vec![SiteConfig::default()]);
        assert_eq!(state.upload_config(0), None);
    }

    #[test]
    fn upload_config_returns_none_for_out_of_range_index() {
        let state = state_with_sites(vec![SiteConfig::default()]);
        assert_eq!(state.upload_config(5), None);
    }

    /// Regression test for the `site_idx` lookup itself: proves the trait
    /// impl actually indexes into `sites[site_idx]` rather than, say, always
    /// returning the first (or last) site's upload config regardless of the
    /// index passed in.
    #[test]
    fn upload_config_picks_the_site_matching_the_given_index_not_just_any_site() {
        let upload_for_site_1 = UploadConfig {
            path: "/site1-upload".to_string(),
            dir: "./site1-uploads".to_string(),
            max_file_size_bytes: None,
            max_total_size_bytes: None,
            max_files: None,
            allowed_mime_types: None,
            field_name: None,
        };
        let state = state_with_sites(vec![
            SiteConfig::default(),
            SiteConfig {
                upload: Some(upload_for_site_1.clone()),
                ..Default::default()
            },
        ]);
        assert_eq!(state.upload_config(0), None);
        assert_eq!(state.upload_config(1), Some(upload_for_site_1));
    }
}
