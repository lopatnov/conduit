//! Authentication and rate limiting: basic auth, API keys, rate limit, consumers, JWT,
//! forward auth.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::Redacted;

// ── Auth & rate limiting ───────────────────────────────────────────────────

/// Rate-limit config — moved to `crates/conduit-ratelimit` (issue #114/#137,
/// slice 1), re-exported here so every existing `crate::config::schema::
/// RateLimitConfig` path keeps resolving. Shared, byte-identical shape with
/// `conduit_auth_consumers::RateLimitConfig` — as of #137 slice 1 they're the
/// *same* type, not just field-compatible duplicates (issue #114/#134's
/// SonarCloud duplication finding is resolved by this re-export).
pub use conduit_ratelimit::RateLimitConfig;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BasicAuthConfig {
    pub users: IndexMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

impl fmt::Debug for BasicAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Usernames (map keys) aren't secret; passwords (values) are —
        // redact only the values, keeping the user list itself visible.
        let users: IndexMap<&str, Redacted> =
            self.users.keys().map(|k| (k.as_str(), Redacted)).collect();
        f.debug_struct("BasicAuthConfig")
            .field("users", &users)
            .field("challenge", &self.challenge)
            .field("realm", &self.realm)
            .field("skip_paths", &self.skip_paths)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyConfig {
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}

impl fmt::Debug for ApiKeyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyConfig")
            .field("keys", &vec![Redacted; self.keys.len()])
            .field("header", &self.header)
            .field("skip_paths", &self.skip_paths)
            .finish()
    }
}

// ── Consumer model ─────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-auth-consumers` (issue #114/#134) — this
/// is a facade re-export so `crate::config::schema::{ConsumersConfig,
/// ConsumersSharedJwtConfig, Consumer, ConsumerBasicAuth, ConsumerJwtConfig}`
/// keep resolving to the same types at the same location for every existing
/// call site/test.
///
/// Named-consumer authentication: credentials and per-consumer policies
/// stored per-consumer rather than per-route. When a request matches a
/// consumer's credentials:
/// 1. The consumer's username is injected as `X-Consumer-ID` (or `idHeader`)
///    into the upstream request.
/// 2. Any per-consumer `headers` are also injected.
/// 3. Per-consumer `rateLimit` is applied (independent of the site rate limit).
///
/// Requests that don't match any consumer receive 401 Unauthorized.
///
/// **Note:** unlike every sibling facade in this file, `ConsumersGuard`
/// itself is *not* re-exported here — it stays in this crate's own
/// `src/filter/chain.rs` (see `conduit_auth_consumers`'s own `src/lib.rs`
/// doc comment for why: `ConsumersGuard` is a `Session`-coupled request-chain
/// guard, same category as `IpGuard`/`CorsPreflight` staying out of their
/// own Layer-0 crates — chain assembly and guard ordering stay in the root
/// crate per `CLAUDE.md` decision #20, regardless of where the *types* it
/// carries live. As of #114/#137 slice 1, `RateLimiter` itself now lives in
/// `conduit-ratelimit`, re-exported via `crate::filter::rate_limit`). Only
/// the config types and the pure `identify::identify_consumer`
/// identification logic moved to `conduit-auth-consumers`.
pub use conduit_auth_consumers::{
    Consumer, ConsumerBasicAuth, ConsumerJwtConfig, ConsumersConfig, ConsumersSharedJwtConfig,
};

// ── JWT auth ───────────────────────────────────────────────────────────────

/// JWT bearer-token validation configuration.
///
/// At least one of `secret` or `jwks_url` must be present.
///
/// Extracted into `crates/conduit-auth-jwt` (issue #114/#133) — this is a
/// facade re-export so `crate::config::schema::JwtAuthConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
pub use conduit_auth_jwt::JwtAuthConfig;

// ── Forward Auth ──────────────────────────────────────────────────────────

/// Extracted into `crates/conduit-auth-forward` (issue #114/#134) — this is
/// a facade re-export so `crate::config::schema::ForwardAuthConfig` keeps
/// resolving to the same type at the same location for every existing call
/// site/test.
///
/// External authentication service integration. The request is forwarded to
/// the auth URL before reaching the upstream. The auth service communicates
/// its decision via HTTP status:
/// - 2xx → allow; copy `responseHeaders` to upstream request
/// - 4xx / 5xx → deny; return the auth service's status to the client
pub use conduit_auth_forward::ForwardAuthConfig;
