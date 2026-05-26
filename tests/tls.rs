mod common;

use std::io::Write;

use serial_test::serial;
use tempfile::TempDir;

// ── Helper: generate a self-signed cert pair for testing ──────────────────────

/// Generates a self-signed TLS certificate for `localhost` using `rcgen` and
/// writes `cert.pem` / `key.pem` into a temporary directory.
/// Returns the temp dir (kept alive for the test's duration) and the file paths.
fn make_self_signed_cert() -> (TempDir, String, String) {
    use rcgen::{generate_simple_self_signed, CertifiedKey};

    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .expect("rcgen cert generation");

    let dir = tempfile::tempdir().expect("tempdir");

    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");

    std::fs::File::create(&cert_path)
        .expect("create cert.pem")
        .write_all(cert.pem().as_bytes())
        .expect("write cert.pem");

    std::fs::File::create(&key_path)
        .expect("create key.pem")
        .write_all(signing_key.serialize_pem().as_bytes())
        .expect("write key.pem");

    (
        dir,
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    )
}

/// A reqwest HTTPS client that accepts any certificate (for self-signed certs in tests).
fn insecure_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("reqwest client")
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn tls_https_get_returns_200() {
    let (_dir, cert_path, key_path) = make_self_signed_cert();

    let port = common::free_port();
    let admin_port = common::free_port();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "tls": { "cert": cert_path, "key": key_path },
                "healthCheck": true
            }]
        }),
    );

    // The health endpoint should be reachable over HTTPS.
    let url = format!("https://127.0.0.1:{}/__health__", server.port);
    let resp = insecure_client().get(&url).send().expect("HTTPS GET");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["status"], "ok");
}

#[test]
#[serial]
fn tls_http2_negotiated() {
    let (_dir, cert_path, key_path) = make_self_signed_cert();

    let port = common::free_port();
    let admin_port = common::free_port();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "tls": { "cert": cert_path, "key": key_path },
                "http2": { "maxConcurrentStreams": 100 },
                "healthCheck": true
            }]
        }),
    );

    // The async reqwest client properly negotiates HTTP/2 via ALPN during the TLS
    // handshake.  The blocking client pools connections differently and may fall back
    // to HTTP/1.1 even when the server offers "h2" in ALPN.
    let url = format!("https://127.0.0.1:{}/__health__", server.port);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (status, version) = rt.block_on(async move {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("async h2 client");
        let resp = client.get(&url).send().await.expect("HTTPS/2 GET");
        (resp.status().as_u16(), resp.version())
    });

    assert_eq!(status, 200);
    assert_eq!(version, reqwest::Version::HTTP_2);
}

#[test]
#[serial]
fn tls_plain_http_site_unaffected() {
    // A site without `tls` config should still work over plain HTTP.
    let port = common::free_port();
    let admin_port = common::free_port();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "healthCheck": true }]
        }),
    );

    let resp = reqwest::blocking::get(server.url("/__health__")).expect("HTTP GET");
    assert_eq!(resp.status(), 200);
}
