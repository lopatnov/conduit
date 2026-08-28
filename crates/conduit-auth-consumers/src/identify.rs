//! Consumer identification logic — pure, self-contained credential checks.
//!
//! **Does not include [`crate::config`]'s owner, `ConsumersGuard` itself** —
//! see this crate's `src/lib.rs` doc comment for why: `ConsumersGuard`
//! additionally needs the root crate's not-yet-extracted `RateLimiter` for
//! its per-consumer rate limit step, so the guard's `RequestFilter` impl
//! stays in the root crate's `src/filter/chain.rs`. Only the identification
//! step — everything in this module — is self-contained enough to move.

use base64::Engine as _;
use conduit_core::util::crypto::ct_eq_str;
use pingora_proxy::Session;

use crate::config::{Consumer, ConsumersConfig};

/// Build a [`conduit_auth_jwt::JwtAuthConfig`] from raw credential parts.
///
/// Used by both the V2 per-consumer JWT check and the V3 sharedJwt check to
/// avoid duplicating the struct literal in two places.
#[cfg(feature = "jwt")]
fn build_jwt_auth_cfg(
    secret: Option<String>,
    jwks_url: Option<String>,
    audience: Option<Vec<String>>,
    issuer: Option<String>,
) -> conduit_auth_jwt::JwtAuthConfig {
    conduit_auth_jwt::JwtAuthConfig {
        secret,
        jwks_url,
        jwks_refresh_secs: None, // use JWKS default TTL (3600 s)
        audience,
        issuer,
        skip_paths: None, // skip_paths handled at ConsumersConfig level
    }
}

/// Attempt to identify the request's consumer from the configured list.
///
/// Evaluation order: consumers are checked in declaration order; the **first
/// matching** consumer wins.  A consumer can use one of four credential types:
///
/// - **API key** — value in the `apiKeyHeader` request header (default:
///   `x-api-key`).  Compared with [`ct_eq_str`] (constant-time).
/// - **Basic Auth** — `Authorization: Basic <base64(username:password)>` where
///   the username must equal `consumer.username`.
/// - **Per-consumer JWT** (V2, feature `jwt`) — a bearer token validated
///   against that consumer's own `secret`/`jwksUrl`.
/// - **Shared JWT** (V3, feature `jwt`) — one JWKS/secret shared across all
///   consumers, identified by a claim (default `sub`) matching
///   `consumer.username`; checked once up front before per-consumer checks.
///
/// Returns `None` when no consumer matches (caller should return 401).
pub fn identify_consumer<'a>(cfg: &'a ConsumersConfig, session: &Session) -> Option<&'a Consumer> {
    let api_key_header = cfg.api_key_header.as_deref().unwrap_or("x-api-key");

    // ── V3: Shared JWT — validate once, identify by claim value ───────────────
    #[cfg(feature = "jwt")]
    if let Some(ref shared) = cfg.shared_jwt {
        if let Some(consumer) = check_shared_jwt_consumer(shared, &cfg.consumers, session) {
            return Some(consumer);
        }
    }

    cfg.consumers
        .iter()
        .find(|consumer| check_consumer_credentials(consumer, api_key_header, session))
}

/// Try to identify a consumer via the shared JWT (V3 / Auth0 / Cognito pattern).
///
/// Returns `Some(&Consumer)` when the JWT is valid and a matching consumer is
/// found by the configured `username_claim` (default: `"sub"`).
#[cfg(feature = "jwt")]
fn check_shared_jwt_consumer<'a>(
    shared: &crate::config::ConsumersSharedJwtConfig,
    consumers: &'a [Consumer],
    session: &Session,
) -> Option<&'a Consumer> {
    let jwt_cfg = build_jwt_auth_cfg(
        shared.secret.clone(),
        shared.jwks_url.clone(),
        shared.audience.clone(),
        shared.issuer.clone(),
    );
    let auth_hdr = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let (jwt_result, maybe_claims) = conduit_auth_jwt::check_jwt_extracting(&jwt_cfg, "", auth_hdr);
    if let conduit_auth_jwt::JwtCheckResult::Allowed = jwt_result {
        if let Some(claims) = maybe_claims {
            let claim_key = shared.username_claim.as_deref().unwrap_or("sub");
            if let Some(serde_json::Value::String(sub)) = claims.get(claim_key) {
                let sub = sub.clone();
                return consumers.iter().find(|c| c.username == sub);
            }
        }
    }
    None
}

