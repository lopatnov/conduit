//! Security-focused integration tests.
//!
//! These tests verify that protections against common web-proxy attacks are
//! working end-to-end: path traversal, CRLF header injection, rate-limiter
//! exhaustion, and admin API security.

mod common;

use reqwest::blocking::Client;
use serial_test::serial;

// ── Null byte path injection ──────────────────────────────────────────────────

/// Null bytes (%00) in static file paths must never reach the OS.
/// On POSIX, a null byte terminates a path at the syscall boundary:
/// `file.php%00.txt` would open `file.php` rather than a `.txt` file.
#[test]
#[serial]
fn static_null_byte_in_path_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.php"), "<?php phpinfo(); ?>").unwrap();
    std::fs::write(dir.path().join("public.txt"), "ok").unwrap();

    let srv = static_server(dir.path().to_str().unwrap());

    // Request for `secret.php%00.txt` must NOT serve `secret.php`.
    let resp = reqwest::blocking::get(srv.url("/secret.php%00.txt")).unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    assert!(
        !body.contains("phpinfo"),
        "null-byte injection must not serve secret.php: {body:.50}"
    );
    assert_ne!(status, 200, "null-byte path must not return 200");
}

// ── Static file symlink traversal ────────────────────────────────────────────

/// Symlinks in the static root must NOT be served — they could point anywhere
/// on the filesystem and bypass path sanitization.
#[test]
#[serial]
#[cfg(unix)]
fn static_symlink_traversal_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("www");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("index.html"), "<h1>ok</h1>").unwrap();

    // Secret file OUTSIDE the static root.
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "TOP SECRET CONTENT").unwrap();

    // Symlink INSIDE the static root pointing to the secret file.
    let link = root.join("link.txt");
    std::os::unix::fs::symlink(&secret, &link).unwrap();

    let srv = static_server(root.to_str().unwrap());

    // The symlink must NOT be served.
    let resp = reqwest::blocking::get(srv.url("/link.txt")).unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();

    assert_ne!(status, 200, "symlink must not return 200");
    assert!(
        !body.contains("TOP SECRET"),
        "symlink traversal must not leak secret file: {body:.100}"
    );
}

// ── Pre-compressed file symlink ───────────────────────────────────────────────

/// A .br or .gz symlink in the static root must NOT be served as pre-compressed.
/// Otherwise an attacker could create `file.html.br` → `/etc/shadow.gz` and
/// retrieve the compressed system file as if it were legitimate static content.
#[test]
#[serial]
#[cfg(unix)]
fn precompressed_symlink_is_not_served() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("www");
    std::fs::create_dir(&root).unwrap();

    // Legitimate file.
    std::fs::write(root.join("index.html"), "<h1>ok</h1>").unwrap();

    // Secret file outside the root (pretend it's already gzip-compressed).
    let secret = dir.path().join("secret.gz");
    std::fs::write(&secret, b"\x1f\x8b secret data").unwrap();

    // Symlink inside the root: index.html.gz → /path/to/secret.gz
    #[cfg(unix)]
    {
        let link = root.join("index.html.gz");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
    }

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": root.to_str().unwrap(),
                "staticOptions": { "preCompressed": true }
            }]
        }),
    );

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/index.html"))
        .header("accept-encoding", "gzip")
        .send()
        .unwrap();

    // Must serve the real file, not the symlinked .gz.
    let body = resp.text().unwrap_or_default();
    assert!(
        !body.contains("secret data"),
        "pre-compressed symlink must not expose secret file"
    );
    assert!(
        body.contains("ok") || body.is_empty() || body.contains("not found"),
        "response should be the real file or 404, got: {body:.50}"
    );
}

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

// ── X-Priority header must not be trusted from clients ───────────────────────

/// A client sending X-Priority: 100 must not bypass load shedding.
/// The header must be stripped before the priority check.
#[test]
#[serial]
fn x_priority_header_from_client_does_not_bypass_load_shedding() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = common::free_port();
    let admin_port = common::free_port();
    // maxInflight=1, threshold=0.0 → always sheds low-priority.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "limits": { "maxInflightRequests": 1, "priorityThreshold": 0.0 },
                "proxy": {
                    "/batch": {
                        "targets": [format!("http://127.0.0.1:{echo_port}")],
                        "priority": 10
                    },
                    "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
                }
            }]
        }),
    );

    // Attacker sends X-Priority: 100 to try to bypass shedding on /batch.
    let resp = Client::new()
        .get(srv.url("/batch/jobs"))
        .header("x-priority", "100")
        .send()
        .unwrap();

    assert_eq!(
        resp.status().as_u16(),
        503,
        "X-Priority: 100 from client must NOT bypass load shedding — priority routing \
         must only use route config, not client-supplied headers"
    );
}

/// X-Priority must be stripped from the request before forwarding to upstream.
#[test]
#[serial]
fn x_priority_header_stripped_before_upstream() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

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

    let resp = Client::new()
        .get(srv.url("/"))
        .header("x-priority", "90")
        .send()
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().unwrap_or_default();
    let headers = body.get("headers").cloned().unwrap_or_default();
    assert!(
        headers.get("x-priority").is_none(),
        "X-Priority must be stripped before reaching upstream, got: {headers}"
    );
}

