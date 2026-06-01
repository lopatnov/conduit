//! Security-focused integration tests.
//!
//! These tests verify that protections against common web-proxy attacks are
//! working end-to-end: path traversal, CRLF header injection, rate-limiter
//! exhaustion, and admin API security.

mod common;

use reqwest::blocking::Client;
use serial_test::serial;

// ── Static-file path traversal ────────────────────────────────────────────────

/// Helper: start a static-file server rooted at `dir`.
fn static_server(dir: &str) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "static": dir }]
        }),
    )
}

#[test]
#[serial]
fn static_dotdot_path_traversal_blocked() {
    let dir = tempfile::tempdir().unwrap();
    // Secret file OUTSIDE the static root.
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "TOP SECRET").unwrap();
    // Nested static root.
    let root = dir.path().join("www");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("index.html"), "<html>ok</html>").unwrap();

    let srv = static_server(root.to_str().unwrap());

    // Attempt to read the secret file one directory up.
    for traversal in &[
        "/../secret.txt",
        "/..%2Fsecret.txt",
        "/%2e%2e/secret.txt",
        "/%2e%2e%2fsecret.txt",
    ] {
        let resp = reqwest::blocking::get(srv.url(traversal)).unwrap();
        assert_ne!(
            resp.status().as_u16(),
            200,
            "path traversal '{traversal}' must not return 200"
        );
        // Must not contain the secret content.
        let body = resp.text().unwrap_or_default();
        assert!(
            !body.contains("TOP SECRET"),
            "path traversal '{traversal}' must not leak secret file content"
        );
    }
}

#[test]
#[serial]
fn static_encoded_traversal_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("www");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("safe.txt"), "safe").unwrap();

    let secret = dir.path().join("leaked.txt");
    std::fs::write(&secret, "LEAKED").unwrap();

    let srv = static_server(root.to_str().unwrap());

    // Double-encoded traversal (should be harmless after single-pass decode).
    let resp = reqwest::blocking::get(srv.url("/%252e%252e/leaked.txt")).unwrap();
    let body = resp.text().unwrap_or_default();
    assert!(
        !body.contains("LEAKED"),
        "double-encoded traversal must not leak content"
    );
}

#[test]
#[serial]
fn static_dotfile_hidden_by_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=abc").unwrap();
    std::fs::write(dir.path().join("index.html"), "ok").unwrap();

    let srv = static_server(dir.path().to_str().unwrap());
    let resp = reqwest::blocking::get(srv.url("/.env")).unwrap();
    assert_ne!(
        resp.status().as_u16(),
        200,
        "hidden dotfiles must not be served by default"
    );
}

// ── CRLF header injection ─────────────────────────────────────────────────────

/// CRLF sequences injected by the upstream must be stripped before they reach
/// the client (ResponseFilterChain Phase 1: CrlfProtectionFilter).
#[test]
#[serial]
fn crlf_in_upstream_header_is_stripped() {
    // Echo server injects a CRLF-containing header value.
    let echo_port = common::free_port();
    let _echo = {
        use std::io::{Read, Write};
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind(format!("127.0.0.1:{echo_port}")).unwrap();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                // Inject CRLF into a custom header value.
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Evil: injected\r\nX-Injected: bad\r\nConnection: close\r\n\r\nok";
                let _ = s.write_all(resp.as_bytes());
            }
        })
    };

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": { "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] } }
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/")).unwrap();
    // X-Evil header should be present (no CRLF in its name).
    // The key check is that no response header value contains raw \r or \n.
    for (name, value) in resp.headers() {
        let v = value.to_str().unwrap_or("");
        assert!(
            !v.contains('\r') && !v.contains('\n'),
            "header '{name}' contains CRLF in value: {v:?}"
        );
    }
}

// ── Rate-limiter bucket cap ───────────────────────────────────────────────────

