use serde::{Deserialize, Serialize};
use std::fmt;

/// JWT bearer-token validation configuration.
///
/// At least one of `secret` or `jwks_url` must be present.
#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct JwtAuthConfig {
    /// HMAC-SHA256 secret for HS256-signed tokens.  Mutually exclusive with
    /// `jwks_url`.  Stored as a plain string (use `$ENV_VAR` for security).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Remote JWKS URL for RS256 / ES256 tokens (e.g. Auth0, Google, Cognito).
    /// Keys are fetched at startup and refreshed every `jwksRefreshSecs` seconds.
    #[serde(rename = "jwksUrl", skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    /// How often to re-fetch the JWKS (seconds).  Default: 3600 (1 hour).
    #[serde(rename = "jwksRefreshSecs", skip_serializing_if = "Option::is_none")]
    pub jwks_refresh_secs: Option<u64>,
    /// Expected `aud` claim.  When set, tokens with a different audience are
    /// rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    /// Expected `iss` claim.  When set, tokens from a different issuer are
    /// rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// Paths that bypass JWT validation (same glob syntax as `basicAuth.skipPaths`).
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

/// Marker printed in place of a secret value in a manual `Debug` impl (issue
/// #354 — `secret` used to derive plain `Debug`, printing the raw HMAC key
/// verbatim on any `{:?}`-formatted print or panic message that includes it).
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl fmt::Debug for JwtAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JwtAuthConfig")
            .field("secret", &self.secret.as_ref().map(|_| Redacted))
            .field("jwks_url", &self.jwks_url)
            .field("jwks_refresh_secs", &self.jwks_refresh_secs)
            .field("audience", &self.audience)
            .field("issuer", &self.issuer)
            .field("skip_paths", &self.skip_paths)
            .finish()
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn jwt_auth_config_secret_is_redacted() {
        let cfg = JwtAuthConfig {
            secret: Some("hs256-shared-secret".to_string()),
            jwks_url: Some("https://example.com/.well-known/jwks.json".to_string()),
            jwks_refresh_secs: Some(3600),
            audience: Some(vec!["my-api".to_string()]),
            issuer: Some("https://issuer.example.com".to_string()),
            skip_paths: None,
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("hs256-shared-secret"), "got: {debug}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("jwks.json"),
            "non-secret field must still print: {debug}"
        );
    }

    #[test]
    fn jwt_auth_config_absent_secret_shows_none() {
        let cfg = JwtAuthConfig::default();
        let debug = format!("{cfg:?}");
        assert!(debug.contains("None"), "got: {debug}");
        assert!(!debug.contains("[REDACTED]"), "got: {debug}");
    }
}
