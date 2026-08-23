//! HS256 / core unit tests for the JWT guard.
//!
//! JWKS (RS256/ES256) coverage lives in the `jwks` submodule.

use super::*;
use crate::config::schema::JwtAuthConfig;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;

mod jwks;

fn make_hs256_token(secret: &str, claims: serde_json::Value) -> String {
    let key = EncodingKey::from_secret(secret.as_bytes());
    encode(&Header::new(Algorithm::HS256), &claims, &key).unwrap()
}

fn exp_future() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

#[test]
fn missing_auth_header_denied() {
    let cfg = JwtAuthConfig {
        secret: Some("s3cr3t".into()),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", None),
        JwtCheckResult::Denied { .. }
    ));
}

#[test]
fn non_bearer_scheme_denied() {
    let cfg = JwtAuthConfig {
        secret: Some("s3cr3t".into()),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some("Basic dXNlcjpwYXNz")),
        JwtCheckResult::Denied { .. }
    ));
}

#[test]
fn valid_hs256_token_allowed() {
    let secret = "test-secret";
    let token = make_hs256_token(secret, json!({ "sub": "user", "exp": exp_future() }));
    let cfg = JwtAuthConfig {
        secret: Some(secret.into()),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Allowed
    ));
}

#[test]
fn wrong_secret_denied() {
    let token = make_hs256_token("correct-secret", json!({ "sub": "u", "exp": exp_future() }));
    let cfg = JwtAuthConfig {
        secret: Some("wrong-secret".into()),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Denied { .. }
    ));
}

#[test]
fn expired_token_denied() {
    // Expire 120 seconds in the past — beyond jsonwebtoken's default 60 s leeway.
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 120;
    let token = make_hs256_token("secret", json!({ "sub": "u", "exp": exp }));
    let cfg = JwtAuthConfig {
        secret: Some("secret".into()),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Denied { .. }
    ));
}

#[test]
fn skip_path_bypasses_validation() {
    // No valid token but path is skipped.
    let cfg = JwtAuthConfig {
        secret: Some("secret".into()),
        skip_paths: Some(vec!["/health".into()]),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/health", None),
        JwtCheckResult::Allowed
    ));
}

#[test]
fn skip_path_glob_bypasses_validation() {
    let cfg = JwtAuthConfig {
        secret: Some("secret".into()),
        skip_paths: Some(vec!["/public/**".into()]),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/public/images/logo.png", None),
        JwtCheckResult::Allowed
    ));
}

#[test]
fn misconfigured_no_secret_or_jwks_denied() {
    // Neither secret nor jwks_url — should be caught by validate, but
    // at runtime it degrades gracefully to Denied.
    let cfg = JwtAuthConfig::default();
    let token = make_hs256_token("anything", json!({ "sub": "u", "exp": exp_future() }));
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Denied { .. }
    ));
}

#[test]
fn issuer_mismatch_denied() {
    let secret = "secret";
    let token = make_hs256_token(
        secret,
        json!({ "sub": "u", "iss": "other", "exp": exp_future() }),
    );
    let cfg = JwtAuthConfig {
        secret: Some(secret.into()),
        issuer: Some("expected-issuer".into()),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Denied { .. }
    ));
}

// ── extract_claims_unchecked / template substitution ───────────────────────

#[test]
fn extract_claims_returns_sub() {
    let secret = "claim-test-secret";
    let token = make_hs256_token(
        secret,
        json!({ "sub": "user42", "email": "u@example.com", "exp": exp_future() }),
    );
    let claims = extract_claims_unchecked(&token).expect("claims should be extracted");
    assert_eq!(
        claims.get("sub").and_then(|v| v.as_str()),
        Some("user42"),
        "sub claim must be extractable"
    );
    assert_eq!(
        claims.get("email").and_then(|v| v.as_str()),
        Some("u@example.com")
    );
}

#[test]
fn expand_jwt_templates_sub() {
    use crate::proxy::request_phase::expand_jwt_templates;
    let mut claims = std::collections::HashMap::new();
    claims.insert("sub".to_string(), serde_json::json!("alice"));
    claims.insert("role".to_string(), serde_json::json!("admin"));

    assert_eq!(
        expand_jwt_templates("{{ jwt.sub }}", &Some(claims.clone())),
        "alice"
    );
    assert_eq!(
        expand_jwt_templates(
            "user={{ jwt.sub }},role={{ jwt.role }}",
            &Some(claims.clone())
        ),
        "user=alice,role=admin"
    );
    // Unknown claim → empty string.
    assert_eq!(expand_jwt_templates("{{ jwt.unknown }}", &Some(claims)), "");
    // No claims → empty string.
    assert_eq!(expand_jwt_templates("{{ jwt.sub }}", &None), "");
}

