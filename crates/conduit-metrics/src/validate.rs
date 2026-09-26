//! Config validation for `metrics`: called by the root crate's `config::validate`.

use crate::config::MetricsConfig;
use conduit_config_core::validation::ValidationError;

pub fn validate_metrics(cfg: &MetricsConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    if let Some(path) = &cfg.path {
        if !path.starts_with('/') {
            errors.push(ValidationError::new(
                format!("{prefix}.metrics.path"),
                format!("Metrics path '{path}' must start with '/'"),
            ));
        }
    }
    // An empty token string is not the same as "no token configured"
    // (`None`, which already warns via `check_metrics_auth_warnings`) — but
    // the constant-time comparison in `handle_metrics` treats a missing
    // `Authorization` header as an empty `provided` string, so `token: ""`
    // silently matches it and authenticates every request with no
    // credentials at all. Reject outright rather than normalizing to `None`,
    // which would silently pick the unauthenticated path instead of
    // surfacing the operator's likely mistake (e.g. an unresolved `$VAR`
    // that expanded to an empty string).
    if cfg.token.as_deref() == Some("") {
        errors.push(ValidationError::new(
            format!("{prefix}.metrics.token"),
            "metrics.token must not be an empty string — an empty token matches a request \
             with no Authorization header at all, authenticating every request. Omit the \
             field entirely to leave the endpoint unauthenticated, or set a real token."
                .to_owned(),
        ));
    }
}
