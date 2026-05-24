mod common;

use common::{free_port, TestServer};
use serial_test::serial;

fn metrics_server(metrics: serde_json::Value) -> TestServer {
    let port = free_port();
    let admin_port = free_port();
    TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "metrics": metrics
            }]
        }),
    )
}

#[test]
#[serial]
fn metrics_endpoint_returns_200() {
    let srv = metrics_server(serde_json::json!({ "path": "/__metrics__" }));
    let resp = reqwest::blocking::get(srv.url("/__metrics__")).expect("send");
    assert_eq!(resp.status(), 200);
}

#[test]
#[serial]
fn metrics_content_type_is_prometheus_text() {
    let srv = metrics_server(serde_json::json!({ "path": "/__metrics__" }));
    let resp = reqwest::blocking::get(srv.url("/__metrics__")).expect("send");
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("text/plain"),
        "content-type should be Prometheus text format, got '{ct}'"
    );
}

#[test]
#[serial]
fn metrics_body_contains_prometheus_lines() {
    let srv = metrics_server(serde_json::json!({ "path": "/__metrics__" }));
    // Make a request first so counters are non-zero.
    let _ = reqwest::blocking::get(srv.url("/__health__"));
    let resp = reqwest::blocking::get(srv.url("/__metrics__")).expect("send");
    let body = resp.text().expect("body");
    // Prometheus text format always includes HELP and TYPE comment lines.
    assert!(
        body.contains("# HELP") || body.contains("# TYPE"),
        "metrics body should contain Prometheus text format markers (# HELP or # TYPE), got: {body}"
    );
}

#[test]
#[serial]
fn metrics_with_token_no_auth_returns_401() {
    let srv = metrics_server(serde_json::json!({
        "path": "/__metrics__",
        "token": "secret-token"
    }));
    let resp = reqwest::blocking::get(srv.url("/__metrics__")).expect("send");
    assert_eq!(resp.status(), 401);
}

#[test]
#[serial]
fn metrics_with_token_wrong_token_returns_401() {
    let srv = metrics_server(serde_json::json!({
        "path": "/__metrics__",
        "token": "correct-token"
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(srv.url("/__metrics__"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 401);
}

#[test]
#[serial]
fn metrics_with_token_correct_bearer_returns_200() {
    let srv = metrics_server(serde_json::json!({
        "path": "/__metrics__",
        "token": "my-secret"
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(srv.url("/__metrics__"))
        .header("Authorization", "Bearer my-secret")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 200);
}

#[test]
#[serial]
fn metrics_401_has_www_authenticate_header() {
    let srv = metrics_server(serde_json::json!({
        "path": "/__metrics__",
        "token": "tok"
    }));
    let resp = reqwest::blocking::get(srv.url("/__metrics__")).expect("send");
    assert_eq!(resp.status(), 401);
    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        www_auth.contains("Bearer"),
        "401 should carry WWW-Authenticate: Bearer, got '{www_auth}'"
    );
}

#[test]
#[serial]
fn metrics_custom_path_works() {
    let port = free_port();
    let admin_port = free_port();
    let srv = TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "metrics": { "path": "/internal/metrics" }
            }]
        }),
    );
    // Custom path should work.
    let resp = reqwest::blocking::get(srv.url("/internal/metrics")).expect("send");
    assert_eq!(resp.status(), 200, "custom metrics path should return 200");
    // Default path should NOT work.
    let resp2 = reqwest::blocking::get(srv.url("/__metrics__")).expect("send");
    assert_eq!(
        resp2.status(),
        404,
        "default path should return 404 when custom path configured"
    );
}