// ── check_jwt_extracting ──────────────────────────────────────────────────

#[test]
fn extracting_success_returns_allowed_and_claims() {
    let secret = "extract-secret";
    let token = make_hs256_token(
        secret,
        json!({ "sub": "extract-user", "role": "admin", "exp": exp_future() }),
    );
    let cfg = JwtAuthConfig {
        secret: Some(secret.into()),
        ..Default::default()
    };
    let (result, claims) = check_jwt_extracting(&cfg, "/api", Some(&format!("Bearer {token}")));
    assert!(matches!(result, JwtCheckResult::Allowed));
    let claims = claims.expect("claims must be present on success");
    assert_eq!(
        claims.get("sub").and_then(|v| v.as_str()),
        Some("extract-user")
    );
    assert_eq!(claims.get("role").and_then(|v| v.as_str()), Some("admin"));
}

#[test]
fn extracting_denied_returns_none_claims() {
    let cfg = JwtAuthConfig {
        secret: Some("secret".into()),
        ..Default::default()
    };
    let (result, claims) = check_jwt_extracting(&cfg, "/api", None);
    assert!(matches!(result, JwtCheckResult::Denied { .. }));
    assert!(claims.is_none(), "claims must be None when denied");
}

#[test]
fn extracting_skip_path_returns_allowed_no_claims() {
    let cfg = JwtAuthConfig {
        secret: Some("secret".into()),
        skip_paths: Some(vec!["/public/**".into()]),
        ..Default::default()
    };
    let (result, claims) = check_jwt_extracting(&cfg, "/public/assets/style.css", None);
    assert!(matches!(result, JwtCheckResult::Allowed));
    assert!(claims.is_none(), "skipped paths return no claims");
}

// ── audience validation ───────────────────────────────────────────────────

#[test]
fn valid_audience_allowed() {
    let secret = "aud-secret";
    let token = make_hs256_token(
        secret,
        json!({ "sub": "u", "aud": "my-service", "exp": exp_future() }),
    );
    let cfg = JwtAuthConfig {
        secret: Some(secret.into()),
        audience: Some(vec!["my-service".into()]),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Allowed
    ));
}

#[test]
fn audience_mismatch_denied() {
    let secret = "aud-secret";
    let token = make_hs256_token(
        secret,
        json!({ "sub": "u", "aud": "wrong-service", "exp": exp_future() }),
    );
    let cfg = JwtAuthConfig {
        secret: Some(secret.into()),
        audience: Some(vec!["my-service".into()]),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Denied { .. }
    ));
}

#[test]
fn issuer_match_allowed() {
    let secret = "iss-secret";
    let token = make_hs256_token(
        secret,
        json!({ "sub": "u", "iss": "https://auth.example.com", "exp": exp_future() }),
    );
    let cfg = JwtAuthConfig {
        secret: Some(secret.into()),
        issuer: Some("https://auth.example.com".into()),
        ..Default::default()
    };
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("Bearer {token}"))),
        JwtCheckResult::Allowed
    ));
}

// ── extract_bearer edge cases ─────────────────────────────────────────────

#[test]
fn bearer_case_insensitive() {
    // The "bearer" prefix matching is done with to_ascii_lowercase().
    let secret = "case-secret";
    let token = make_hs256_token(secret, json!({ "sub": "u", "exp": exp_future() }));
    let cfg = JwtAuthConfig {
        secret: Some(secret.into()),
        ..Default::default()
    };
    // Uppercase BEARER prefix should still be parsed.
    assert!(matches!(
        check_jwt(&cfg, "/api", Some(&format!("BEARER {token}"))),
        JwtCheckResult::Allowed
    ));
}

#[test]
fn bearer_with_no_token_after_space_denied() {
    let cfg = JwtAuthConfig {
        secret: Some("secret".into()),
        ..Default::default()
    };
    // "Bearer " with nothing after → empty token → invalid
    assert!(matches!(
        check_jwt(&cfg, "/api", Some("Bearer ")),
        JwtCheckResult::Denied { .. }
    ));
}

#[test]
fn non_object_claims_returns_none_from_extract() {
    // A JWT whose payload is a JSON array (not an object) can't be
    // extracted as a HashMap<String, Value> — extract_claims_unchecked
    // must return None rather than panicking or silently discarding data.
    let secret = "secret";
    let token = make_hs256_token(secret, json!(["not", "an", "object"]));
    assert!(
        extract_claims_unchecked(&token).is_none(),
        "non-object payload must yield None, not a HashMap"
    );
}
