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

#[test]
#[serial]
fn ip_filter_dry_run_logs_but_allows() {
    // A deny rule that *would* block the request must still let it through
    // when dryRun: true — only the enforcing (non-dry-run) path returns 403.
    let server = server_with_ip_filter(serde_json::json!({
        "deny": ["127.0.0.1", "::1"],
        "dryRun": true
    }));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(
        resp.status(),
        200,
        "dryRun: true must allow a request that would otherwise be denied"
    );
}

#[test]
#[serial]
fn ip_filter_dry_run_false_still_enforces() {
    // Sanity check for the test above: with dryRun explicitly false, the
    // same deny rule must actually block.
    let server = server_with_ip_filter(serde_json::json!({
        "deny": ["127.0.0.1", "::1"],
        "dryRun": false
    }));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 403);
}

// ── Dynamic deny list (Admin API POST/DELETE /ip-deny) ───────────────────────

#[test]
#[serial]
fn dynamic_deny_list_blocks_after_admin_add_and_unblocks_after_remove() {
    // No static ipFilter rules — starts open.
    let server = server_with_ip_filter(serde_json::json!({}));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 200, "no rules yet — must be open");

    let client = reqwest::blocking::Client::new();

    // Add both loopback forms via the Admin API so the test is portable
    // across IPv4/IPv6 test runners, same as the static-config tests above.
    for cidr in ["127.0.0.1", "::1"] {
        let resp = client
            .post(server.admin_url("/ip-deny"))
            .json(&serde_json::json!({ "cidr": cidr }))
            .send()
            .expect("POST /ip-deny");
        assert_eq!(resp.status(), 200, "POST /ip-deny for {cidr} must succeed");
    }

    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(
        resp.status(),
        403,
        "dynamic deny entry added via the Admin API must actually block IpGuard"
    );

    for cidr in ["127.0.0.1", "::1"] {
        let resp = client
            .delete(server.admin_url("/ip-deny"))
            .json(&serde_json::json!({ "cidr": cidr }))
            .send()
            .expect("DELETE /ip-deny");
        assert_eq!(
            resp.status(),
            200,
            "DELETE /ip-deny for {cidr} must succeed"
        );
    }

    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(
        resp.status(),
        200,
        "removing the dynamic deny entry must unblock again"
    );
}

#[test]
#[serial]
fn ip_deny_add_invalid_cidr_returns_400() {
    let server = server_with_ip_filter(serde_json::json!({}));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(server.admin_url("/ip-deny"))
        .json(&serde_json::json!({ "cidr": "not-an-ip" }))
        .send()
        .expect("POST /ip-deny");
    assert_eq!(
        resp.status(),
        400,
        "an invalid CIDR must be rejected with 400"
    );
    let body: serde_json::Value = resp.json().unwrap();
    assert_eq!(body["status"], "error");

    // The rejected entry must not have been added — the deny list is still
    // effectively empty, so a plain request must still pass.
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

#[test]
#[serial]
fn trust_proxy_true_without_xff_header_falls_back_to_direct_ip() {
    // trustProxy: true but the client sends no X-Forwarded-For header at all
    // (e.g. a direct connection, not behind the configured proxy). Must fall
    // back to the real connection IP rather than treating "no XFF" as
    // "unknown IP, fail open" — deny 127.0.0.1/::1 directly and confirm it's
    // still enforced.
    let server = server_with_ip_filter(serde_json::json!({
        "deny": ["127.0.0.1", "::1"],
        "trustProxy": true
    }));
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "trustProxy: true with no XFF header must still enforce against the direct IP"
    );
}