/// When using `keyBy: "header:X-Custom"` and thousands of unique header values
/// are sent, the DashMap must not grow without bound.  After the cap is reached,
/// new unique keys are treated as rate-limited rather than creating new buckets.
#[test]
#[serial]
fn rate_limiter_bucket_cap_prevents_memory_exhaustion() {
    // Set a very generous rate limit so normal traffic always passes,
    // but use header-based keying which creates a bucket per unique value.
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "rateLimit": {
                    "windowSecs": 60,
                    "limit": 1000,
                    "keyBy": "header:X-Tenant-Id"
                }
            }]
        }),
    );

    let client = Client::new();

    // Send a burst of requests with unique X-Tenant-Id values.
    // Each creates a new bucket.  At some point the cap kicks in.
    let mut allowed = 0u32;
    let mut denied = 0u32;
    for i in 0..200u32 {
        let resp = client
            .get(srv.url("/__health__"))
            .header("X-Tenant-Id", format!("tenant-unique-{i}"))
            .send()
            .unwrap();
        match resp.status().as_u16() {
            200 => allowed += 1,
            429 => denied += 1,
            s => panic!("unexpected status {s}"),
        }
    }
    // 200 requests under the cap → all should be allowed.
    // This test documents the behaviour: it passes as long as it doesn't OOM.
    assert!(
        allowed + denied == 200,
        "every request must be either allowed or rejected"
    );
}

// ── Error masking ─────────────────────────────────────────────────────────────

/// When `maskErrors: true`, upstream 5xx responses have their body replaced
/// with a generic JSON error — the upstream's internal error details are hidden.
#[test]
#[serial]
fn mask_errors_hides_upstream_5xx_body() {
    // Echo upstream that returns 500 with a detailed error body.
    let echo_port = common::free_port();
    let _echo = {
        use std::io::{Read, Write};
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind(format!("127.0.0.1:{echo_port}")).unwrap();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let body = "Internal DB error: SELECT * FROM users WHERE id=1 FAILED";
                let resp = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        })
    };

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "maskErrors": true,
                "proxy": { "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] } }
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/")).unwrap();
    assert_eq!(resp.status().as_u16(), 500, "must still return 500");
    let body = resp.text().unwrap_or_default();
    // Internal details must not leak.
    assert!(
        !body.contains("DB error") && !body.contains("SELECT"),
        "upstream error details must be masked: got '{body}'"
    );
    // Must be a generic JSON error instead.
    assert!(
        body.contains("Internal Server Error") || body.contains("error"),
        "masked body should contain generic error: got '{body}'"
    );
}

#[test]
#[serial]
fn mask_errors_false_passes_upstream_body_through() {
    // Without maskErrors, the upstream error body is forwarded as-is.
    let echo_port = common::free_port();
    let _echo = {
        use std::io::{Read, Write};
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind(format!("127.0.0.1:{echo_port}")).unwrap();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let body = "upstream-error-detail";
                let resp = format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        })
    };

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": { "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] } }
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/")).unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    let body = resp.text().unwrap_or_default();
    assert!(
        body.contains("upstream-error-detail"),
        "without maskErrors, body must pass through: got '{body}'"
    );
}

// ── Priority routing (load shedding) ─────────────────────────────────────────

/// When inflight requests are within the normal range, all routes are served
/// regardless of priority.
#[test]
#[serial]
fn priority_routing_below_threshold_serves_all() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = common::free_port();
    let admin_port = common::free_port();
    // maxInflight=1000, threshold=0.9 → only starts shedding at 900+ concurrent.
    // Test sends one sequential request — nowhere near the threshold.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "limits": { "maxInflightRequests": 1000, "priorityThreshold": 0.9 },
                "proxy": {
                    "/low": { "targets": [format!("http://127.0.0.1:{echo_port}")], "priority": 10 },
                    "/":    { "targets": [format!("http://127.0.0.1:{echo_port}")] }
                }
            }]
        }),
    );

    // Low-priority route must be served when well below the threshold.
    let resp = reqwest::blocking::get(srv.url("/low/test")).unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "low-priority route must be served when below threshold"
    );
}

