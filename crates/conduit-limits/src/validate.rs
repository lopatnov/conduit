//! Config validation for `limits`: called by the root crate's `config::validate`.

use conduit_config_core::validation::ValidationError;

pub fn validate_limits(
    cfg: &crate::config::LimitsConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if cfg.max_inflight_requests == Some(0) {
        errors.push(ValidationError::new(
            format!("{prefix}.maxInflightRequests"),
            "limits.maxInflightRequests must be >= 1 (set to null/omit to disable)",
        ));
    }
    if cfg.max_body_bytes == Some(0) {
        errors.push(ValidationError::new(
            format!("{prefix}.maxBodyBytes"),
            "limits.maxBodyBytes must be >= 1 (set to null/omit to disable)",
        ));
    }
    if cfg.timeout_secs == Some(0) {
        errors.push(ValidationError::new(
            format!("{prefix}.timeoutSecs"),
            "limits.timeoutSecs must be >= 1 (set to null/omit to disable)",
        ));
    }
}
