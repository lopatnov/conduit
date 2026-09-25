//! Validation of the smaller per-site handlers: `middleware`, `redirects`, `fallback`, `upload`, `metrics`.

use super::ValidationError;

use crate::config::schema::{
    FallbackConfig, MetricsConfig, MiddlewareEntry, RedirectRule, UploadConfig,
};

pub(super) fn validate_middleware(
    entries: &[MiddlewareEntry],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (i, entry) in entries.iter().enumerate() {
        let entry_prefix = format!("{prefix}.middleware[{i}]");
        match entry.r#type.as_str() {
            // Both script and wasm require a `path` field.
            "script" | "wasm" => {
                if entry.path.is_none() {
                    errors.push(ValidationError::new(
                        format!("{entry_prefix}.path"),
                        format!(
                            "middleware type {:?} requires a \"path\" field",
                            entry.r#type
                        ),
                    ));
                }
            }
            "ipFilter" | "rateLimit" | "auth" | "headers" => {
                // Built-in types — currently executed via top-level config fields;
                // listed here so the validator accepts them without error.
            }
            other => {
                errors.push(ValidationError::new(
                    format!("{entry_prefix}.type"),
                    format!("unknown middleware type \"{other}\""),
                ));
            }
        }

        // Only "request" (the default) and "response" are ever checked against —
        // any other value (a typo like "resposne") silently falls through both the
        // request-phase and response-phase skip checks and runs in whichever phase
        // it wasn't meant for, or in both. Catch it here instead.
        if let Some(phase) = entry.phase.as_deref() {
            if phase != "request" && phase != "response" {
                errors.push(ValidationError::new(
                    format!("{entry_prefix}.phase"),
                    format!("invalid phase \"{phase}\" — must be \"request\" or \"response\""),
                ));
            }
        }
    }
}

pub(super) fn validate_redirect_rules(
    redirects: &[RedirectRule],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (j, rule) in redirects.iter().enumerate() {
        if let Some(status) = rule.status {
            if !matches!(status, 301 | 302 | 307 | 308) {
                errors.push(ValidationError::new(
                    format!("{prefix}.redirects[{j}].status"),
                    format!("Invalid redirect status {status} — must be 301, 302, 307, or 308"),
                ));
            }
        }
    }
}

pub(super) fn validate_fallback(
    fb: &FallbackConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(status) = fb.status {
        if !(100..=599).contains(&status) {
            errors.push(ValidationError::new(
                format!("{prefix}.fallback.status"),
                format!(
                    "Invalid fallback status {status} \
                     — must be a valid HTTP status code (100–599)"
                ),
            ));
        }
    }
}

pub(super) fn validate_upload(cfg: &UploadConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
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

pub(super) fn validate_metrics(
    cfg: &MetricsConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
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
