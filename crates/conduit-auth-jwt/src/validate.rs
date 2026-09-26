//! Config validation for `jwtAuth`: called by the root crate's `config::validate`.

use conduit_config_core::validation::ValidationError;

pub fn validate_jwt_auth(
    cfg: &crate::config::JwtAuthConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let has_secret = cfg.secret.is_some();
    let has_jwks = cfg.jwks_url.is_some();
    if !has_secret && !has_jwks {
        errors.push(ValidationError::new(
            prefix.to_owned(),
            "jwtAuth requires either \"secret\" (HS256) or \"jwksUrl\" (RS256/ES256)",
        ));
    }
    if has_secret && has_jwks {
        errors.push(ValidationError::new(
            prefix.to_owned(),
            "jwtAuth.secret and jwtAuth.jwksUrl are mutually exclusive",
        ));
    }
    if let Some(url) = &cfg.jwks_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{prefix}.jwksUrl"),
                "jwksUrl must be an http:// or https:// URL",
            ));
        }
    }
    // Matches schema/conduit.schema.json's documented minimum: below 60s, the
    // JWKS cache (currently a synchronous per-request fetch on expiry — #163)
    // would refetch on nearly every request instead of caching meaningfully.
    if let Some(refresh) = cfg.jwks_refresh_secs {
        if refresh < 60 {
            errors.push(ValidationError::new(
                format!("{prefix}.jwksRefreshSecs"),
                "jwtAuth.jwksRefreshSecs must be >= 60",
            ));
        }
    }
}
