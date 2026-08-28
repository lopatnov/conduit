use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
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

/// A single named API consumer — a client with its own credentials and limits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Key: `"consumer:{username}"` (global across all IPs for this consumer).
    ///
    /// **Type note (issue #114/#134):** this is [`RateLimitConfig`], a struct
    /// defined *in this crate*, not the root crate's own
    /// `crate::config::schema::RateLimitConfig` (used by site-level and
    /// route-level `rateLimit`) — even though the two are field-for-field
    /// identical today. A Layer-1 feature crate can't depend on a type that
    /// lives in the root crate that depends on *it* (that's exactly the
    /// circular coupling the workspace split exists to avoid), and
    /// `RateLimitConfig` itself hasn't been extracted yet — that's
    /// [#137](https://github.com/lopatnov/conduit/issues/137)'s job
    /// (`conduit-ratelimit`). This is a deliberate, temporary duplication of
    /// a small, plain, `serde`-only data struct (not any validation or
    /// rate-limiting *logic* — see `validate_rate_limit` in the root crate's
    /// `src/config/validate.rs`, which takes primitive fields rather than a
    /// concrete `RateLimitConfig` specifically so the two call sites — site/
    /// route-level using the root's type, this one using this crate's type —
    /// don't need two copies of the actual validation rules). Once #137
    /// extracts `conduit-ratelimit`, both this copy and the root's copy
    /// should be replaced by a shared dependency on that crate.
    #[serde(rename = "rateLimit", skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitConfig>,
    /// Additional request headers to inject into the upstream request for this
    /// consumer (e.g., `X-Tier: premium`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
}

/// Basic Auth password for a `Consumer`.  The username comes from
/// `Consumer.username`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerBasicAuth {
    pub password: String,
}

/// JWT credential for a `Consumer`.
///
/// A simplified subset of `JwtAuthConfig` (`crates/conduit-auth-jwt`)
/// without `skip_paths` or `jwks_refresh_secs` — those concerns belong to
/// the site-level JWT guard, not to the per-consumer credential.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
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

/// A deliberate, temporary duplicate of the root crate's
/// `crate::config::schema::RateLimitConfig` — see the doc comment on
/// [`Consumer::rate_limit`] for the full reasoning (issue #114/#134,
/// consolidation tracked as #137).
///
/// Field-for-field identical (including `serde` renames) to the root's
/// type, so `consumers.consumers[].rateLimit` accepts exactly the same JSON/
/// YAML shape as `sites[].rateLimit` / `proxy.*.rateLimit` always has.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitConfig {
    pub window_secs: u64,
    pub limit: u64,
    /// Optional burst capacity on top of `limit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub burst: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    /// Backend store for the rate limiter ("memory" / "redis://…" / "rediss://…").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
    /// Dry-run mode — log violations but allow requests through.
    #[serde(rename = "dryRun", skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}