/// Check whether a request matches a single consumer's credentials.
///
/// Returns `true` when any credential matches (API key, Basic Auth, or JWT V2).
fn check_consumer_credentials(
    consumer: &Consumer,
    api_key_header: &str,
    session: &Session,
) -> bool {
    // ── API key check ─────────────────────────────────────────────────────────
    if let Some(ref expected_key) = consumer.api_key {
        if let Some(provided) = session
            .req_header()
            .headers
            .get(api_key_header)
            .and_then(|v| v.to_str().ok())
        {
            if ct_eq_str(provided, expected_key.as_str()) {
                return true;
            }
        }
    }

    // ── Basic Auth check ──────────────────────────────────────────────────────
    if let Some(ref basic) = consumer.basic_auth {
        if check_consumer_basic(&consumer.username, &basic.password, session) {
            return true;
        }
    }

    // ── JWT check (V2) — requires `jwt` feature ───────────────────────────────
    #[cfg(feature = "jwt")]
    if let Some(ref consumer_jwt) = consumer.jwt {
        let jwt_cfg = build_jwt_auth_cfg(
            consumer_jwt.secret.clone(),
            consumer_jwt.jwks_url.clone(),
            consumer_jwt.audience.clone(),
            consumer_jwt.issuer.clone(),
        );
        let auth_hdr = session
            .req_header()
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok());
        if let conduit_auth_jwt::JwtCheckResult::Allowed =
            conduit_auth_jwt::check_jwt(&jwt_cfg, "", auth_hdr)
        {
            return true;
        }
    }

    false
}

