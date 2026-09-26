//! Config validation for `upload`: called by the root crate's `config::validate`.

use crate::config::UploadConfig;
use conduit_config_core::validation::ValidationError;

pub fn validate_upload(cfg: &UploadConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if !cfg.path.starts_with('/') {
        errors.push(ValidationError::new(
            format!("{prefix}.upload.path"),
            format!("Upload path '{}' must start with '/'", cfg.path),
        ));
    }
    if cfg.dir.trim().is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.upload.dir"),
            "Upload directory path must not be empty",
        ));
    }
}
