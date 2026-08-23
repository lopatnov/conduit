#![cfg(feature = "jwt")]
//! JWT bearer-token validation.
//!
//! Supports two modes:
//!
//! - **HS256** — shared HMAC-SHA256 secret (`jwtAuth.secret`).
//! - **RS256 / ES256** — asymmetric keys from a remote JWKS URL
//!   (`jwtAuth.jwksUrl`).  Keys are fetched once at startup and refreshed in
//!   a background task every `jwksRefreshSecs` seconds (default 3600).
//!
//! The token must be present in the `Authorization: Bearer <token>` header.
//! A missing or invalid token returns `401 Unauthorized`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use jsonwebtoken::{decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::config::schema::JwtAuthConfig;
use crate::filter::auth::is_path_skipped;

// ── JWKS key cache ────────────────────────────────────────────────────────────

/// A cached JWKS response with the time it was last fetched.
struct JwksCache {
    /// Map from `kid` → base-64-encoded public key material.
    keys: HashMap<String, CachedKey>,
    fetched_at: Instant,
}

#[derive(Clone)]
enum CachedKey {
    Rsa {
        n: String,
        e: String,
    },
    Ec {
        x: String,
        y: String,
        // Parsed from the JWK for completeness but not currently consulted —
        // `DecodingKey::from_ec_components` infers the curve from the JWT's
        // `alg` header (ES256 → P-256, ES384 → P-384), not from this field.
        #[allow(dead_code)]
        crv: String,
    },
}

/// Global JWKS caches keyed by JWKS URL.
static JWKS_CACHES: OnceLock<Arc<RwLock<HashMap<String, JwksCache>>>> = OnceLock::new();

fn jwks_caches() -> &'static Arc<RwLock<HashMap<String, JwksCache>>> {
    JWKS_CACHES.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

// ── Minimal JWKS JSON types ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    #[serde(rename = "kty")]
    key_type: String,
    #[serde(default)]
    kid: Option<String>,
    // RSA
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    // EC
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default, rename = "crv")]
    curve: Option<String>,
}

// ── JWKS fetch ────────────────────────────────────────────────────────────────

/// Blocking-compatible JWKS fetch using `reqwest`.
///
/// Called at startup on a `current_thread` runtime (same pattern as ACME) so
/// it doesn't block the Pingora worker thread pool.
async fn fetch_jwks(url: &str) -> anyhow::Result<HashMap<String, CachedKey>> {
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<JwksResponse>()
        .await?;

    let mut map = HashMap::new();
    for jwk in resp.keys {
        let kid = jwk
            .kid
            .unwrap_or_else(|| format!("{}-default", jwk.key_type));
        let cached = match jwk.key_type.as_str() {
            "RSA" => {
                if let (Some(n), Some(e)) = (jwk.n, jwk.e) {
                    Some(CachedKey::Rsa { n, e })
                } else {
                    tracing::warn!(kid, "JWKS RSA key missing n or e — skipped");
                    None
                }
            }
            "EC" => {
                if let (Some(x), Some(y), Some(crv)) = (jwk.x, jwk.y, jwk.curve) {
                    Some(CachedKey::Ec { x, y, crv })
                } else {
                    tracing::warn!(kid, "JWKS EC key missing x, y, or crv — skipped");
                    None
                }
            }
            other => {
                tracing::debug!(key_type = other, "JWKS key type not supported — skipped");
                None
            }
        };
        if let Some(c) = cached {
            map.insert(kid, c);
        }
    }
    Ok(map)
}