/// Validate `Authorization: Basic <b64>` against a consumer's username and password.
fn check_consumer_basic(username: &str, password: &str, session: &Session) -> bool {
    let auth_header = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let (scheme, encoded) = auth_header.split_once(' ').unwrap_or(("", ""));
    if !scheme.eq_ignore_ascii_case("Basic") {
        return false;
    }
    let encoded = encoded.trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok();
    let decoded = match decoded {
        Some(d) => d,
        None => return false,
    };
    let decoded_str = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Format: "username:password" — split only on the first colon.
    let (provided_user, provided_pass) = match decoded_str.split_once(':') {
        Some(pair) => pair,
        None => return false,
    };

    // Evaluate both comparisons unconditionally (bitwise AND, not &&) to prevent
    // username-validity leakage via timing: `&&` would skip the password check
    // when the username fails, creating a measurable timing difference.
    let user_ok = ct_eq_str(provided_user, username);
    let pass_ok = ct_eq_str(provided_pass, password);
    user_ok & pass_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConsumerBasicAuth;

    fn consumer_with_api_key(username: &str, key: &str) -> Consumer {
        Consumer {
            username: username.to_owned(),
            api_key: Some(key.to_owned()),
            basic_auth: None,
            jwt: None,
            rate_limit: None,
            headers: None,
        }
    }

    fn consumer_with_basic(username: &str, password: &str) -> Consumer {
        Consumer {
            username: username.to_owned(),
            api_key: None,
            basic_auth: Some(ConsumerBasicAuth {
                password: password.to_owned(),
            }),
            jwt: None,
            rate_limit: None,
            headers: None,
        }
    }

    /// Build a real [`pingora_proxy::Session`] with a parsed GET request
    /// already read off the wire (see `conduit-faults`/`conduit-auth-jwt`'s
    /// own `guard.rs` test helper — same pattern, reused here since
    /// `identify_consumer` et al. take `&Session` directly).
    async fn session_with_headers(raw: &[u8]) -> pingora_proxy::Session {
        use tokio::io::AsyncWriteExt;
        let (server_side, mut client_side) = tokio::io::duplex(4096);
        client_side.write_all(raw).await.unwrap();
        let stream: pingora_core::protocols::Stream = Box::new(server_side);
        let mut session = pingora_proxy::Session::new_h1(stream);
        session
            .as_downstream_mut()
            .read_request()
            .await
            .expect("read_request");
        session
    }

    // ── identify_consumer / check_consumer_credentials ───────────────────────

    #[tokio::test]
    async fn identify_consumer_by_api_key() {
        let session = session_with_headers(
            b"GET /api HTTP/1.1\r\nHost: test\r\nx-api-key: secret-key\r\n\r\n",
        )
        .await;
        let cfg = ConsumersConfig {
            consumers: vec![consumer_with_api_key("alice", "secret-key")],
            ..Default::default()
        };
        let found = identify_consumer(&cfg, &session);
        assert_eq!(found.map(|c| c.username.as_str()), Some("alice"));
    }

    #[tokio::test]
    async fn identify_consumer_no_match_returns_none() {
        let session = session_with_headers(b"GET /api HTTP/1.1\r\nHost: test\r\n\r\n").await;
        let cfg = ConsumersConfig {
            consumers: vec![consumer_with_api_key("alice", "secret-key")],
            ..Default::default()
        };
        assert!(identify_consumer(&cfg, &session).is_none());
    }

    #[tokio::test]
    async fn identify_consumer_by_basic_auth() {
        // base64("bob:pw") = "Ym9iOnB3"
        let session = session_with_headers(
            b"GET /api HTTP/1.1\r\nHost: test\r\nAuthorization: Basic Ym9iOnB3\r\n\r\n",
        )
        .await;
        let cfg = ConsumersConfig {
            consumers: vec![consumer_with_basic("bob", "pw")],
            ..Default::default()
        };
        let found = identify_consumer(&cfg, &session);
        assert_eq!(found.map(|c| c.username.as_str()), Some("bob"));
    }

    #[tokio::test]
    async fn check_consumer_basic_wrong_password_denied() {
        // base64("bob:wrong") = "Ym9iOndyb25n"
        let session = session_with_headers(
            b"GET /api HTTP/1.1\r\nHost: test\r\nAuthorization: Basic Ym9iOndyb25n\r\n\r\n",
        )
        .await;
        assert!(!check_consumer_basic("bob", "pw", &session));
    }

    #[tokio::test]
    async fn check_consumer_basic_wrong_scheme_denied() {
        let session = session_with_headers(
            b"GET /api HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer x\r\n\r\n",
        )
        .await;
        assert!(!check_consumer_basic("bob", "pw", &session));
    }

    // ── build_jwt_auth_cfg ────────────────────────────────────────────────────

    #[test]
    #[cfg(feature = "jwt")]
    fn build_jwt_auth_cfg_with_secret() {
        let cfg = build_jwt_auth_cfg(Some("my-secret".to_owned()), None, None, None);
        assert_eq!(cfg.secret.as_deref(), Some("my-secret"));
        assert!(cfg.jwks_url.is_none());
        assert!(cfg.audience.is_none());
        assert!(cfg.issuer.is_none());
        assert!(cfg.skip_paths.is_none());
        assert!(cfg.jwks_refresh_secs.is_none());
    }

    #[test]
    #[cfg(feature = "jwt")]
    fn build_jwt_auth_cfg_with_jwks_url() {
        let cfg = build_jwt_auth_cfg(
            None,
            Some("https://auth.example.com/.well-known/jwks.json".to_owned()),
            Some(vec!["my-app".to_owned()]),
            Some("https://auth.example.com/".to_owned()),
        );
        assert!(cfg.secret.is_none());
        assert_eq!(
            cfg.jwks_url.as_deref(),
            Some("https://auth.example.com/.well-known/jwks.json")
        );
        assert_eq!(
            cfg.audience.as_deref(),
            Some(["my-app".to_owned()].as_slice())
        );
        assert_eq!(cfg.issuer.as_deref(), Some("https://auth.example.com/"));
    }

    #[cfg(feature = "jwt")]
    #[tokio::test]
    async fn identify_consumer_by_shared_jwt_sub_claim() {
        let secret = "shared-jwt-secret";
        let claims = serde_json::json!({ "sub": "carol", "exp": exp_future() });
        let token = hs256_token(secret, claims);
        let raw =
            format!("GET /api HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer {token}\r\n\r\n");
        let session = session_with_headers(raw.as_bytes()).await;

        let cfg = ConsumersConfig {
            consumers: vec![Consumer {
                username: "carol".to_owned(),
                api_key: None,
                basic_auth: None,
                jwt: None,
                rate_limit: None,
                headers: None,
            }],
            shared_jwt: Some(crate::config::ConsumersSharedJwtConfig {
                secret: Some(secret.to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let found = identify_consumer(&cfg, &session);
        assert_eq!(found.map(|c| c.username.as_str()), Some("carol"));
    }

    #[cfg(feature = "jwt")]
    fn hs256_token(secret: &str, claims: serde_json::Value) -> String {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let key = EncodingKey::from_secret(secret.as_bytes());
        encode(&Header::new(Algorithm::HS256), &claims, &key).unwrap()
    }

    #[cfg(feature = "jwt")]
    fn exp_future() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600
    }
}
