use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Marker printed in place of a secret value in a manual `Debug` impl (issue
/// #354 — every secret-bearing field in this module used to derive plain
/// `Debug`, printing the raw value verbatim on any `{:?}`-formatted print or
/// panic message that happens to include it).
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

/// Named-consumer authentication: credentials and per-consumer policies stored
/// per-consumer rather than per-route.
///
/// When a request matches a consumer's credentials:
/// 1. The consumer's username is injected as `X-Consumer-ID` (or `idHeader`)
///    into the upstream request.
/// 2. Any per-consumer `headers` are also injected.
/// 3. Per-consumer `rateLimit` is applied (independent of the site rate limit).
///
/// Requests that don't match any consumer receive 401 Unauthorized.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsumersConfig {
    /// The list of named consumers (evaluated in order; first match wins).
    #[serde(default)]
    pub consumers: Vec<Consumer>,
    /// Header name used to inject the consumer's username into the upstream
    /// request.  Defaults to `"x-consumer-id"`.
    #[serde(rename = "idHeader", skip_serializing_if = "Option::is_none")]
    pub id_header: Option<String>,
    /// Header name used to read the API key from the request.
    /// Defaults to `"x-api-key"`.  Only relevant for consumers that use
    /// `apiKey` credentials.
    #[serde(rename = "apiKeyHeader", skip_serializing_if = "Option::is_none")]
    pub api_key_header: Option<String>,
    /// Paths that bypass consumers authentication entirely.
    /// Same glob syntax as `basicAuth.skipPaths`.
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    /// Shared JWT configuration for V3 consumer identification.
    ///
    /// When set, the Bearer token is validated once against the shared
    /// JWKS / secret, and the consumer is identified by matching the
    /// configured `usernameClaim` (default: `"sub"`) against
    /// `consumer.username`.
    ///
    /// This is the canonical Auth0 / Cognito / Keycloak pattern: the identity
    /// provider issues tokens with `sub = user-id`, and consumers are the list
    /// of allowed user IDs with per-user policies.
    ///
    /// Checked **before** per-consumer credentials (api_key / basicAuth / jwt).
    ///
    /// ```yaml
    /// consumers:
    ///   sharedJwt:
    ///     jwksUrl: "https://auth0.example.com/.well-known/jwks.json"
    ///     audience: ["my-api"]
    ///     issuer:   "https://auth0.example.com"
    ///   consumers:
    ///     - username: user-abc   # identified when jwt.sub == "user-abc"
    /// ```
    #[serde(rename = "sharedJwt", skip_serializing_if = "Option::is_none")]
    pub shared_jwt: Option<ConsumersSharedJwtConfig>,
}

/// Shared JWT configuration for V3 consumer identification.
///
/// All consumers in the list share one JWKS endpoint (or HS256 secret).
/// After token validation the `usernameClaim` value is matched against
/// `consumer.username` to determine which consumer made the request.
#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsumersSharedJwtConfig {
    /// Remote JWKS URL for RS256 / ES256 tokens.  Mutually exclusive with `secret`.
    #[serde(rename = "jwksUrl", skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    /// HS256 shared secret.  Mutually exclusive with `jwks_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Expected `aud` claim values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    /// Expected `iss` claim value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// JWT claim whose value is matched against `consumer.username`.
    ///
    /// Defaults to `"sub"` (the standard subject claim).  Use a different
    /// claim name when the identity provider stores the user identifier
    /// in a non-standard field (e.g., `"email"`, `"preferred_username"`).
    #[serde(rename = "usernameClaim", skip_serializing_if = "Option::is_none")]
    pub username_claim: Option<String>,
}

impl fmt::Debug for ConsumersSharedJwtConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsumersSharedJwtConfig")
            .field("jwks_url", &self.jwks_url)
            .field("secret", &self.secret.as_ref().map(|_| Redacted))
            .field("audience", &self.audience)
            .field("issuer", &self.issuer)
            .field("username_claim", &self.username_claim)
            .finish()
    }
}

/// A single named API consumer — a client with its own credentials and limits.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Consumer {
    /// Unique name injected as `X-Consumer-ID` after identification.
    pub username: String,
    /// API key credential.  The consumer is identified when the request
    /// carries this value in the `apiKeyHeader` (default: `x-api-key`).
    #[serde(rename = "apiKey", skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// HTTP Basic Auth credential.  The consumer is identified when the
    /// request carries `Authorization: Basic <base64(username:password)>` where
    /// the username matches `Consumer.username` and the password matches this.
    #[serde(rename = "basicAuth", skip_serializing_if = "Option::is_none")]
    pub basic_auth: Option<ConsumerBasicAuth>,
    /// JWT bearer-token credential (V2).
    ///
    /// The consumer is identified when the request carries a valid
    /// `Authorization: Bearer <token>` whose signature and claims are accepted
    /// by the configured secret / JWKS endpoint.
    ///
    /// Unlike `jwtAuth` at site level, this credential is checked
    /// independently inside `ConsumersGuard` — no separate `jwtAuth` block is
    /// required.
    ///
    /// ```yaml
    /// - username: service-a
    ///   jwt:
    ///     secret: "$SERVICE_A_SECRET"
    ///     issuer:  "https://auth.example.com"
    /// ```
    #[serde(rename = "jwt", skip_serializing_if = "Option::is_none")]
    pub jwt: Option<ConsumerJwtConfig>,
    /// Per-consumer rate limit, evaluated after identification.
    /// Independent of the site-level `rateLimit`.
    /// Key: `"consumer\0{username}"` (global across all IPs and sites for
    /// this consumer — see `rate_limit::consumer_key` and `CLAUDE.md`
    /// decision #14 in the root crate).
    ///
    /// **Type note (issue #114/#134, resolved by #114/#137 slice 1):** this
    /// used to be a separate, deliberately-duplicated `RateLimitConfig`
    /// defined in this crate — a Layer-1 crate couldn't depend on a type
    /// living in the root crate that depends on *it*. As of #137 slice 1
    /// (`crates/conduit-ratelimit`), `RateLimitConfig` lives in its own
    /// Layer-0 crate that both this crate and the root crate depend on (see
    /// the re-export a few lines below) — `conduit_auth_consumers::
    /// RateLimitConfig` and `crate::config::schema::RateLimitConfig` (root
    /// crate) are now the *same* type, not just field-compatible copies.
    /// This closed the SonarCloud "Duplicated Lines on New Code" finding the
    /// old duplicate caused.
    #[serde(rename = "rateLimit", skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitConfig>,
    /// Additional request headers to inject into the upstream request for this
    /// consumer (e.g., `X-Tier: premium`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
}