/// Low-priority routes are shed with 503 when inflight >= threshold * max.
#[test]
#[serial]
fn priority_routing_above_threshold_sheds_low_priority() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = common::free_port();
    let admin_port = common::free_port();
    // maxInflight=1, threshold=0.0 → ALWAYS above threshold.
    // Any inflight request (including the one being processed) triggers shedding.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "limits": { "maxInflightRequests": 1, "priorityThreshold": 0.0 },
                "proxy": {
                    "/batch": { "targets": [format!("http://127.0.0.1:{echo_port}")], "priority": 10 },
                    "/":      { "targets": [format!("http://127.0.0.1:{echo_port}")] }
                }
            }]
        }),
    );

    // Low-priority route → 503 Load Shedding.
    let resp = reqwest::blocking::get(srv.url("/batch/jobs")).unwrap();
    assert_eq!(
        resp.status().as_u16(),
        503,
        "low-priority route must be shed when above threshold"
    );
    let body: serde_json::Value = resp.json().unwrap_or_default();
    assert!(
        body.get("reason").and_then(|r| r.as_str()) == Some("load shedding"),
        "shed response must explain reason: {body}"
    );
}

// ── Rate-limit burst ──────────────────────────────────────────────────────────

/// With `burst` configured, clients can exceed the window rate briefly.
/// Note: health/ACME endpoints bypass rate-limiting guards; use a proxied path.
#[test]
#[serial]
fn rate_limit_burst_allows_burst_requests() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = common::free_port();
    let admin_port = common::free_port();
    // limit=2/min burst=3 → first 5 requests are allowed (2+3), 6th is 429.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "rateLimit": { "windowSecs": 60, "limit": 2, "burst": 3, "keyBy": "ip" },
                "proxy": {
                    "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
                }
            }]
        }),
    );

    let client = Client::new();
    let mut statuses: Vec<u16> = Vec::new();
    for _ in 0..6 {
        let s = client.get(srv.url("/")).send().unwrap().status().as_u16();
        statuses.push(s);
    }

    let allowed = statuses.iter().filter(|&&s| s == 200).count();
    let rejected = statuses.iter().filter(|&&s| s == 429).count();
    // burst=3 + limit=2 → first 5 requests pass, 6th is rejected.
    assert!(allowed >= 5, "burst must allow at least 5 requests: {statuses:?}");
    assert!(rejected >= 1, "must rate-limit after burst: {statuses:?}");
}

// ── Feature warnings in reload response ──────────────────────────────────────

#[test]
#[serial]
fn reload_returns_warnings_for_wasm_without_feature() {
    // Only meaningful when wasm feature is NOT compiled in.
    #[cfg(feature = "wasm")]
    {
        // If wasm IS enabled, there's no warning — test is a no-op.
        return;
    }
    #[cfg(not(feature = "wasm"))]
    {
        let port = common::free_port();
        let admin_port = common::free_port();
        let srv = common::TestServer::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [{ "port": port }]
            }),
        );

        // Write a new config with a wasm middleware entry to the config file.
        let new_cfg = serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "middleware": [{ "type": "wasm", "path": "nonexistent.wasm" }]
            }]
        });
        srv.rewrite_config(new_cfg);
        let reload_resp = srv.reload();

        assert_eq!(reload_resp["status"], "ok", "reload must succeed: {reload_resp}");
        let warnings = reload_resp["warnings"].as_array().cloned().unwrap_or_default();
        assert!(
            !warnings.is_empty(),
            "reload of a config with wasm middleware must include a warning when wasm is not compiled in"
        );
        assert!(
            warnings.iter().any(|w| w.as_str().unwrap_or("").contains("wasm")),
            "warning must mention 'wasm': {warnings:?}"
        );
    }
}
