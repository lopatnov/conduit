mod common;

use serial_test::serial;

fn server_with_ip_filter(ip_filter: serde_json::Value) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "ipFilter": ip_filter }]
        }),
    )
}

fn server_with_limits(limits: serde_json::Value) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "limits": limits }]
        }),
    )
}

// ── IP filter ──────────────────────────────────────────────────────────────

#[test]
#[serial]
fn ip_deny_loopback_returns_403() {
    // Deny both IPv4 and IPv6 loopback so the test is portable
    let server = server_with_ip_filter(serde_json::json!({
        "deny": ["127.0.0.1", "::1"]
    }));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 403);
}

#[test]
#[serial]
fn ip_allow_whitelist_blocks_loopback() {
    // Only 10.x.x.x is allowed — loopback is not in that range
    let server = server_with_ip_filter(serde_json::json!({
        "allow": ["10.0.0.0/8"]
    }));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 403);
}

#[test]
#[serial]
fn ip_allow_loopback_cidr_passes() {
    let server = server_with_ip_filter(serde_json::json!({
        "allow": ["127.0.0.0/8", "::1/128"]
    }));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    // Should be 200 (health passes) not 403
    assert_eq!(resp.status(), 200);
}

#[test]
#[serial]
fn ip_no_filter_allows_all() {
    let server = server_with_ip_filter(serde_json::json!({}));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 200);
}

// ── Request limits ─────────────────────────────────────────────────────────

#[test]
#[serial]
fn limits_body_too_large_returns_413() {
    let server = server_with_limits(serde_json::json!({ "maxBodyBytes": 10 }));
    let client = reqwest::blocking::Client::new();
    // Send 100 bytes — exceeds the 10-byte limit declared via Content-Length
    let resp = client
        .post(server.url("/upload"))
        .body("x".repeat(100))
        .send()
        .expect("POST");
    assert_eq!(resp.status(), 413);
}

#[test]
#[serial]
fn limits_small_body_passes() {
    let server = server_with_limits(serde_json::json!({ "maxBodyBytes": 10000 }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(server.url("/anything"))
        .body("small")
        .send()
        .expect("POST");
    // Should not be 413 (may be 404 since no route, but not rejected by limits)
    assert_ne!(resp.status(), 413u16);
}

#[test]
#[serial]
fn limits_headers_too_large_returns_431() {
    // Set a very small header limit — any real request will exceed it
    let server = server_with_limits(serde_json::json!({ "maxHeaderBytes": 50 }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/anything"))
        .header("x-big", "a".repeat(200))
        .send()
        .expect("GET");
    assert_eq!(resp.status(), 431);
}

#[test]
#[serial]
fn limits_health_exempt_from_body_limit() {
    // Health endpoint bypasses limits
    let server = server_with_limits(serde_json::json!({ "maxBodyBytes": 1 }));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 200);
}
