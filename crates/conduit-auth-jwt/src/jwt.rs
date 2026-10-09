//! JWT bearer-token validation.
//!
//! Supports two modes:
//!
//! - **HS256** — shared HMAC-SHA256 secret (`jwtAuth.secret`).
//! - **RS256 / ES256** — asymmetric keys from a remote JWKS URL
//!   (`jwtAuth.jwksUrl`).  Keys are fetched on the first request that needs
//!   them and refreshed in the background once they are `jwksRefreshSecs`
//!   old (default 3600); see the `jwks_cache` module for the single-flight and
//!   stale-fallback behaviour.
//!
//! The token must be present in the `Authorization: Bearer <token>` header.
//! A missing or invalid token returns `401 Unauthorized`.
//!
//! Gated behind this crate's own `jwt` Cargo feature (was
//! `#![cfg(feature = "jwt")]` on this file pre-extraction, #133 — now
//! applied to the `mod jwt;` declaration in `lib.rs` instead).

use jsonwebtoken::{decode_header, Algorithm, DecodingKey, Validation};

use conduit_core::filter::path::is_path_skipped;

use crate::config::JwtAuthConfig;

mod jwks_cache;
#[cfg(test)]
use jwks_cache::fetch_jwks;
use jwks_cache::{get_jwks_keys, CachedKey};

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
/// Used by the root crate's per-consumer JWT auth (`src/filter/auth.rs`,
/// V2/V3 credential checks) when both validation AND claim extraction are
/// needed in one call.
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
    //
    // Issue #281: a kid-less key is cached under a synthesized identifier
    // (`"{key_type}-default"`, e.g. "RSA-default" — see get_jwks_keys), but
    // a kid-less *token* was looked up under the literal string "default",
    // which never matches. A single-key IdP omitting `kid` on both the JWKS
    // entry and the token is a legitimate, common setup (many single-key
    // IdPs don't bother with `kid` at all) — so when the token has no `kid`,
    // use the sole cached key if there's exactly one; reject as ambiguous
    // if there's more than one (no way to know which one the token means).
    let header = decode_header(token).map_err(|_| "invalid JWT header")?;
    let key_material = match header.kid.as_deref() {
        Some(kid) => keys.get(kid).ok_or("no matching JWKS key found for kid")?,
        None => match keys.len() {
            1 => keys.values().next().expect("checked len == 1"),
            0 => return Err("no matching JWKS key found for kid"),
            _ => return Err("JWT has no kid but JWKS contains multiple keys — ambiguous"),
        },
    };

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
mod tests;