/// Load JWKS keys for `url`, using the cache when fresh enough.
///
/// `refresh_secs` is the maximum age before a refresh is attempted.
/// Defaults to 3600 s (1 hour) when `None`.
fn get_jwks_keys(url: &str, refresh_secs: u64) -> Option<Arc<HashMap<String, CachedKey>>> {
    // Fast path: cache hit within TTL.
    {
        let cache = jwks_caches().read().unwrap();
        if let Some(entry) = cache.get(url) {
            if entry.fetched_at.elapsed().as_secs() < refresh_secs {
                // Build a temporary Arc-wrapped copy of the keys.
                return Some(Arc::new(entry.keys.clone()));
            }
        }
    }

    // Slow path: fetch synchronously in a temporary tokio current_thread runtime.
    let url_owned = url.to_owned();
    let result = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?
            .block_on(fetch_jwks(&url_owned))
            .ok()
    })
    .join()
    .ok()??;

    let keys_arc: Arc<HashMap<String, CachedKey>> = Arc::new(result);
    {
        let mut cache = jwks_caches().write().unwrap();
        cache.insert(
            url.to_owned(),
            JwksCache {
                keys: (*keys_arc).clone(),
                fetched_at: Instant::now(),
            },
        );
    }
    Some(keys_arc)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Result of JWT validation.
pub enum JwtCheckResult {
    /// Token is valid (or the path is in skipPaths).
    Allowed,
    /// Token is missing or invalid — respond with 401.
    Denied { reason: &'static str },
}

/// Shared skip-path + bearer-extraction prelude for [`check_jwt`] and
/// [`check_jwt_extracting`].
///
/// Returns `Ok(token)` when the request should proceed to signature
/// validation, or `Err(result)` with the early-return result (`Allowed` for
/// a skipped path, `Denied` for a missing/malformed header) otherwise.
fn jwt_prelude<'a>(
    cfg: &JwtAuthConfig,
    path: &str,
    auth_header: Option<&'a str>,
) -> Result<&'a str, JwtCheckResult> {
    if let Some(skip) = &cfg.skip_paths {
        if is_path_skipped(Some(skip.as_slice()), path) {
            return Err(JwtCheckResult::Allowed);
        }
    }
    extract_bearer(auth_header).ok_or(JwtCheckResult::Denied {
        reason: "missing or malformed Bearer token",
    })
}

/// Validate the `Authorization: Bearer` token for the current request.
///
/// Returns [`JwtCheckResult::Allowed`] when:
/// - The request path is in `skip_paths`, OR
/// - The token is present and valid.
pub fn check_jwt(cfg: &JwtAuthConfig, path: &str, auth_header: Option<&str>) -> JwtCheckResult {
    let raw_token = match jwt_prelude(cfg, path, auth_header) {
        Ok(token) => token,
        Err(result) => return result,
    };

    match validate_token(cfg, raw_token) {
        Ok(()) => JwtCheckResult::Allowed,
        Err(reason) => JwtCheckResult::Denied { reason },
    }
}

/// Validate the JWT **and** return the decoded claims in a single pass.
///
/// Equivalent to calling [`check_jwt`] followed by `extract_claims_unchecked` but
/// avoids the second base64-decode + JSON-parse that the two-step pattern
/// requires.  Returns `(Allowed, Some(claims))` on success or
/// `(Denied, None)` on failure.
///
/// Used by `do_request_filter` when both validation AND claim extraction are
/// needed (e.g. for `requestTransform.setHeaders: { "X-User": "{{ jwt.sub }}" }`).
pub fn check_jwt_extracting(
    cfg: &JwtAuthConfig,
    path: &str,
    auth_header: Option<&str>,
) -> (
    JwtCheckResult,
    Option<std::collections::HashMap<String, serde_json::Value>>,
) {
    let raw_token = match jwt_prelude(cfg, path, auth_header) {
        Ok(token) => token,
        Err(result) => return (result, None),
    };
    match validate_token(cfg, raw_token) {
        Ok(()) => {
            let claims = extract_claims_unchecked(raw_token);
            (JwtCheckResult::Allowed, claims)
        }
        Err(reason) => (JwtCheckResult::Denied { reason }, None),
    }
}

