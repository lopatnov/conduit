use serde::{Deserialize, Serialize};

/// JWT bearer-token validation configuration.
///
/// At least one of `secret` or `jwks_url` must be present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
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
