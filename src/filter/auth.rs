use base64::Engine as _;
use pingora_proxy::Session;
use subtle::ConstantTimeEq as _;

#[cfg(feature = "jwt")]
use crate::config::schema::JwtAuthConfig;
use crate::config::schema::{ApiKeyConfig, BasicAuthConfig};
#[cfg(feature = "consumers")]
use crate::config::schema::{Consumer, ConsumersConfig};
#[cfg(feature = "jwt")]
use crate::filter::jwt;

/// Constant-time byte-string equality for credential comparisons.
///
/// Using `==` for password/API-key matching leaks timing information: the
/// comparison short-circuits on the first differing byte, letting an attacker
/// determine how many leading bytes of a guessed credential are correct.
///
/// This helper always inspects every byte regardless of where a difference
/// occurs.  Length is checked first (NOT constant-time) — a length mismatch
/// still reveals the correct length, but the set of lengths that need to be
/// tried is already known (API keys are fixed-length by convention).
#[inline]
fn ct_eq_str(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Build a [`JwtAuthConfig`] from raw credential parts.
///
/// Used by both the V2 per-consumer JWT check and the V3 sharedJwt check to
/// avoid duplicating the struct literal in two places.
#[cfg(feature = "jwt")]
fn build_jwt_auth_cfg(
    secret: Option<String>,
    jwks_url: Option<String>,
    audience: Option<Vec<String>>,
    issuer: Option<String>,
) -> JwtAuthConfig {
    JwtAuthConfig {
        secret,
        jwks_url,
        jwks_refresh_secs: None, // use JWKS default TTL (3600 s)
        audience,
        issuer,
        skip_paths: None, // skip_paths handled at ConsumersConfig level
    }
}

/// Result of a Basic Auth credential check.
pub enum BasicAuthResult {
    Allowed,
    Denied { challenge: bool, realm: String },
}

// Layer-0 vocabulary (#114/#126) -- shared by every skipPaths config field
// (JWT, ForwardAuth, Consumers).
pub use conduit_core::filter::path::is_path_skipped;

/// Validate a raw `Authorization` header value against a user map.
///
/// Extracted for unit testability — `check_basic_auth` calls this after reading
/// the header value from the Pingora session.
///
/// RFC 7235: the scheme token is case-insensitive.
pub(crate) fn check_credentials(
    users: &indexmap::IndexMap<String, String>,
    auth_header: &str,
    challenge: bool,
    realm: String,
) -> BasicAuthResult {
    let (scheme, rest) = match auth_header.split_once(' ') {
        Some(pair) => pair,
        None => return BasicAuthResult::Denied { challenge, realm },
    };
    if !scheme.eq_ignore_ascii_case("Basic") {
        return BasicAuthResult::Denied { challenge, realm };
    }
    let b64 = rest.trim();

    let decoded = match base64::engine::general_purpose::STANDARD.decode(b64) {
        Ok(b) => b,
        Err(_) => return BasicAuthResult::Denied { challenge, realm },
    };

    let user_pass = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return BasicAuthResult::Denied { challenge, realm },
    };

    let (username, password) = match user_pass.split_once(':') {
        Some((u, p)) => (u, p),
        None => return BasicAuthResult::Denied { challenge, realm },
    };

    match users.get(username) {
        Some(expected) if ct_eq_str(expected, password) => BasicAuthResult::Allowed,
        _ => BasicAuthResult::Denied { challenge, realm },
    }
}

/// Validate the `Authorization: Basic …` header against the configured user map.
pub fn check_basic_auth(cfg: &BasicAuthConfig, session: &Session) -> BasicAuthResult {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return BasicAuthResult::Allowed;
    }

    let realm = cfg.realm.as_deref().unwrap_or("Restricted").to_owned();
    let challenge = cfg.challenge.unwrap_or(true);

    let auth_value = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    check_credentials(&cfg.users, auth_value, challenge, realm)
}

/// Check whether the request carries a valid API key.
///
/// The key is read from the header named by `cfg.header` (default: `x-api-key`).
/// Returns `true` if the request is allowed.
pub fn check_api_key(cfg: &ApiKeyConfig, session: &Session) -> bool {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return true;
    }

    let header_name = cfg.header.as_deref().unwrap_or("x-api-key");
    let provided = session
        .req_header()
        .headers
        .get(header_name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Compare against ALL keys without early exit — prevents an attacker from
    // inferring the key's position in the list via response-time differences.
    cfg.keys
        .iter()
        .fold(false, |found, k| found | ct_eq_str(k, provided))
}

// ── Consumer model ─────────────────────────────────────────────────────────

