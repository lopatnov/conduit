//! Validation of `limits` and `rateLimit`.

use super::ValidationError;

use crate::config::schema::RateLimitConfig;

pub(super) fn validate_limits(
    cfg: &crate::config::schema::LimitsConfig,
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

/// Validate the shared rate-limit rules (`windowSecs`/`limit`/`algorithm`/
/// `keyBy`/`store`).
///
/// Takes a concrete `&RateLimitConfig` — as of issue #114/#137 slice 1, the
/// site/route-level type (`crate::config::schema::RateLimitConfig`) and the
/// per-consumer type (`conduit_auth_consumers::RateLimitConfig`) are the
/// *same* type (both re-export `conduit_ratelimit::RateLimitConfig`),
/// so all three call sites below can share one signature. Before #137 this
/// took primitive fields specifically because the two were nominally
/// distinct types.
pub(super) fn validate_rate_limit(
    cfg: &RateLimitConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if cfg.window_secs == 0 {
        errors.push(ValidationError::new(
            format!("{prefix}.rateLimit.windowSecs"),
            "windowSecs must be greater than 0",
        ));
    }
    if cfg.limit == 0 {
        errors.push(ValidationError::new(
            format!("{prefix}.rateLimit.limit"),
            "limit must be greater than 0",
        ));
    }
    // "token-bucket" is the only algorithm implemented — the field exists so a typo
    // is caught at config-load time rather than silently ignored (it was previously
    // parsed and schema-declared but never read anywhere, see issue tracking the
    // 2026-08-30 rate_limit.rs integrity audit).
    if let Some(algorithm) = cfg.algorithm.as_deref() {
        if algorithm != "token-bucket" {
            errors.push(ValidationError::new(
                format!("{prefix}.rateLimit.algorithm"),
                format!("invalid algorithm \"{algorithm}\" — only \"token-bucket\" is supported"),
            ));
        }
    }
    // `keyBy: "header:<name>"` must name a syntactically valid HTTP header field —
    // `extract_key()` (src/filter/rate_limit.rs) uses `HeaderMap::get()`, which returns
    // `None` for a malformed name and silently falls back to a shared "unknown" bucket,
    // collapsing every client into one rate limit. Catch the typo at config-load time
    // instead (CodeRabbit finding on PR #302's review).
    if let Some(key_by) = cfg.key_by.as_deref() {
        if let Some(header_name) = key_by.strip_prefix("header:") {
            let valid = !header_name.is_empty()
                && header_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
            if !valid {
                errors.push(ValidationError::new(
                    format!("{prefix}.rateLimit.keyBy"),
                    format!(
                        "invalid header name \"{header_name}\" in \"header:{header_name}\" — \
                         must be a valid HTTP header field name (letters, digits, and !#$%&'*+-.^_`|~)"
                    ),
                ));
            }
        } else if key_by != "ip" {
            errors.push(ValidationError::new(
                format!("{prefix}.rateLimit.keyBy"),
                format!("invalid keyBy \"{key_by}\" — must be \"ip\" or \"header:<name>\""),
            ));
        }
    }
    // Validate the store field: must be "memory", a redis:// URL (plaintext),
    // or a rediss:// URL (TLS — requires Redis with in-transit encryption,
    // e.g. AWS ElastiCache TLS, Azure Cache for Redis).
    if let Some(store) = cfg.store.as_deref() {
        let valid_store =
            store == "memory" || store.starts_with("redis://") || store.starts_with("rediss://");
        if !valid_store {
            errors.push(ValidationError::new(
                format!("{prefix}.rateLimit.store"),
                format!(
                    "invalid store \"{store}\" — must be \"memory\", \
                     a redis:// URL (plaintext), or a rediss:// URL (TLS)"
                ),
            ));
        }
    }
}
