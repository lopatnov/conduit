mod common;

use serial_test::serial;
use std::io::Write as _;
use tempfile::TempDir;

/// Generate a self-signed cert for localhost and return (TempDir, cert_path, key_path).
fn make_self_signed_cert() -> (TempDir, String, String) {
    use rcgen::{generate_simple_self_signed, CertifiedKey};
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .expect("rcgen");
    let dir = tempfile::tempdir().expect("tempdir");
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::File::create(&cert_path)
        .expect("cert.pem")
        .write_all(cert.pem().as_bytes())
        .expect("write cert");
    std::fs::File::create(&key_path)
        .expect("key.pem")
        .write_all(signing_key.serialize_pem().as_bytes())
        .expect("write key");
    (
        dir,
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    )
}

/// Non-redirecting client for checking raw 3xx responses.
fn no_redirect_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
}

// ── HTTP → HTTPS redirect ─────────────────────────────────────────────────

#[test]
#[serial]
fn http_redirect_returns_308() {
    let (_cert_dir, cert_path, key_path) = make_self_signed_cert();
    let tls_port = common::free_port();
    let http_port = common::free_port();
    let admin_port = common::free_port();

    // The TLS site's wait_ready polls HTTPS on tls_port via the insecure client.
    let _srv = common::TestServer::start_with_config(
        tls_port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": tls_port,
                "tls": {
                    "cert": cert_path,
                    "key": key_path,
                    "httpRedirectPort": http_port
                },
                "healthCheck": true
            }]
        }),
    );

    // Give the redirect service a moment to bind after the TLS service is ready.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let resp = no_redirect_client()
        .get(format!("http://127.0.0.1:{http_port}/hello"))
        .send()
        .expect("GET");

    assert_eq!(
        resp.status().as_u16(),
        308,
        "HTTP → HTTPS redirect must use 308 Permanent Redirect"
    );
}

#[test]
#[serial]
fn http_redirect_location_is_https() {
    let (_cert_dir, cert_path, key_path) = make_self_signed_cert();
    let tls_port = common::free_port();
    let http_port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        tls_port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": tls_port,
                "tls": {
                    "cert": cert_path,
                    "key": key_path,
                    "httpRedirectPort": http_port
                },
                "healthCheck": true
            }]
        }),
    );

    std::thread::sleep(std::time::Duration::from_millis(200));

    let resp = no_redirect_client()
        .get(format!("http://127.0.0.1:{http_port}/some/path?q=1"))
        .send()
        .expect("GET");

    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    assert!(
        location.starts_with("https://"),
        "Location must be an HTTPS URL, got: '{location}'"
    );
    assert!(
        location.contains("/some/path"),
        "Location must preserve the original path, got: '{location}'"
    );
}

#[test]
#[serial]
fn http_redirect_preserves_query_string() {
    let (_cert_dir, cert_path, key_path) = make_self_signed_cert();
    let tls_port = common::free_port();
    let http_port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        tls_port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": tls_port,
                "tls": {
                    "cert": cert_path,
                    "key": key_path,
                    "httpRedirectPort": http_port
                },
                "healthCheck": true
            }]
        }),
    );

    std::thread::sleep(std::time::Duration::from_millis(200));

    let resp = no_redirect_client()
        .get(format!("http://127.0.0.1:{http_port}/page?foo=bar&baz=1"))
        .send()
        .expect("GET");

    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    assert!(
        location.contains("foo=bar"),
        "query string must be preserved in redirect Location, got: '{location}'"
    );
}

// ── ACME HTTP-01 challenge passes through ─────────────────────────────────

#[test]
#[serial]
fn acme_challenge_path_returns_404_not_redirect() {
    // The redirect service must NOT redirect ACME challenge paths — it needs
    // to serve them directly so that the ACME CA can validate ownership.
    // Without a real ACME challenge token registered, the server returns 404
    // (no token found), but crucially NOT a 3xx redirect.
    let (_cert_dir, cert_path, key_path) = make_self_signed_cert();
    let tls_port = common::free_port();
    let http_port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        tls_port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": tls_port,
                "tls": {
                    "cert": cert_path,
                    "key": key_path,
                    "httpRedirectPort": http_port
                },
                "healthCheck": true
            }]
        }),
    );

    std::thread::sleep(std::time::Duration::from_millis(200));

    let resp = no_redirect_client()
        .get(format!(
            "http://127.0.0.1:{http_port}/.well-known/acme-challenge/sometoken"
        ))
        .send()
        .expect("GET");

    // Must NOT be a redirect — could be 200 (if token registered) or 404.
    let status = resp.status().as_u16();
    assert!(
        status != 301 && status != 302 && status != 307 && status != 308,
        "ACME challenge path must not be redirected, got status: {status}"
    );
}