/// Extract the JWT payload claims as a key→value map, **without verifying
/// the signature**.
///
/// Returns `None` when the token can't be decoded or the claims can't be
/// parsed.
///
/// # Safety contract (not type-enforced)
/// The caller MUST have already confirmed the token's signature and
/// standard claims are valid — via [`check_jwt`] or [`validate_token`] —
/// before calling this. Never call this directly on a token whose
/// provenance you haven't verified. `pub(crate)`-scoped precisely so this
/// can't be reached from outside the crate (also published as a library
/// on crates.io) by a caller unaware of that contract.
///
/// Prefer [`check_jwt_extracting`] when both validation and claims are needed.
pub(crate) fn extract_claims_unchecked(
    token: &str,
) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    // Decode without validation — see safety contract above.
    // Use the dedicated insecure_decode API instead of the deprecated
    // `Validation::insecure_disable_signature_validation()` method.
    let data = jsonwebtoken::dangerous::insecure_decode::<serde_json::Value>(token).ok()?;

    if let serde_json::Value::Object(map) = data.claims {
        Some(map.into_iter().collect())
    } else {
        None
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

pub(crate) fn extract_bearer(auth_header: Option<&str>) -> Option<&str> {
    let hdr = auth_header?.trim();
    let lower = hdr.to_ascii_lowercase();
    if lower.starts_with("bearer ") {
        Some(hdr[7..].trim())
    } else {
        None
    }
}

fn validate_token(cfg: &JwtAuthConfig, token: &str) -> Result<(), &'static str> {
    if let Some(secret) = &cfg.secret {
        // HS256 path
        let key = DecodingKey::from_secret(secret.as_bytes());
        let mut v = Validation::new(Algorithm::HS256);
        configure_validation(&mut v, cfg);
        decode::<serde_json::Value>(token, &key, &v).map_err(|_| "invalid HS256 token")?;
        Ok(())
    } else if let Some(jwks_url) = &cfg.jwks_url {
        validate_with_jwks(cfg, token, jwks_url)
    } else {
        Err("jwtAuth misconfigured: neither secret nor jwksUrl provided")
    }
}

fn configure_validation(v: &mut Validation, cfg: &JwtAuthConfig) {
    v.validate_exp = true;
    if let Some(iss) = &cfg.issuer {
        v.set_issuer(&[iss]);
    }
    // When no expected audience is configured, disable audience validation so
    // tokens without an `aud` claim are accepted.
    if let Some(aud) = &cfg.audience {
        v.set_audience(aud);
    } else {
        v.validate_aud = false;
    }
}

fn validate_with_jwks(
    cfg: &JwtAuthConfig,
    token: &str,
    jwks_url: &str,
) -> Result<(), &'static str> {
    let refresh = cfg.jwks_refresh_secs.unwrap_or(3600);
    let keys = get_jwks_keys(jwks_url, refresh).ok_or("failed to fetch JWKS keys")?;

    // Determine which key to use from the JWT `kid` header.
    let header = decode_header(token).map_err(|_| "invalid JWT header")?;
    let kid = header.kid.as_deref().unwrap_or("default");

    let key_material = keys.get(kid).ok_or("no matching JWKS key found for kid")?;

    let decoding_key = match key_material {
        CachedKey::Rsa { n, e } => {
            DecodingKey::from_rsa_components(n, e).map_err(|_| "invalid RSA key material")?
        }
        CachedKey::Ec { x, y, .. } => {
            DecodingKey::from_ec_components(x, y).map_err(|_| "invalid EC key material")?
        }
    };

    let algo = match header.alg {
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => header.alg,
        Algorithm::ES256 | Algorithm::ES384 => header.alg,
        _ => Algorithm::RS256, // safe default
    };

    let mut v = Validation::new(algo);
    configure_validation(&mut v, cfg);
    decode::<serde_json::Value>(token, &decoding_key, &v)
        .map_err(|_| "JWT signature or claims validation failed")?;
    Ok(())
}

