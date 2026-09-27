//! Authentication-related validation that stays in the root crate: `apiKey` (`ApiKeyConfig` lives here). The `consumers`,
//! `sharedJwt`, `forwardAuth` and `jwtAuth` validators live in their auth crates.

use super::ValidationError;

use crate::config::schema::ApiKeyConfig;

/// Validate an API key configuration block.
///
/// Empty strings in the key list create a bypass: when a client sends no
/// X-Api-Key header, `provided` defaults to `""` which matches `""`.
pub(super) fn validate_api_key(
    api_key_cfg: &ApiKeyConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (i, key) in api_key_cfg.keys.iter().enumerate() {
        if key.is_empty() {
            errors.push(ValidationError::new(
                format!("{prefix}.apiKey.keys[{i}]"),
                "API key must not be empty — an empty key allows unauthenticated access",
            ));
        }
    }
    if api_key_cfg.keys.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.apiKey.keys"),
            "apiKey.keys must contain at least one key",
        ));
    }
}
