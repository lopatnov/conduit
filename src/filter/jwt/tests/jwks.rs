//! JWKS / RS256 / ES256 coverage (issue #164).
//!
//! Exercises `fetch_jwks`, `get_jwks_keys`, and `validate_with_jwks` — the
//! half of this feature that previously had zero automated coverage.
//!
//! RSA-2048 / P-256 test key material is generated fresh at test-run time
//! (not embedded as static PEM literals) — matches the `rcgen` "no
//! checked-in cert fixtures" idiom already used for TLS test certs
//! (`.claude/skills/testing/SKILL.md`), and keeps nothing that looks like
//! real private-key material in the repo for a secret-scanner to flag.

use super::*;
use std::sync::OnceLock;

use base64::Engine as _;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::EncodePrivateKey;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

struct RsaTestKey {
    pem: String,
    n: String,
    e: String,
}

fn gen_rsa_test_key() -> RsaTestKey {
    let private_key =
        rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).expect("RSA-2048 keygen");
    let pem = private_key
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("RSA PKCS#1 PEM encode")
        .to_string();
    let public_key = private_key.to_public_key();
    RsaTestKey {
        pem,
        n: b64url(&public_key.n().to_bytes_be()),
        e: b64url(&public_key.e().to_bytes_be()),
    }
}

/// The primary RSA test key, generated once and shared read-only across
/// every test in this binary — RSA-2048 keygen is comparatively slow, no
/// need to pay that cost per test when the key material itself doesn't
/// need to vary.
fn primary_rsa_key() -> &'static RsaTestKey {
    static KEY: OnceLock<RsaTestKey> = OnceLock::new();
    KEY.get_or_init(gen_rsa_test_key)
}

/// A second, unrelated RSA key — used only to prove JWKS signature
/// verification actually checks the signature, not just kid presence.
fn other_rsa_key() -> &'static RsaTestKey {
    static KEY: OnceLock<RsaTestKey> = OnceLock::new();
    KEY.get_or_init(gen_rsa_test_key)
}

struct EcTestKey {
    pem: String,
    x: String,
    y: String,
}

fn gen_ec_test_key() -> EcTestKey {
    let secret_key = p256::SecretKey::random(&mut rand_core::OsRng);
    let pem = secret_key
        .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
        .expect("EC PKCS#8 PEM encode")
        .to_string();
    let point = secret_key.public_key().to_encoded_point(false);
    EcTestKey {
        pem,
        x: b64url(point.x().expect("uncompressed point has x")),
        y: b64url(point.y().expect("uncompressed point has y")),
    }
}

fn ec_key() -> &'static EcTestKey {
    static KEY: OnceLock<EcTestKey> = OnceLock::new();
    KEY.get_or_init(gen_ec_test_key)
}

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

fn make_hs256_token_with_kid(kid: &str, secret_bytes: &[u8], claims: serde_json::Value) -> String {
    let key = EncodingKey::from_secret(secret_bytes);
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(kid.to_owned());
    encode(&header, &claims, &key).unwrap()
}

// ── fetch_jwks ────────────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_jwks_parses_valid_rsa_key() {
    let rsa = primary_rsa_key();
    let body = rsa_jwks_body("rsa-1", &rsa.n, &rsa.e);
    let base = spawn_mock_http_server("HTTP/1.1 200 OK", &body);
    let keys = fetch_jwks(&format!("{base}/jwks"))
        .await
        .expect("fetch should succeed");
    assert_eq!(keys.len(), 1);
    match keys.get("rsa-1").expect("rsa-1 key must be present") {
        CachedKey::Rsa { n, e } => {
            assert_eq!(n, &rsa.n);
            assert_eq!(e, &rsa.e);
        }
        CachedKey::Ec { .. } => panic!("expected an RSA key, got EC"),
    }
}

#[tokio::test]
async fn fetch_jwks_parses_valid_ec_key() {
    let ec = ec_key();
    let body = ec_jwks_body("ec-1", &ec.x, &ec.y);
    let base = spawn_mock_http_server("HTTP/1.1 200 OK", &body);
    let keys = fetch_jwks(&format!("{base}/jwks"))
        .await
        .expect("fetch should succeed");
    assert_eq!(keys.len(), 1);
    match keys.get("ec-1").expect("ec-1 key must be present") {
        CachedKey::Ec { x, y, .. } => {
            assert_eq!(x, &ec.x);
            assert_eq!(y, &ec.y);
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
    let rsa = primary_rsa_key();
    let body = rsa_jwks_body("rsa-1", &rsa.n, &rsa.e);
    let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
    let cfg = JwtAuthConfig {
        jwks_url: Some(jwks_url.clone()),
        ..Default::default()
    };
    let token = make_rs256_token(
        "rsa-1",
        &rsa.pem,
        json!({ "sub": "u", "exp": exp_future() }),
    );
    assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_ok());
}

#[test]
fn validate_with_jwks_es256_valid_token_passes() {
    let ec = ec_key();
    let body = ec_jwks_body("ec-1", &ec.x, &ec.y);
    let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
    let cfg = JwtAuthConfig {
        jwks_url: Some(jwks_url.clone()),
        ..Default::default()
    };
    let token = make_es256_token("ec-1", &ec.pem, json!({ "sub": "u", "exp": exp_future() }));
    assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_ok());
}

#[test]
fn validate_with_jwks_kid_mismatch_denied() {
    let rsa = primary_rsa_key();
    let body = rsa_jwks_body("rsa-other", &rsa.n, &rsa.e);
    let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
    let cfg = JwtAuthConfig {
        jwks_url: Some(jwks_url.clone()),
        ..Default::default()
    };
    let token = make_rs256_token(
        "rsa-1",
        &rsa.pem,
        json!({ "sub": "u", "exp": exp_future() }),
    );
    assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_err());
}

#[test]
fn validate_with_jwks_wrong_rsa_key_signature_denied() {
    let rsa = primary_rsa_key();
    let other = other_rsa_key();
    let body = rsa_jwks_body("rsa-1", &rsa.n, &rsa.e);
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
        &other.pem,
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
    let rsa = primary_rsa_key();
    let body = rsa_jwks_body("rsa-1", &rsa.n, &rsa.e);
    let jwks_url = format!("{}/jwks", spawn_mock_http_server("HTTP/1.1 200 OK", &body));
    let cfg = JwtAuthConfig {
        jwks_url: Some(jwks_url.clone()),
        ..Default::default()
    };
    let token = make_hs256_token_with_kid(
        "rsa-1",
        rsa.n.as_bytes(),
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
    let rsa = primary_rsa_key();
    let jwks_url = "http://127.0.0.1:1/jwks".to_owned();
    let cfg = JwtAuthConfig {
        jwks_url: Some(jwks_url.clone()),
        ..Default::default()
    };
    let token = make_rs256_token(
        "rsa-1",
        &rsa.pem,
        json!({ "sub": "u", "exp": exp_future() }),
    );
    assert!(validate_with_jwks(&cfg, &token, &jwks_url).is_err());
}