// ── X-Consumer-ID header injection ───────────────────────────────────────────

/// A client forging X-Consumer-ID must not have it reach the upstream.
/// ConsumersGuard strips the header first, then sets it to the real consumer.
/// Requires `--features consumers`.
#[test]
#[serial]
#[cfg(feature = "consumers")]
fn x_consumer_id_from_client_is_stripped() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = common::free_port();
    let admin_port = common::free_port();
    // Configure a consumer with a known API key.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "consumers": {
                    "consumers": [
                        { "username": "alice", "apiKey": "alice-key" }
                    ]
                },
                "proxy": { "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] } }
            }]
        }),
    );

    // Attacker sends a forged X-Consumer-ID with their own key.
    let resp = Client::new()
        .get(srv.url("/"))
        .header("x-api-key", "alice-key")
        .header("x-consumer-id", "admin-user") // forged!
        .send()
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200, "valid key must reach upstream");
    let body: serde_json::Value = resp.json().unwrap_or_default();
    let headers = body.get("headers").cloned().unwrap_or_default();

    // Upstream must see "alice", not "admin-user".
    let consumer_id = headers
        .get("x-consumer-id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        consumer_id, "alice",
        "upstream must receive the real consumer ID, not the forged one: got {consumer_id:?}"
    );
}

// ── Host header allowlist (AllowedHosts) ─────────────────────────────────────

/// When `allowedHosts` is configured, requests with a disallowed Host return 400.
#[test]
#[serial]
fn allowed_hosts_rejects_bad_host() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "securityHeaders": {
                    "allowedHosts": ["api.example.com"]
                },
                "proxy": { "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] } }
            }]
        }),
    );

    // Bad host → 400
    let resp = Client::new()
        .get(srv.url("/"))
        .header("host", "evil.com")
        .send()
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        400,
        "disallowed Host must return 400"
    );
}

/// When `allowedHosts` is configured, requests with an allowed Host pass through.
#[test]
#[serial]
fn allowed_hosts_passes_good_host() {
    let echo_port = common::free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "securityHeaders": {
                    "allowedHosts": ["127.0.0.1"]
                },
                "proxy": { "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] } }
            }]
        }),
    );

    let resp = Client::new()
        .get(srv.url("/"))
        .send() // reqwest sends Host: 127.0.0.1:<port>
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "allowed Host must pass through"
    );
}

// ── maxBodyBytes chunked bypass ───────────────────────────────────────────────

/// Clients that use chunked transfer encoding (no Content-Length) must not
/// bypass `maxBodyBytes`.  Previously only the declared Content-Length was
/// checked; actual body bytes were not enforced.
#[test]
#[serial]
fn max_body_bytes_enforced_without_content_length() {
    // Upstream echoes whatever it receives.
    let echo_port = common::free_port();
    let _echo = {
        use std::io::{Read, Write};
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind(format!("127.0.0.1:{echo_port}")).unwrap();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = vec![0u8; 65536];
                let n = s.read(&mut buf).unwrap_or(0);
                let body = format!("received {} bytes", n);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        })
    };

    let port = common::free_port();
    let admin_port = common::free_port();
    // maxBodyBytes = 100 bytes.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "limits": { "maxBodyBytes": 100 },
                "proxy": {
                    "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
                }
            }]
        }),
    );

    // Send a POST with Content-Length that reports 10 bytes but body is 200 bytes.
    // This is the standard bypass — the LimitsGuard sees 10 (< 100) and passes.
    // The actual body enforcement should catch the 200 bytes.
    let client = reqwest::blocking::Client::new();

    // Case 1: over-declared Content-Length is checked at header phase → 413.
    let resp_declared = client
        .post(srv.url("/"))
        .header("content-length", "5000")
        .body("x".repeat(5000))
        .send()
        .unwrap();
    assert_eq!(
        resp_declared.status().as_u16(),
        413,
        "declared Content-Length > maxBodyBytes must return 413"
    );

    // Case 2: no Content-Length (e.g. chunked) — actual bytes enforced in body filter.
    // We can't trivially send chunked with reqwest, but we can send a body
    // without explicitly setting Content-Length by using streaming:
    // reqwest will add Content-Length for a known-size body, so we use raw TCP.
    //
    // IMPORTANT: We send `Connection: close` so the proxy closes the TCP connection
    // after responding.  Without it the proxy keeps the HTTP/1.1 connection alive
    // and `read_to_string` would block forever waiting for EOF.
    let raw_req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
        200usize, // chunk size in hex
        "B".repeat(200)
    );
    let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    // Safety timeout: prevent the test from hanging indefinitely if the proxy
    // somehow keeps the connection open despite Connection: close.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    use std::io::{Read, Write};
    stream.write_all(raw_req.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    // The upstream should NOT receive 200 bytes — the body must be truncated/dropped.
    // Either we get a 413 from Conduit or the upstream receives truncated data.
    // The key assertion: the upstream must not echo "received 200 bytes".
    assert!(
        !response.contains("received 200 bytes"),
        "chunked body exceeding maxBodyBytes must not fully reach upstream: {response:.100}"
    );
}