impl fmt::Debug for Consumer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Consumer")
            .field("username", &self.username)
            .field("api_key", &self.api_key.as_ref().map(|_| Redacted))
            // basic_auth/jwt are Option<_> of their own manually-redacting
            // Debug types below, so their own secret fields stay hidden too.
            .field("basic_auth", &self.basic_auth)
            .field("jwt", &self.jwt)
            .field("rate_limit", &self.rate_limit)
            .field("headers", &self.headers)
            .finish()
    }
}

/// Basic Auth password for a `Consumer`.  The username comes from
/// `Consumer.username`.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerBasicAuth {
    pub password: String,
}

impl fmt::Debug for ConsumerBasicAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsumerBasicAuth")
            .field("password", &Redacted)
            .finish()
    }
}

/// JWT credential for a `Consumer`.
///
/// A simplified subset of `JwtAuthConfig` (`crates/conduit-auth-jwt`)
/// without `skip_paths` or `jwks_refresh_secs` — those concerns belong to
/// the site-level JWT guard, not to the per-consumer credential.
#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerJwtConfig {
    /// HMAC-SHA256 secret for HS256 tokens.  Mutually exclusive with `jwks_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Remote JWKS URL for RS256 / ES256 tokens.  Mutually exclusive with `secret`.
    #[serde(rename = "jwksUrl", skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    /// Expected `aud` claim values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    /// Expected `iss` claim value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
}

impl fmt::Debug for ConsumerJwtConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsumerJwtConfig")
            .field("secret", &self.secret.as_ref().map(|_| Redacted))
            .field("jwks_url", &self.jwks_url)
            .field("audience", &self.audience)
            .field("issuer", &self.issuer)
            .finish()
    }
}

/// Rate-limit config — moved to `crates/conduit-ratelimit` (issue #114/#137,
/// slice 1), re-exported here so `conduit_auth_consumers::RateLimitConfig`
/// keeps resolving. See [`Consumer::rate_limit`]'s doc comment for the full
/// history (this used to be a deliberate, temporary duplicate of the root
/// crate's own type).
pub use conduit_ratelimit::RateLimitConfig;

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn shared_jwt_config_secret_is_redacted() {
        let cfg = ConsumersSharedJwtConfig {
            jwks_url: None,
            secret: Some("shared-jwt-hmac-secret".to_string()),
            audience: None,
            issuer: None,
            username_claim: Some("sub".to_string()),
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("shared-jwt-hmac-secret"), "got: {debug}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("sub"),
            "non-secret field must still print: {debug}"
        );
    }

    #[test]
    fn consumer_api_key_is_redacted_but_username_is_not() {
        let consumer = Consumer {
            username: "alice".to_string(),
            api_key: Some("alices-raw-api-key".to_string()),
            basic_auth: None,
            jwt: None,
            rate_limit: None,
            headers: None,
        };
        let debug = format!("{consumer:?}");
        assert!(!debug.contains("alices-raw-api-key"), "got: {debug}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("alice"),
            "username isn't secret and should still print: {debug}"
        );
    }

    #[test]
    fn consumer_nested_basic_auth_and_jwt_secrets_are_redacted_too() {
        let consumer = Consumer {
            username: "bob".to_string(),
            api_key: None,
            basic_auth: Some(ConsumerBasicAuth {
                password: "bobs-plaintext-password".to_string(),
            }),
            jwt: Some(ConsumerJwtConfig {
                secret: Some("bobs-jwt-secret".to_string()),
                jwks_url: None,
                audience: None,
                issuer: None,
            }),
            rate_limit: None,
            headers: None,
        };
        let debug = format!("{consumer:?}");
        assert!(!debug.contains("bobs-plaintext-password"), "got: {debug}");
        assert!(!debug.contains("bobs-jwt-secret"), "got: {debug}");
    }

    #[test]
    fn consumer_basic_auth_password_is_redacted() {
        let cfg = ConsumerBasicAuth {
            password: "hunter2".to_string(),
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("hunter2"), "got: {debug}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
    }

    #[test]
    fn consumer_jwt_config_secret_is_redacted() {
        let cfg = ConsumerJwtConfig {
            secret: Some("per-consumer-jwt-secret".to_string()),
            jwks_url: None,
            audience: None,
            issuer: Some("https://issuer.example.com".to_string()),
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("per-consumer-jwt-secret"), "got: {debug}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("issuer.example.com"),
            "non-secret field must still print: {debug}"
        );
    }
}
