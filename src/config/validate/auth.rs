//! Authentication-related validation: `apiKey`, `consumers`, `sharedJwt`, `forwardAuth`, `jwtAuth`.

// `url` is optional (issue #144, PR 4b): only the proxy-loop warning (`proxy`) and the
// forwardAuth-targets-the-Admin-API check (`forward-auth`) parse URLs.
#[cfg(feature = "forward-auth")]
use url::Url as ParsedUrl;

use super::ValidationError;

use conduit_ratelimit::validate::validate_rate_limit;

use crate::config::schema::{ApiKeyConfig, Consumer, ConsumerJwtConfig, ConsumersSharedJwtConfig};

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

pub(super) fn validate_consumers(
    cfg: &crate::config::schema::ConsumersConfig,
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

/// Reject a forwardAuth URL that targets the Conduit Admin API (default
/// 127.0.0.1:2019) -- the `forward-auth` variant.
///
/// A misconfigured forwardAuth pointing to the admin API would allow an
/// attacker to exploit the proxy's own admin endpoint as the auth server.
/// Compiled only with `forward-auth` (issue #144, PR 4b): without it no
/// forwardAuth subrequest is ever made -- `feature_warnings()` says the whole
/// block is ignored -- so there is nothing for the rule to guard, and the
/// rule itself is unchanged wherever the guard it protects can exist.
#[cfg(feature = "forward-auth")]
fn check_forward_auth_admin_target(
    cfg: &crate::config::schema::ForwardAuthConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if let Ok(parsed) = ParsedUrl::parse(&cfg.url) {
        let host = parsed.host_str().unwrap_or("");
        let port = parsed.port().unwrap_or(80);
        let is_loopback =
            host == "localhost" || host == "127.0.0.1" || host == "::1" || host.starts_with("127.");
        if is_loopback && port == 2019 {
            errors.push(ValidationError::new(
                format!("{prefix}.url"),
                "forwardAuth.url points to 127.0.0.1:2019 — this is the Conduit Admin API. \
                 Routing external auth requests through the admin API is a security risk. \
                 Use a dedicated auth service instead.",
            ));
        }
    }
}

/// No-`forward-auth` variant of [`check_forward_auth_admin_target`]: the
/// forwardAuth block is ignored in such a build, so no admin-API target can
/// be exploited (and the URL parsing that needs the `url` crate is compiled out).
#[cfg(not(feature = "forward-auth"))]
fn check_forward_auth_admin_target(
    _cfg: &crate::config::schema::ForwardAuthConfig,
    _prefix: &str,
    _errors: &mut Vec<ValidationError>,
) {
}

pub(super) fn validate_forward_auth(
    cfg: &crate::config::schema::ForwardAuthConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if cfg.url.is_empty() {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            "forwardAuth.url must not be empty",
        ));
        return;
    } else if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
        errors.push(ValidationError::new(
            format!("{prefix}.url"),
            "forwardAuth.url must be an http:// or https:// URL",
        ));
        return;
    }

    check_forward_auth_admin_target(cfg, prefix, errors);
    if let Some(0) = cfg.timeout_ms {
        errors.push(ValidationError::new(
            format!("{prefix}.timeoutMs"),
            "forwardAuth.timeoutMs must be > 0",
        ));
    }
}

pub(super) fn validate_jwt_auth(
    cfg: &crate::config::schema::JwtAuthConfig,
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