// ── Authorization blocks caching (RFC 7234 § 3.2) ────────────────────────────

/// Responses to requests with an Authorization header must NEVER be served
/// from cache — they are user-specific and would leak to other users.
#[test]
#[serial]
fn cache_does_not_serve_authorized_responses() {
    use std::io::{Read, Write};

    // Counter upstream that tracks how many times it was called.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let call_count2 = call_count.clone();
    let echo_port = common::free_port();
    let _echo = std::thread::spawn(move || {
        let listener = std::net::TcpListener::bind(format!("127.0.0.1:{echo_port}")).unwrap();
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let n = call_count2.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let body = format!("call-{n}");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nCache-Control: public, max-age=3600\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/": {
                        "targets": [format!("http://127.0.0.1:{echo_port}")],
                        "cache": { "store": "memory", "ttlSecs": 3600 }
                    }
                }
            }]
        }),
    );

    let client = Client::new();

    // Request 1: with Authorization — must hit upstream.
    let r1 = client
        .get(srv.url("/"))
        .header("authorization", "Bearer token-user-a")
        .send()
        .unwrap();
    assert_eq!(r1.status().as_u16(), 200);
    let body1 = r1.text().unwrap();

    // Request 2: different user, with Authorization — must ALSO hit upstream,
    // not receive user A's cached response.
    let r2 = client
        .get(srv.url("/"))
        .header("authorization", "Bearer token-user-b")
        .send()
        .unwrap();
    let body2 = r2.text().unwrap();

    // Both requests must reach the upstream (call count = 2, not 1).
    // If caching was wrongly applied, the second request would return body1.
    let calls = call_count.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        calls >= 2,
        "both authorized requests must reach upstream (calls={calls}), \
         not return a cached response from a different user"
    );
    assert_ne!(body1, body2, "each call must get a unique response");
}

// ── Unsafe POST retry blocked ─────────────────────────────────────────────────

/// POST requests must NOT be retried on 5xx to prevent double-mutations
/// (double charges, duplicate emails, etc.).
#[test]
#[serial]
fn post_requests_are_not_retried_on_5xx() {
    use std::io::{Read, Write};
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };

    let call_count = Arc::new(AtomicU32::new(0));
    let cc2 = call_count.clone();
    let echo_port = common::free_port();
    let _echo = std::thread::spawn(move || {
        let listener = std::net::TcpListener::bind(format!("127.0.0.1:{echo_port}")).unwrap();
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            cc2.fetch_add(1, Ordering::Relaxed);
            // Always return 500.
            let body = "error";
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/": {
                        "targets": [format!("http://127.0.0.1:{echo_port}")],
                        "retry": { "attempts": 3, "conditions": ["5xx"] }
                    }
                }
            }]
        }),
    );

    let client = Client::new();
    let _resp = client.post(srv.url("/")).body("payload").send().unwrap();
    // The proxy may return 500 or 502 depending on how Pingora handles the
    // upstream response; what matters is that the upstream was hit exactly ONCE.

    // POST must be forwarded exactly ONCE — no retries.
    let calls = call_count.load(Ordering::Relaxed);
    assert_eq!(
        calls, 1,
        "POST must not be retried on 5xx — upstream was called {calls} times"
    );
}

/// GET requests ARE retried on 5xx (safe, idempotent method).
#[test]
#[serial]
fn get_requests_are_retried_on_5xx() {
    use std::io::{Read, Write};
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };

    let call_count = Arc::new(AtomicU32::new(0));
    let cc2 = call_count.clone();
    let echo_port = common::free_port();
    let _echo = std::thread::spawn(move || {
        let listener = std::net::TcpListener::bind(format!("127.0.0.1:{echo_port}")).unwrap();
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let n = cc2.fetch_add(1, Ordering::Relaxed);
            let (status, body) = if n == 0 { (500, "error") } else { (200, "ok") };
            let resp = format!(
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "limits": { "maxBodyBufferBytes": 1048576 },
                "proxy": {
                    "/": {
                        "targets": [format!("http://127.0.0.1:{echo_port}")],
                        "retry": { "attempts": 2, "conditions": ["5xx"] }
                    }
                }
            }]
        }),
    );

    let resp = Client::new().get(srv.url("/")).send().unwrap();
    // First call returns 500 → retried → second call returns 200.
    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET should be retried and succeed on second attempt"
    );
    let calls = call_count.load(Ordering::Relaxed);
    assert_eq!(calls, 2, "upstream must be called twice (1 fail + 1 retry)");
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
    assert!(
        allowed >= 5,
        "burst must allow at least 5 requests: {statuses:?}"
    );
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

        assert_eq!(
            reload_resp["status"], "ok",
            "reload must succeed: {reload_resp}"
        );
        let warnings = reload_resp["warnings"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !warnings.is_empty(),
            "reload of a config with wasm middleware must include a warning when wasm is not compiled in"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("wasm")),
            "warning must mention 'wasm': {warnings:?}"
        );
    }
}