// Re-export for convenience
use jsonwebtoken::decode;

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::JwtAuthConfig;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use serde_json::json;

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

    // ── JWKS / RS256 / ES256 (issue #164) ─────────────────────────────────────
    //
    // Everything below exercises the JWKS code path — fetch_jwks,
    // get_jwks_keys, and validate_with_jwks — which previously had zero
    // automated coverage despite being half of what this feature advertises
    // ("HS256 + RS256/ES256 via JWKS", CLAUDE.md / docs/configuration.md).
    //
    // Test key material below is a throwaway RSA-2048 and P-256 keypair
    // generated once via:
    //   openssl genrsa 2048
    //   openssl ecparam -genkey -noout -name prime256v1 \
    //       | openssl pkcs8 -topk8 -nocrypt
    // with their public JWK components (n/e, x/y) derived from the same
    // keys. Not used anywhere outside this module — kept as source literals
    // rather than a checked-in file fixture, same rationale as the `rcgen`
    // idiom used for TLS test certs (`.claude/skills/testing/SKILL.md`).

    const TEST_RSA_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDNWnf0LvkZ5S5x
eRyvsL+ItXjrVF9Rxpkja1QCtbGI4kSIYpeNexcdCRbpylB2SgB39v4g/xRktBBk
7brOQbn9kBLDa49vpnGlHrViA2e8dmwC3OLLAza64p5hItraLJ909XOeck/sKyCx
o3dHFsz74xhLyqZv6kGjm67GVN6+rdR/ItCcMgEk5pbA1apW2659H7T9MkqCP2g2
xKLaTCWeCFVVCBCGCXOxESlGdXmZq/4VT+q7gtq0d3f6XuCIE+5dvREO8lPBG+hq
sjKCnEpddWxfPwy/085r8vLwBFKo7GgndwRPEmoboQUEy4n4+QbLDpKfidPfo/b2
GQBAhlqjAgMBAAECggEARev3CirwYLPbk4GkleH95aO874xEBIk13YyPB3ksYSqC
IVpItkDiRt2wcpyTtyNNc4ujTkLsg7mYF3Wm9NIGbWMgMHAwX9jxu0JwilYUfWRp
NLRXeL64ZPwC55pBoKYvCVkGLD5KHmU09aduVsNZuq7BuBThhRvji7zXzupZCd10
ZrjigojOlbVD7qUd50Hxm4MBmWR81uEGIocZjaeIyJBVI5jZHdo2SthXI0WESgSl
5GBIptdCDdBRFP9NdmkjqvkG4sjg3NC58LqRLLVit2q0sJUF1Djq20hYnFRG3amX
swPggHMeCY3b1ofKZPbe4DBPW48oCeICqcrS82zjMQKBgQDnTgk/ljSyl1uUhI3n
sMB/N0LNSxqotxenTIdGWQjNZPCuPqODYZ/QTfs1CGxpy30zZMyrU4XlUerYJfSB
K7c3OfcQc4PaXFMd9xfD4HGFfQsY+AxVH0I5JbXbHwKkgQCBZlY51RCVn6sS8Y6B
FC/Yap8SHOmM765O0Y3FWdnCawKBgQDjRyJuiI+zdKOHW9nZ7tRhYLk5vVV/l02N
0sm7qzods+NyUQy/RG1x/73WSBmzimUUKwVxu57sQ4k087fuHIk0C/X69wzrYk+j
AVjEVqDYttL5HUZx0RBt42plyNnnfoTmk0pZMoGCSlVmhiWzi0cIL3WB0c/nBMT5
cqPutkOGqQKBgGXIois4BtJ75lHRjrxgvCR/BcdfAEkz4JW/CFv9e/EeNQcIC14a
DIBWgG+S2FopsFt4RNQzed0ykfwxn4lj2kjUGhNEMcZaED1EaVHJp0rNfp+rL4oZ
qkOJg5/74mbPWZCXnuPuDVE6JMa+Qy4r2u4J5RvMWz2ojvSiJBeu9TMnAoGAPcqn
R9oFB8tccn68eg3+3ALKGTKqvifKxBZdFpL1GAJCgmAa0R2vi+D2If40TqX/2T3h
GwzhpmauNSFWDnzfqLDfzb3BW3W9JRpGogrTbFg4f9Y/ws4OY3IDCW1UISY6x92f
xyR+JYhEM72hHnFtfII6tnLuzWZ0j0Vl4I7ZSRECgYBDjd0Q4xNMh86Ww33Jw00s
u4DwNPb1Ej548zHBzLhLYEvjI55HfzdMC3gWyFN61OmBn/cW3ICRpdIWY8SBjCcD
PbDYkdfi3nNWacEhvLerpi6H9btsbmkhJGCtN8XER/1TopD+rMfa9h3Z8dUx5XF/
LfRIVRjRYsggsiOofp2hgQ==
-----END PRIVATE KEY-----
";
    const TEST_RSA_N: &str = "zVp39C75GeUucXkcr7C_iLV461RfUcaZI2tUArWxiOJEiGKXjXsXHQkW6cpQdkoAd_b-IP8UZLQQZO26zkG5_ZASw2uPb6ZxpR61YgNnvHZsAtziywM2uuKeYSLa2iyfdPVznnJP7CsgsaN3RxbM--MYS8qmb-pBo5uuxlTevq3UfyLQnDIBJOaWwNWqVtuufR-0_TJKgj9oNsSi2kwlnghVVQgQhglzsREpRnV5mav-FU_qu4LatHd3-l7giBPuXb0RDvJTwRvoarIygpxKXXVsXz8Mv9POa_Ly8ARSqOxoJ3cETxJqG6EFBMuJ-PkGyw6Sn4nT36P29hkAQIZaow";
    const TEST_RSA_E: &str = "AQAB";

    // A second, unrelated RSA private key — same shape, different key
    // material. Used only to prove JWKS signature verification actually
    // checks the signature, not just kid presence / structural validity.
    const OTHER_RSA_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCRWwkHNyEaS2Zy
