mod common;

use reqwest::blocking::Client;
use serial_test::serial;

#[test]
#[serial]
fn health_returns_200() {
    let server = common::TestServer::start_minimal();
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET /__health__");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["status"], "ok");
}

#[test]
#[serial]
fn unknown_path_returns_404() {
    let server = common::TestServer::start_minimal();
    let resp = reqwest::blocking::get(server.url("/does-not-exist")).expect("GET /does-not-exist");
    assert_eq!(resp.status(), 404);
}

#[test]
#[serial]
fn admin_status_returns_running() {
    let server = common::TestServer::start_minimal();
    let resp = reqwest::blocking::get(server.admin_url("/status")).expect("GET /status");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["status"], "running");
    // New fields added in this session.
    assert!(body["sites"].is_number(), "sites count should be present");
    assert!(body["inflight"].is_number(), "inflight should be present");
}

// ── Admin API token auth tests ────────────────────────────────────────────────

fn server_with_admin_token(token: &str) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": {
                "admin": {
                    "bind": format!("127.0.0.1:{admin_port}"),
                    "token": token
                }
            },
            "sites": [{ "port": port, "healthCheck": true }]
        }),
    )
}

#[test]
fn admin_token_correct_allows_access() {
    let srv = server_with_admin_token("my-secret-token");
    let resp = Client::new()
        .get(srv.admin_url("/status"))
        .header("authorization", "Bearer my-secret-token")
        .send()
        .expect("GET /status");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "correct token must be accepted"
    );
}

#[test]
fn admin_token_wrong_returns_401() {
    let srv = server_with_admin_token("correct-token");
    let resp = Client::new()
        .get(srv.admin_url("/status"))
        .header("authorization", "Bearer wrong-token")
        .send()
        .expect("GET /status");
    assert_eq!(resp.status().as_u16(), 401, "wrong token must return 401");
}

#[test]
fn admin_token_missing_returns_401() {
    let srv = server_with_admin_token("some-token");
    let resp = Client::new()
        .get(srv.admin_url("/status"))
        .send()
        .expect("GET /status without auth");
    assert_eq!(resp.status().as_u16(), 401, "missing token must return 401");
}

// ── POST /certs/reload ───────────────────────────────────────────────────────

/// Helper: generate a fresh self-signed (cert PEM, key PEM) pair.
fn gen_cert_pair() -> (String, String) {
    let kp = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&kp)
        .unwrap();
    (cert.pem(), kp.serialize_pem())
}

/// Helper: start a server with TLS configured, writing cert/key to temp files.
fn server_with_tls() -> (common::TestServer, std::path::PathBuf, std::path::PathBuf) {
    let port = common::free_port();
    let admin_port = common::free_port();
    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");

    let (cert_pem, key_pem) = gen_cert_pair();
    std::fs::write(&cert_path, &cert_pem).unwrap();
    std::fs::write(&key_path, &key_pem).unwrap();

    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "tls": {
                    "cert": cert_path.to_str().unwrap(),
                    "key":  key_path.to_str().unwrap()
                }
            }]
        }),
    );
    // Keep dir alive by leaking it (test process exits shortly after anyway).
    std::mem::forget(dir);
    (srv, cert_path, key_path)
}

#[test]
#[serial]
fn certs_reload_accepts_valid_pair() {
    let (srv, cert_path, key_path) = server_with_tls();
    let (new_cert, new_key) = gen_cert_pair();

    let resp = Client::new()
        .post(srv.admin_url("/certs/reload"))
        .json(&serde_json::json!({ "cert": new_cert, "key": new_key }))
        .send()
        .expect("POST /certs/reload");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "valid cert/key must return 200"
    );
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["cert_path"].is_string());
    assert!(body["key_path"].is_string());
    assert!(body["note"].as_str().unwrap().contains("restart"));

    // New cert must be on disk.
    let written = std::fs::read_to_string(&cert_path).unwrap();
    assert_eq!(written.trim(), new_cert.trim(), "cert file must be updated");
    let written_key = std::fs::read_to_string(&key_path).unwrap();
    assert_eq!(
        written_key.trim(),
        new_key.trim(),
        "key file must be updated"
    );
}

#[test]
#[serial]
fn certs_reload_rejects_mismatched_pair() {
    let (srv, _, _) = server_with_tls();
    let (cert, _) = gen_cert_pair();
    let (_, other_key) = gen_cert_pair();

    let resp = Client::new()
        .post(srv.admin_url("/certs/reload"))
        .json(&serde_json::json!({ "cert": cert, "key": other_key }))
        .send()
        .expect("POST /certs/reload mismatched");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "mismatched pair must return 400"
    );
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["status"], "error");
}

#[test]
#[serial]
fn certs_reload_rejects_garbage_pem() {
    let (srv, _, _) = server_with_tls();

    let resp = Client::new()
        .post(srv.admin_url("/certs/reload"))
        .json(&serde_json::json!({ "cert": "not a cert", "key": "not a key" }))
        .send()
        .expect("POST /certs/reload garbage");

    assert_eq!(resp.status().as_u16(), 400);
}

#[test]
#[serial]
fn certs_reload_returns_400_when_no_tls_configured() {
    // Site with no TLS — endpoint must explain there is nothing to rotate.
    let srv = common::TestServer::start_minimal();
    let (cert, key) = gen_cert_pair();

    let resp = Client::new()
        .post(srv.admin_url("/certs/reload"))
        .json(&serde_json::json!({ "cert": cert, "key": key }))
        .send()
        .expect("POST /certs/reload no-tls");

    assert_eq!(resp.status().as_u16(), 400, "no TLS site must return 400");
    let body: serde_json::Value = resp.json().unwrap();
    assert!(body["message"].as_str().unwrap().contains("tls.cert"));
}
