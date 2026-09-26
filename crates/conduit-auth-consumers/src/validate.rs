//! Config validation for `consumers` (incl. `sharedJwt`): called by the root crate's `config::validate`.

use conduit_config_core::validation::ValidationError;
use conduit_ratelimit::validate::validate_rate_limit;

use crate::config::{Consumer, ConsumerJwtConfig, ConsumersSharedJwtConfig};

pub fn validate_consumers(
    cfg: &crate::config::ConsumersConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(ref sj) = cfg.shared_jwt {
        validate_shared_jwt(sj, prefix, errors);
    }
    let has_shared_jwt = cfg.shared_jwt.is_some();
    let mut seen_usernames = std::collections::HashSet::new();
    for (i, c) in cfg.consumers.iter().enumerate() {
        validate_consumer_entry(c, i, prefix, has_shared_jwt, &mut seen_usernames, errors);
    }
}

/// Validate the `consumers.sharedJwt` block.
fn validate_shared_jwt(
    sj: &ConsumersSharedJwtConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let sj_prefix = format!("{prefix}.sharedJwt");
    let has_secret = sj.secret.is_some();
    let has_jwks = sj.jwks_url.is_some();
    if !has_secret && !has_jwks {
        errors.push(ValidationError::new(
            sj_prefix.clone(),
            "consumers.sharedJwt requires either \"secret\" (HS256) or \"jwksUrl\" (RS256/ES256)",
        ));
    }
    if has_secret && has_jwks {
        errors.push(ValidationError::new(
            sj_prefix.clone(),
            "consumers.sharedJwt.secret and sharedJwt.jwksUrl are mutually exclusive",
        ));
    }
    if let Some(url) = &sj.jwks_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{sj_prefix}.jwksUrl"),
                "consumers.sharedJwt.jwksUrl must be an http:// or https:// URL",
            ));
        }
    }
}

/// Validate a single consumer entry.
fn validate_consumer_entry(
    c: &Consumer,
    i: usize,
    prefix: &str,
    has_shared_jwt: bool,
    seen_usernames: &mut std::collections::HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    let entry_prefix = format!("{prefix}.consumers[{i}]");
    if c.username.is_empty() {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.username"),
            "consumer username must not be empty",
        ));
    }
    // Empty consumer API key creates a bypass (same as site-level).
    if let Some(ref key) = c.api_key {
        if key.is_empty() {
            errors.push(ValidationError::new(
                format!("{entry_prefix}.apiKey"),
                "consumer apiKey must not be empty — an empty key allows unauthenticated access",
            ));
        }
    }
    // When sharedJwt is configured, individual consumers are identified by the
    // sharedJwt sub claim and don't need their own credentials.
    if !has_shared_jwt && c.api_key.is_none() && c.basic_auth.is_none() && c.jwt.is_none() {
        errors.push(ValidationError::new(
            entry_prefix.clone(),
            "consumer requires at least one credential: apiKey, basicAuth, jwt (or configure consumers.sharedJwt)",
        ));
    }
    if let Some(ref jwt_cfg) = c.jwt {
        validate_consumer_jwt(jwt_cfg, &entry_prefix, errors);
    }
    if !seen_usernames.insert(c.username.clone()) {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.username"),
            format!("consumer username {:?} is duplicated", c.username),
        ));
    }
    if let Some(ref ba) = c.basic_auth {
        if ba.password.is_empty() {
            errors.push(ValidationError::new(
                format!("{entry_prefix}.basicAuth.password"),
                "consumer basicAuth.password must not be empty",
            ));
        }
    }
    if let Some(ref rl) = c.rate_limit {
        validate_rate_limit(rl, &entry_prefix, errors);
    }
}

/// Validate a consumer-level JWT config block.
fn validate_consumer_jwt(
    jwt_cfg: &ConsumerJwtConfig,
    entry_prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let has_secret = jwt_cfg.secret.is_some();
    let has_jwks = jwt_cfg.jwks_url.is_some();
    if !has_secret && !has_jwks {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.jwt"),
            "consumer jwt requires either \"secret\" (HS256) or \"jwksUrl\" (RS256/ES256)",
        ));
    }
    if has_secret && has_jwks {
        errors.push(ValidationError::new(
            format!("{entry_prefix}.jwt"),
            "consumer jwt.secret and jwt.jwksUrl are mutually exclusive",
        ));
    }
    if let Some(url) = &jwt_cfg.jwks_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{entry_prefix}.jwt.jwksUrl"),
                "consumer jwt.jwksUrl must be an http:// or https:// URL",
            ));
        }
    }
}
