//! Config validation for `jwtAuth`: called by the root crate's `config::validate`.

use conduit_config_core::validation::ValidationError;

/// The wording of the "exactly one of `secret` / `jwksUrl`, and `jwksUrl` must be http(s)" check. It
/// differs between `jwtAuth`, `consumers.sharedJwt` and a consumer's `jwt`, and the golden tests pin
/// each text, so the shared check ([`check_secret_or_jwks`]) takes it as data.
pub struct SecretOrJwksMessages<'a> {
    /// Neither `secret` nor `jwksUrl` is set.
    pub missing: &'a str,
    /// Both are set.
    pub both: &'a str,
    /// `jwksUrl` is not an `http://` / `https://` URL.
    pub bad_scheme: &'a str,
}

/// A JWT block needs exactly one of `secret` (HS256) or `jwksUrl` (RS256/ES256), and a `jwksUrl` must
/// be an http(s) URL. Reports at `path` for the first two problems and at `jwks_path` for the URL
/// scheme, in that order. Used by `jwtAuth`, `consumers.sharedJwt` and `consumers[].jwt` alike.
pub fn check_secret_or_jwks(
    has_secret: bool,
    jwks_url: Option<&str>,
    path: &str,
    jwks_path: &str,
    messages: &SecretOrJwksMessages<'_>,
    errors: &mut Vec<ValidationError>,
) {
    let has_jwks = jwks_url.is_some();
    if !has_secret && !has_jwks {
        errors.push(ValidationError::new(path.to_owned(), messages.missing));
    }
    if has_secret && has_jwks {
        errors.push(ValidationError::new(path.to_owned(), messages.both));
    }
    if let Some(url) = jwks_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError::new(
                jwks_path.to_owned(),
                messages.bad_scheme,
            ));
        }
    }
}

pub fn validate_jwt_auth(
    cfg: &crate::config::JwtAuthConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    check_secret_or_jwks(
        cfg.secret.is_some(),
        cfg.jwks_url.as_deref(),
        prefix,
        &format!("{prefix}.jwksUrl"),
        &SecretOrJwksMessages {
            missing: "jwtAuth requires either \"secret\" (HS256) or \"jwksUrl\" (RS256/ES256)",
            both: "jwtAuth.secret and jwtAuth.jwksUrl are mutually exclusive",
            bad_scheme: "jwksUrl must be an http:// or https:// URL",
        },
        errors,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGES: SecretOrJwksMessages<'static> = SecretOrJwksMessages {
        missing: "missing",
        both: "both",
        bad_scheme: "bad scheme",
    };

    fn run(has_secret: bool, jwks_url: Option<&str>) -> Vec<(String, String)> {
        let mut errors = Vec::new();
        check_secret_or_jwks(
            has_secret,
            jwks_url,
            "p",
            "p.jwksUrl",
            &MESSAGES,
            &mut errors,
        );
        errors.into_iter().map(|e| (e.path, e.message)).collect()
    }

    #[test]
    fn exactly_one_of_secret_and_jwks_url_is_fine() {
        assert!(run(true, None).is_empty());
        assert!(run(false, Some("https://idp/keys")).is_empty());
        assert!(run(false, Some("http://idp/keys")).is_empty());
    }

    #[test]
    fn neither_and_both_are_reported_at_the_block_path() {
        assert_eq!(run(false, None), [("p".to_owned(), "missing".to_owned())]);
        assert_eq!(
            run(true, Some("https://idp/keys")),
            [("p".to_owned(), "both".to_owned())]
        );
    }

    #[test]
    fn a_non_http_jwks_url_is_reported_at_its_own_path_after_the_others() {
        assert_eq!(
            run(false, Some("ftp://idp/keys")),
            [("p.jwksUrl".to_owned(), "bad scheme".to_owned())]
        );
        // both set AND a bad scheme: `both` first, then the scheme error, as before the helper existed
        assert_eq!(
            run(true, Some("ftp://idp/keys")),
            [
                ("p".to_owned(), "both".to_owned()),
                ("p.jwksUrl".to_owned(), "bad scheme".to_owned())
            ]
        );
    }
}
