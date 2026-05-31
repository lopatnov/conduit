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
