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

enum CachedKey {
    Rsa { n: String, e: String },
    Ec { x: String, y: String, crv: String },
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
            .unwrap_or_else(|| format!("{}-default", &jwk.key_type));
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
                return Some(Arc::new(
                    entry
                        .keys
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                match v {
                                    CachedKey::Rsa { n, e } => CachedKey::Rsa {
                                        n: n.clone(),
                                        e: e.clone(),
                                    },
                                    CachedKey::Ec { x, y, crv } => CachedKey::Ec {
                                        x: x.clone(),
                                        y: y.clone(),
                                        crv: crv.clone(),
                                    },
                                },
                            )
                        })
                        .collect(),
                ));
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
                keys: keys_arc
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            match v {
                                CachedKey::Rsa { n, e } => CachedKey::Rsa {
                                    n: n.clone(),
                                    e: e.clone(),
                                },
                                CachedKey::Ec { x, y, crv } => CachedKey::Ec {
                                    x: x.clone(),
                                    y: y.clone(),
                                    crv: crv.clone(),
                                },
                            },
                        )
                    })
                    .collect(),
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

/// Validate the `Authorization: Bearer` token for the current request.
///
/// Returns [`JwtCheckResult::Allowed`] when:
/// - The request path is in `skip_paths`, OR
/// - The token is present and valid.
pub fn check_jwt(cfg: &JwtAuthConfig, path: &str, auth_header: Option<&str>) -> JwtCheckResult {
    if let Some(skip) = &cfg.skip_paths {
        if is_path_skipped(Some(skip.as_slice()), path) {
            return JwtCheckResult::Allowed;
        }
    }

    let raw_token = match extract_bearer(auth_header) {
        Some(t) => t,
        None => {
            return JwtCheckResult::Denied {
                reason: "missing or malformed Bearer token",
            }
        }
    };

    match validate_token(cfg, raw_token) {
        Ok(()) => JwtCheckResult::Allowed,
        Err(reason) => JwtCheckResult::Denied { reason },
    }
}

/// Validate the JWT **and** return the decoded claims in a single pass.
///
/// Equivalent to calling [`check_jwt`] followed by [`extract_claims`] but
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
    use crate::filter::auth::is_path_skipped;
    if let Some(skip) = &cfg.skip_paths {
        if is_path_skipped(Some(skip.as_slice()), path) {
            return (JwtCheckResult::Allowed, None);
        }
    }
    let raw_token = match extract_bearer(auth_header) {
        Some(t) => t,
        None => {
            return (
                JwtCheckResult::Denied {
                    reason: "missing or malformed Bearer token",
                },
                None,
            )
        }
    };
    match validate_token(cfg, raw_token) {
        Ok(()) => {
            let claims = extract_claims(raw_token, cfg);
            (JwtCheckResult::Allowed, claims)
        }
        Err(reason) => (JwtCheckResult::Denied { reason }, None),
    }
}

/// Extract the JWT payload claims as a key→value map.
///
/// Returns `None` when the token is invalid or the claims can't be parsed.
/// Only call this after [`check_jwt`] has already validated the token
/// (fast second decode — no remote I/O).
/// Prefer [`check_jwt_extracting`] when both validation and claims are needed.
pub fn extract_claims(
    token: &str,
    cfg: &JwtAuthConfig,
) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    // Decode without validation (already validated by check_jwt earlier).
    let mut v = jsonwebtoken::Validation::new(Algorithm::HS256);
    v.insecure_disable_signature_validation();
    v.validate_exp = false;
    v.validate_aud = false;

    // Try all the same key paths as validate_token but skip sig check.
    let data = if let Some(secret) = &cfg.secret {
        let key = DecodingKey::from_secret(secret.as_bytes());
        decode::<serde_json::Value>(token, &key, &v).ok()
    } else {
        // For JWKS — use insecure decode (sig already validated).
        let key = DecodingKey::from_secret(b"");
        decode::<serde_json::Value>(token, &key, &v).ok()
    }?;

    if let serde_json::Value::Object(map) = data.claims {
        Some(map.into_iter().collect())
    } else {
        None
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn extract_bearer(auth_header: Option<&str>) -> Option<&str> {
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

    // ── extract_claims / template substitution ────────────────────────────────

    #[test]
    fn extract_claims_returns_sub() {
        let secret = "claim-test-secret";
        let token = make_hs256_token(
            secret,
            json!({ "sub": "user42", "email": "u@example.com", "exp": exp_future() }),
        );
        let cfg = JwtAuthConfig {
            secret: Some(secret.into()),
            ..Default::default()
        };
        let claims = extract_claims(&token, &cfg).expect("claims should be extracted");
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
        use crate::proxy::service::expand_jwt_templates;
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
}