/// Attempt to identify the request's consumer from the configured list.
///
/// Evaluation order: consumers are checked in declaration order; the **first
/// matching** consumer wins.  A consumer can use one of two credential types:
///
/// - **API key** — value in the `apiKeyHeader` request header (default:
///   `x-api-key`).  Compared with [`ct_eq_str`] (constant-time).
/// - **Basic Auth** — `Authorization: Basic <base64(username:password)>` where
///   the username must equal `consumer.username`.
///
/// Returns `None` when no consumer matches (caller should return 401).
#[cfg(feature = "consumers")]
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
#[cfg(all(feature = "consumers", feature = "jwt"))]
fn check_shared_jwt_consumer<'a>(
    shared: &crate::config::schema::ConsumersSharedJwtConfig,
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

    let (jwt_result, maybe_claims) = jwt::check_jwt_extracting(&jwt_cfg, "", auth_hdr);
    if let jwt::JwtCheckResult::Allowed = jwt_result {
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
#[cfg(feature = "consumers")]
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
        if let jwt::JwtCheckResult::Allowed = jwt::check_jwt(&jwt_cfg, "", auth_hdr) {
            return true;
        }
    }

    false
}

/// Validate `Authorization: Basic <b64>` against a consumer's username and password.
#[cfg(feature = "consumers")]
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
    use indexmap::IndexMap;

    fn users(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(u, p)| (u.to_string(), p.to_string()))
            .collect()
    }

    fn denied(r: BasicAuthResult) -> bool {
        matches!(r, BasicAuthResult::Denied { .. })
    }

    // ── check_credentials ─────────────────────────────────────────────────────

    #[test]
    fn credentials_missing_header_denied() {
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(&u, "", true, "R".to_owned())));
    }

    #[test]
    fn credentials_wrong_scheme_denied() {
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(
            &u,
            "Bearer token",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_invalid_base64_denied() {
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(
            &u,
            "Basic not!valid!b64",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_no_colon_in_decoded_denied() {
        // base64("nocolon") = "bm9jb2xvbg=="
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(
            &u,
            "Basic bm9jb2xvbg==",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_wrong_password_denied() {
        let u = users(&[("admin", "secret")]);
        // base64("admin:wrong") = "YWRtaW46d3Jvbmc="
        assert!(denied(check_credentials(
            &u,
            "Basic YWRtaW46d3Jvbmc=",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_correct_password_allowed() {
        let u = users(&[("admin", "secret")]);
        // base64("admin:secret") = "YWRtaW46c2VjcmV0"
        assert!(matches!(
            check_credentials(&u, "Basic YWRtaW46c2VjcmV0", true, "R".to_owned()),
            BasicAuthResult::Allowed
        ));
    }

    #[test]
    fn credentials_scheme_case_insensitive() {
        let u = users(&[("u", "p")]);
        // base64("u:p") = "dTpw"
        assert!(matches!(
            check_credentials(&u, "BASIC dTpw", false, "R".to_owned()),
            BasicAuthResult::Allowed
        ));
    }

    #[test]
    fn credentials_unknown_user_denied() {
        let u = users(&[("admin", "secret")]);
        // base64("other:secret") = "b3RoZXI6c2VjcmV0"
        assert!(denied(check_credentials(
            &u,
            "Basic b3RoZXI6c2VjcmV0",
            true,
            "R".to_owned()
        )));
    }

    // ── path_matches / is_path_skipped moved to conduit_core::filter::path ─────

    // ── ct_eq_str (timing-safe comparison) ────────────────────────────────────

    #[test]
    fn ct_eq_identical_strings() {
        assert!(ct_eq_str("secret", "secret"));
    }

    #[test]
    fn ct_eq_different_strings() {
        assert!(!ct_eq_str("secret", "wrong"));
    }

    #[test]
    fn ct_eq_different_lengths() {
        assert!(!ct_eq_str("short", "longer-value"));
        assert!(!ct_eq_str("longer-value", "short"));
    }

    #[test]
    fn ct_eq_empty_strings() {
        assert!(ct_eq_str("", ""));
    }

    #[test]
    fn ct_eq_empty_vs_nonempty() {
        assert!(!ct_eq_str("", "x"));
        assert!(!ct_eq_str("x", ""));
    }

    #[test]
    fn ct_eq_unicode_strings() {
        // Unicode strings — length in bytes matters, not chars.
        assert!(ct_eq_str("café", "café"));
        assert!(!ct_eq_str("café", "cafe"));
    }

    // ── check_credentials extended cases ─────────────────────────────────────

    #[test]
    fn credentials_empty_user_map_denied() {
        // No users configured → always denied.
        let users = indexmap::IndexMap::new();
        assert!(denied(check_credentials(
            &users,
            "Basic YWRtaW46c2VjcmV0",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_realm_included_in_denial() {
        let u = users(&[("admin", "secret")]);
        let result = check_credentials(&u, "", true, "MyRealm".to_owned());
        if let BasicAuthResult::Denied { realm, .. } = result {
            assert_eq!(realm, "MyRealm");
        } else {
            panic!("expected Denied");
        }
    }

    #[test]
    fn credentials_challenge_false_propagated() {
        let u = users(&[("admin", "secret")]);
        let result = check_credentials(&u, "", false, "R".to_owned());
        if let BasicAuthResult::Denied { challenge, .. } = result {
            assert!(!challenge);
        } else {
            panic!("expected Denied");
        }
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
}
