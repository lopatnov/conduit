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

// ── trustProxy: X-Forwarded-For ──────────────────────────────────────────

#[test]
#[serial]
fn trust_proxy_xff_blocked_ip_returns_403() {
    // When trustProxy: true, the IP filter checks X-Forwarded-For instead of
    // the direct connection IP (127.0.0.1).  Block the spoofed IP "1.2.3.4".
    let server = server_with_ip_filter(serde_json::json!({
        "deny": ["1.2.3.4"],
        "trustProxy": true
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/"))
        .header("X-Forwarded-For", "1.2.3.4")
        .send()
        .expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "XFF IP 1.2.3.4 should be denied when trustProxy: true"
    );
}

#[test]
#[serial]
fn trust_proxy_xff_allowed_ip_passes() {
    // 127.0.0.1 is the direct connection IP. Deny it but allow 10.0.0.1 via XFF.
    let server = server_with_ip_filter(serde_json::json!({
        "allow": ["10.0.0.0/8"],
        "deny":  ["0.0.0.0/0"],
        "trustProxy": true
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/__health__"))
        .header("X-Forwarded-For", "10.1.2.3")
        .send()
        .expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "XFF IP 10.1.2.3 should pass the allow-list when trustProxy: true"
    );
}

/// Security: forged leftmost XFF must NOT bypass IP filtering.
///
/// Attack: attacker sends `X-Forwarded-For: <allowed-ip>`.
/// Trusted proxy appends the real client IP (test: 127.0.0.1).
/// With rightmost XFF semantics, 127.0.0.1 is used → blocked by the allowlist.
#[test]
#[serial]
fn trust_proxy_forged_leftmost_xff_does_not_bypass_filter() {
    // Allow only 10.0.0.0/8 — localhost (127.0.0.1) is NOT in the allowed range.
    let server = server_with_ip_filter(serde_json::json!({
        "allow": ["10.0.0.0/8"],
        "trustProxy": true
    }));
    let client = reqwest::blocking::Client::new();
    // Attacker forges the first (leftmost) XFF entry to look like an allowed IP.
    // The second entry simulates what a real trusted proxy would append (127.0.0.1).
    let resp = client
        .get(server.url("/__health__"))
        .header("X-Forwarded-For", "10.1.2.3, 127.0.0.1")
        .send()
        .expect("GET");
    // With rightmost semantics: 127.0.0.1 is used → NOT in 10.0.0.0/8 → 403.
    // Old leftmost semantics would have used 10.1.2.3 → ALLOWED (bypass!).
    assert_eq!(
        resp.status().as_u16(),
        403,
        "forged leftmost XFF must not bypass the IP allowlist; rightmost (127.0.0.1) must be used"
    );
}

#[test]
#[serial]
fn trust_proxy_false_uses_direct_ip_not_xff() {
    // When trustProxy is NOT set (default false), X-Forwarded-For is ignored
    // and the real connection IP (127.0.0.1) is used for filtering.
    let server = server_with_ip_filter(serde_json::json!({
        "deny": ["1.2.3.4"]
        // trustProxy not set → false
    }));
    let client = reqwest::blocking::Client::new();
    // Even though XFF says 1.2.3.4, the real IP is 127.0.0.1 → should pass.
    let resp = client
        .get(server.url("/__health__"))
        .header("X-Forwarded-For", "1.2.3.4")
        .send()
        .expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "without trustProxy, XFF should be ignored and 127.0.0.1 passes the deny:1.2.3.4 rule"
    );
}
