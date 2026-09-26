//! Config validation for `forwardAuth`: called by the root crate's `config::validate`.

// `url` is optional: only the forwardAuth-targets-the-Admin-API check parses a URL, and that check exists only
// with the `forward-auth` feature.
#[cfg(feature = "forward-auth")]
use url::Url as ParsedUrl;

use conduit_config_core::validation::ValidationError;

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
    cfg: &crate::config::ForwardAuthConfig,
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
    _cfg: &crate::config::ForwardAuthConfig,
    _prefix: &str,
    _errors: &mut Vec<ValidationError>,
) {
}

pub fn validate_forward_auth(
    cfg: &crate::config::ForwardAuthConfig,
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
