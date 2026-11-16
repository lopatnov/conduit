mod common;

use std::io::Write;

use common::{new_ca, new_client_cert, write_ca_pem};
use serial_test::serial;
use tempfile::TempDir;

/// A self-signed server cert for `localhost`/127.0.0.1 (same as tests/tls.rs).
fn make_server_cert(dir: &TempDir) -> (String, String) {
    use rcgen::{generate_simple_self_signed, CertifiedKey};

    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .expect("rcgen server cert");

    let cert_path = dir.path().join("server-cert.pem");
    let key_path = dir.path().join("server-key.pem");
    std::fs::File::create(&cert_path)
        .expect("create server-cert.pem")
        .write_all(cert.pem().as_bytes())
        .expect("write server-cert.pem");
    std::fs::File::create(&key_path)
        .expect("create server-key.pem")
        .write_all(signing_key.serialize_pem().as_bytes())
        .expect("write server-key.pem");

    (
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    )
}

/// A reqwest client presenting the given client cert/key as its TLS identity,
/// accepting any server certificate (self-signed server cert in tests).
fn client_with_identity(cert_pem: &str, key_pem: &str) -> reqwest::blocking::Client {
    let identity_pem = format!("{key_pem}\n{cert_pem}");
    let identity = reqwest::Identity::from_pem(identity_pem.as_bytes()).expect("build identity");
    common::local_client_builder()
        .danger_accept_invalid_certs(true)
        .identity(identity)
        .build()
        .expect("reqwest client with identity")
}

fn insecure_client_no_identity() -> reqwest::blocking::Client {
    common::local_client_builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("reqwest client")
}

/// Starts a conduit server with `tls.clientAuth: { ca, optional }`.
fn start_mtls_server(
    port: u16,
    admin_port: u16,
    ca_path: &str,
    optional: bool,
) -> common::TestServer {
    let dir = TempDir::new().expect("tempdir for server cert");
    let (cert_path, key_path) = make_server_cert(&dir);
    // Leak the tempdir so its files outlive this function — the server process
    // reads them for the lifetime of the test.
    std::mem::forget(dir);

    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "tls": {
                    "cert": cert_path,
                    "key": key_path,
                    "clientAuth": { "ca": ca_path, "optional": optional }
                },
                "healthCheck": true
            }]
        }),
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn mtls_required_rejects_missing_client_cert() {
    let (ca_cert, _issuer) = new_ca();
    let dir = TempDir::new().expect("tempdir");
    let ca_path = write_ca_pem(&dir, &ca_cert);

    let port = common::free_port();
    let admin_port = common::free_port();
    let server = start_mtls_server(port, admin_port, &ca_path, false);

    let url = format!("https://127.0.0.1:{}/__health__", server.port);
    let result = insecure_client_no_identity().get(&url).send();
    assert!(
        result.is_err(),
        "request without a client cert must be rejected when clientAuth.optional=false"
    );
}

#[test]
#[serial]
fn mtls_required_accepts_valid_client_cert() {
    let (ca_cert, issuer) = new_ca();
    let (client_cert, client_key) = new_client_cert(&issuer);
    let dir = TempDir::new().expect("tempdir");
    let ca_path = write_ca_pem(&dir, &ca_cert);

    let port = common::free_port();
    let admin_port = common::free_port();
    let server = start_mtls_server(port, admin_port, &ca_path, false);

    let url = format!("https://127.0.0.1:{}/__health__", server.port);
    let client = client_with_identity(&client_cert.pem(), &client_key.serialize_pem());
    let resp = client
        .get(&url)
        .send()
        .expect("HTTPS GET with valid client cert");
    assert_eq!(resp.status(), 200);
}

#[test]
#[serial]
fn mtls_required_rejects_untrusted_ca_client_cert() {
    let (ca_cert, _issuer) = new_ca();
    let dir = TempDir::new().expect("tempdir");
    let ca_path = write_ca_pem(&dir, &ca_cert);

    // A second, unrelated CA signs the client cert presented to the server —
    // the server only trusts the first CA.
    let (_other_ca_cert, other_issuer) = new_ca();
    let (client_cert, client_key) = new_client_cert(&other_issuer);

    let port = common::free_port();
    let admin_port = common::free_port();
    let server = start_mtls_server(port, admin_port, &ca_path, false);

    let url = format!("https://127.0.0.1:{}/__health__", server.port);
    let client = client_with_identity(&client_cert.pem(), &client_key.serialize_pem());
    let result = client.get(&url).send();
    assert!(
        result.is_err(),
        "a client cert signed by an untrusted CA must be rejected"
    );
}

#[test]
#[serial]
fn mtls_optional_accepts_missing_client_cert() {
    let (ca_cert, _issuer) = new_ca();
    let dir = TempDir::new().expect("tempdir");
    let ca_path = write_ca_pem(&dir, &ca_cert);

    let port = common::free_port();
    let admin_port = common::free_port();
    let server = start_mtls_server(port, admin_port, &ca_path, true);

    let url = format!("https://127.0.0.1:{}/__health__", server.port);
    let resp = insecure_client_no_identity()
        .get(&url)
        .send()
        .expect("HTTPS GET without client cert, optional mTLS");
    assert_eq!(resp.status(), 200);
}

#[test]
#[serial]
fn mtls_optional_accepts_valid_client_cert() {
    let (ca_cert, issuer) = new_ca();
    let (client_cert, client_key) = new_client_cert(&issuer);
    let dir = TempDir::new().expect("tempdir");
    let ca_path = write_ca_pem(&dir, &ca_cert);

    let port = common::free_port();
    let admin_port = common::free_port();
    let server = start_mtls_server(port, admin_port, &ca_path, true);

    let url = format!("https://127.0.0.1:{}/__health__", server.port);
    let client = client_with_identity(&client_cert.pem(), &client_key.serialize_pem());
    let resp = client
        .get(&url)
        .send()
        .expect("HTTPS GET with valid client cert, optional mTLS");
    assert_eq!(resp.status(), 200);
}