x4pAQr685oc7A+QReSIIvuihvdZ1pQCIbKax7xdkCTGJ1gJy+Z1w/qF4PE3Om6RZ
HBzjajNtKJDQUNdOO+qGB2Kul7/wETuZxzwY0KLcwwDmjYJiYH0iSaWWQFkNsPcz
xsk7EBzm6GAAP5dYvYtIrDLdFmzSq4gchpZFjFqGR/iPrRH/RcY+KpwijWPxN9fk
0kFOom05W3CN2naMPG25EZRwB8OCgnyZIqaSBA+XLUH8x+UkqdhM1tuRP9wmJyP7
mSBNGDU+c/BdGIwJqM5zDE/RjHZqb/NaCubqoAtR4bAInygp9xRe1yEqFwZ62CJv
UNetpGJLAgMBAAECggEARRDghVEopXnWQAuYIViVkORotR3wLG1GQqmTl+bAFD5G
towJ2NomXx4PL9NEbqU0rhAPYTYmMlm6Ca1V/KjlrqRrys/evgmyMeUoepUYWlWV
4EfOwmvANu1hbCspHN2EF9qul2oT5nGDxFJcI3hQg1c+5l9Q5pWJrQpFUM/q/V5U
M3uDZWN1ZfHMWNmB94hfRd6skSJP9Djnd5uOXth5fQIIPv/UqY+Xa/nuCHUI4rud
XiKdlZSpBWrzOuqvXl2kbJC8E4MsHIPbdjlEowknpQ2OuNJyAF1Bp3gwjbrwAQGm
ueFkiM+1FZN8YESOxacL7QXoKoezrvy5OQX0c2xMQQKBgQDM53MrWnXLh28gcVYA
QUlNVDCUbTcMK501xbZpLwN8GwQXj36IFgNLmrK/9hyXvC2vnyispxZymUHyIdlE
Y5wajS+5fJhbYcYlizC/tupNQEf0Ole9ulKFloVc8jnDDmLBOCW3rDtv5+lNj+gF
RaRRjPQUY4fXGlla8HcWsakfiQKBgQC1mimwbVmeVzZYxVdoakcg1TYF3lHULuQI
SYEC4rJfmIIefCOJ5FDeBRrG6JSC4r0bbDb6V/PGJS7ATWJ90qvKW5xoYlT3xqXr
HoVsSm7toXZvgVmgWSR7ZdWjrCj9YaDQLPF2lVXs46sXf6VreyVmvqrwwUyLXRJs
88JUGEnKMwKBgHUqyAV7Va5LRHU1uaqtql/Ii3rkNL0F14CfDN56nrCBtkZOrFje
1YWO8TWpYtI1LZ6mERkg9koTbs0pI9biaqoYH7keEPT4JNjlDbwiuTnxTvPNxMxd
1cBDwQDUFcl+2WOJWq/7kYU9BIBwkIkrOHnVcuCRxWRv0baZmE9mycGZAoGAQE20
UVqHD0BGaCyIhNqNER0uIenVA9MOv7h3TDRFgQAZov3F/7+uus8H6kLUw3vSBnHN
Ddwy34ivAzzjkTYVynOh8HxRJeNbQOPvzqaUnOQ9ccJVoCeweVlXyrrdUMtPDCe9
4IWEhXsgTBPQ2TwjxDvjf5iSqA5uxdGSkACBsG0CgYBiR8vCK1i1Q2XaGrYgactQ
9PucxLwA7FJieoxhszuCMjUvRUfxS3eSTKShbR/mP0Da3q3ku3kzlRF57kjA9Wd9
iRx9uVETgyq9g3iT+TYRuSg5WwEvUfisqbNL/Zsc4W8KM7sDM1YPuSH0tbu1V/VT
CGSyIC/d2C9tqL5Ay/de+w==
-----END PRIVATE KEY-----
";

    const TEST_EC_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgO+vG0gJqP/s7JUdy
BwPeilrWBLT5dNFHVXKlX6uu12yhRANCAASe6tX2EgNOmZZVXoVAqvJTQKashueQ
MBdVbndLwtz+I5BBg5yFxotFl9oEJsFbskyQBFV5Lx5glUzRJD5+D+p6
-----END PRIVATE KEY-----
";
    const TEST_EC_X: &str = "nurV9hIDTpmWVV6FQKryU0CmrIbnkDAXVW53S8Lc_iM";
    const TEST_EC_Y: &str = "kEGDnIXGi0WX2gQmwVuyTJAEVXkvHmCVTNEkPn4P6no";

    /// Bare-TCP HTTP server (see `.claude/skills/testing/SKILL.md` "Mock
    /// upstream = raw TcpListener, not Axum") that returns `body` with
    /// `status_line` for every connection it accepts, until the test
    /// process exits. Returns the `http://127.0.0.1:PORT` base URL.
    fn spawn_mock_http_server(status_line: &str, body: &str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let status_line = status_line.to_owned();
        let body = body.to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let status_line = status_line.clone();
                let body = body.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    let response = format!(
                        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(response.as_bytes());
                });
            }
        });
        format!("http://{addr}")
    }

    fn rsa_jwks_body(kid: &str, n: &str, e: &str) -> String {
        format!(r#"{{"keys":[{{"kty":"RSA","kid":"{kid}","n":"{n}","e":"{e}"}}]}}"#)
    }

    fn ec_jwks_body(kid: &str, x: &str, y: &str) -> String {
        format!(r#"{{"keys":[{{"kty":"EC","kid":"{kid}","crv":"P-256","x":"{x}","y":"{y}"}}]}}"#)
    }

    fn make_rs256_token(kid: &str, private_pem: &str, claims: serde_json::Value) -> String {
        let key = EncodingKey::from_rsa_pem(private_pem.as_bytes()).unwrap();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        encode(&header, &claims, &key).unwrap()
    }

    fn make_es256_token(kid: &str, private_pem: &str, claims: serde_json::Value) -> String {
        let key = EncodingKey::from_ec_pem(private_pem.as_bytes()).unwrap();
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(kid.to_owned());
        encode(&header, &claims, &key).unwrap()
    }

    fn make_hs256_token_with_kid(
        kid: &str,
        secret_bytes: &[u8],
        claims: serde_json::Value,
    ) -> String {
        let key = EncodingKey::from_secret(secret_bytes);
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some(kid.to_owned());
        encode(&header, &claims, &key).unwrap()
    }

    // ── fetch_jwks ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_jwks_parses_valid_rsa_key() {
        let body = rsa_jwks_body("rsa-1", TEST_RSA_N, TEST_RSA_E);
        let base = spawn_mock_http_server("HTTP/1.1 200 OK", &body);
        let keys = fetch_jwks(&format!("{base}/jwks"))
            .await
            .expect("fetch should succeed");
        assert_eq!(keys.len(), 1);
        match keys.get("rsa-1").expect("rsa-1 key must be present") {
            CachedKey::Rsa { n, e } => {
                assert_eq!(n, TEST_RSA_N);
                assert_eq!(e, TEST_RSA_E);
            }
            CachedKey::Ec { .. } => panic!("expected an RSA key, got EC"),
        }
    }

    #[tokio::test]
    async fn fetch_jwks_parses_valid_ec_key() {
        let body = ec_jwks_body("ec-1", TEST_EC_X, TEST_EC_Y);
        let base = spawn_mock_http_server("HTTP/1.1 200 OK", &body);
        let keys = fetch_jwks(&format!("{base}/jwks"))
            .await
            .expect("fetch should succeed");
        assert_eq!(keys.len(), 1);
        match keys.get("ec-1").expect("ec-1 key must be present") {
            CachedKey::Ec { x, y, .. } => {
                assert_eq!(x, TEST_EC_X);
                assert_eq!(y, TEST_EC_Y);
            }
            CachedKey::Rsa { .. } => panic!("expected an EC key, got RSA"),
        }
    }

    #[tokio::test]
    async fn fetch_jwks_skips_rsa_key_missing_e() {
        // No "e" field — the RSA branch in fetch_jwks must warn and skip
        // this key rather than erroring the whole fetch or panicking.
        let body = r#"{"keys":[{"kty":"RSA","kid":"rsa-bad","n":"abc"}]}"#;
        let base = spawn_mock_http_server("HTTP/1.1 200 OK", body);
        let keys = fetch_jwks(&format!("{base}/jwks"))
            .await
            .expect("fetch should still succeed for the response as a whole");
        assert!(
            keys.is_empty(),
            "key missing a required component must be skipped, not partially inserted"
        );
    }

    #[tokio::test]
    async fn fetch_jwks_malformed_json_returns_err() {
        let base = spawn_mock_http_server("HTTP/1.1 200 OK", "not valid json{");
        assert!(fetch_jwks(&format!("{base}/jwks")).await.is_err());
    }

    #[tokio::test]
    async fn fetch_jwks_non_200_status_returns_err() {
        let base = spawn_mock_http_server("HTTP/1.1 404 Not Found", "{}");
        assert!(fetch_jwks(&format!("{base}/jwks")).await.is_err());
    }

    // ── validate_with_jwks (full round-trip: fetch + kid match + verify) ──────

    #[test]
    fn validate_with_jwks_rs256_valid_token_passes() {
        let body = rsa_jwks_body("rsa-1", TEST_RSA_N, TEST_RSA_E);
        let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
        let cfg = JwtAuthConfig {
            jwks_url: Some(jwks_url.clone()),
            ..Default::default()
        };
        let token = make_rs256_token(
            "rsa-1",
            TEST_RSA_PRIVATE_PEM,
            json!({ "sub": "u", "exp": exp_future() }),
        );
        assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_ok());
    }

    #[test]
    fn validate_with_jwks_es256_valid_token_passes() {
        let body = ec_jwks_body("ec-1", TEST_EC_X, TEST_EC_Y);
        let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
        let cfg = JwtAuthConfig {
            jwks_url: Some(jwks_url.clone()),
            ..Default::default()
        };
        let token = make_es256_token(
            "ec-1",
            TEST_EC_PRIVATE_PEM,
            json!({ "sub": "u", "exp": exp_future() }),
        );
        assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_ok());
    }

    #[test]
    fn validate_with_jwks_kid_mismatch_denied() {
        let body = rsa_jwks_body("rsa-other", TEST_RSA_N, TEST_RSA_E);
        let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
        let cfg = JwtAuthConfig {
            jwks_url: Some(jwks_url.clone()),
            ..Default::default()
        };
        let token = make_rs256_token(
            "rsa-1",
            TEST_RSA_PRIVATE_PEM,
            json!({ "sub": "u", "exp": exp_future() }),
        );
        assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_err());
    }

    #[test]
    fn validate_with_jwks_wrong_rsa_key_signature_denied() {
        let body = rsa_jwks_body("rsa-1", TEST_RSA_N, TEST_RSA_E);
        let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
        let cfg = JwtAuthConfig {
            jwks_url: Some(jwks_url.clone()),
            ..Default::default()
        };
        // Signed with a *different* RSA private key than the one JWKS
        // advertises under the same kid — proves signature verification
        // actually happens, not just kid lookup / structural well-formedness.
        let token = make_rs256_token(
            "rsa-1",
            OTHER_RSA_PRIVATE_PEM,
            json!({ "sub": "u", "exp": exp_future() }),
        );
        assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_err());
    }

    #[test]
    fn validate_with_jwks_hs256_algorithm_confusion_rejected() {
        // Classic RS256->HS256 algorithm-confusion attack: the JWKS
        // endpoint's public RSA key material is, by definition, public —
        // an attacker signs an HS256 token using that public key's
        // components as the HMAC secret, hoping a naive verifier reuses the
        // same key material for HMAC validation. `validate_with_jwks` picks
        // its `Validation` algorithm from the *token's own* header (falling
        // back to RS256 for anything outside the RS/ES set), so an HS256
        // header here selects Validation::new(RS256) — and jsonwebtoken's
        // decode() cross-checks the header's actual alg against that,
        // rejecting the mismatch before any key material is even used to
        // verify. This test backs the "checked and found clean" note in
        // issue #164 with an actual assertion against this codebase.
        let body = rsa_jwks_body("rsa-1", TEST_RSA_N, TEST_RSA_E);
        let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
        let cfg = JwtAuthConfig {
            jwks_url: Some(jwks_url.clone()),
            ..Default::default()
        };
        let token = make_hs256_token_with_kid(
            "rsa-1",
            TEST_RSA_N.as_bytes(),
            json!({ "sub": "attacker", "exp": exp_future() }),
        );
        assert!(
            validate_with_jwks(&cfg, &token, &jwks_url).is_err(),
            "an HS256 token signed with the RSA public key material must not be \
             accepted via the RS256 JWKS path"
        );
    }

    #[test]
    fn validate_with_jwks_unreachable_endpoint_denied() {
        // Nothing listening on port 1 — same "unreachable" idiom already
        // used for the Redis fail-open tests in this codebase.
        let jwks_url = "http://127.0.0.1:1/jwks".to_owned();
        let cfg = JwtAuthConfig {
            jwks_url: Some(jwks_url.clone()),
            ..Default::default()
        };
        let token = make_rs256_token(
            "rsa-1",
            TEST_RSA_PRIVATE_PEM,
            json!({ "sub": "u", "exp": exp_future() }),
        );
        assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_err());
    }
}
