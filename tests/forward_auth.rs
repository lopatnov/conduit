/// Integration tests for Forward Auth and Header Transform features.
mod common;

use reqwest::blocking::Client;

fn plain_client() -> Client {
    Client::new()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Spawn a simple mock auth server.
///
/// `auth_response_status` determines what status it replies with (200 = pass,
/// 401/403 = deny).  The server also echoes an `X-User-ID: mock-user` header
/// so forward-auth response-header forwarding can be tested.
fn spawn_mock_auth_server(auth_response_status: u16) -> std::net::SocketAddr {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let status_line = format!(
        "HTTP/1.1 {status} {reason}\r\n",
        status = auth_response_status,
        reason = if auth_response_status == 200 {
            "OK"
        } else {
            "Unauthorized"
        }
    );
    std::thread::spawn(move || {
        // Accept multiple connections so tests can reuse the server.
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            // Drain the request.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                format!("{status_line}X-User-ID: mock-user\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            );
        }
    });
    addr
}

/// Spawn a mock auth server that replies 200 with no custom headers at all —
/// simulates an auth service that doesn't return the configured
/// `responseHeaders` name for this session (e.g. an anonymous/guest auth
/// result).
fn spawn_mock_auth_server_no_headers() -> std::net::SocketAddr {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        }
    });
    addr
}

// ── Forward Auth tests ────────────────────────────────────────────────────────

#[test]
fn forward_auth_pass_allows_request() {
    let auth_addr = spawn_mock_auth_server(200);
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
                "forwardAuth": {
                    "url": format!("http://{auth_addr}/auth"),
                    "timeoutMs": 3000
                }
            }]
        }),
    );
    // Request gets through auth (200) — no upstream configured → 404 fallback, not 401.
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_ne!(
        resp.status().as_u16(),
        401,
        "forward-auth 200 should allow request"
    );
}

#[test]
fn forward_auth_deny_returns_401() {
    let auth_addr = spawn_mock_auth_server(401);
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "forwardAuth": {
                    "url": format!("http://{auth_addr}/auth"),
                    "timeoutMs": 3000
                }
            }]
        }),
    );
    let resp = plain_client()
        .get(srv.url("/protected"))
        .send()
        .expect("GET /");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "forward-auth 401 should deny request"
    );
}

#[test]
fn forward_auth_skip_path_bypasses() {
    let auth_addr = spawn_mock_auth_server(401);
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
                "forwardAuth": {
                    "url": format!("http://{auth_addr}/auth"),
                    "skipPaths": ["/__health__"],
                    "timeoutMs": 3000
                }
            }]
        }),
    );
    // Health endpoint skips forward-auth.
    let resp = plain_client()
        .get(srv.url("/__health__"))
        .send()
        .expect("health");
    assert_eq!(resp.status().as_u16(), 200);
}

#[test]
fn forward_auth_unreachable_denies() {
    // Port 1 is almost certainly not listening → connection refused → fail closed.
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "forwardAuth": {
                    "url": "http://127.0.0.1:1/auth",
                    "timeoutMs": 500
                }
            }]
        }),
    );
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "unreachable auth service must fail closed (401)"
    );
}

/// A client-forged identity header must never reach the upstream unchanged
/// when the auth service doesn't return that header itself. Regression test
/// for the header-spoofing bug: `forward_auth_inject_response_headers` used
/// to only *insert* headers the auth service returned, leaving any
/// client-supplied value for a configured (but not returned) header name
/// completely untouched — an attacker could set `X-User-ID: admin` and have
/// it forwarded to the upstream as if the auth service had vouched for it.
#[test]
fn forward_auth_strips_forged_header_when_auth_omits_it() {
    let auth_addr = spawn_mock_auth_server_no_headers();
    let (echo_port, _echo) = common::start_echo_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "forwardAuth": {
                    "url": format!("http://{auth_addr}/auth"),
                    "responseHeaders": ["X-User-ID"],
                    "timeoutMs": 3000
                },
                "proxy": format!("http://127.0.0.1:{echo_port}")
            }]
        }),
    );
    let resp = plain_client()
        .get(srv.url("/"))
        .header("X-User-ID", "forged-admin")
        .send()
        .expect("GET /");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().expect("echo body");
    assert!(
        body["headers"]["x-user-id"].is_null(),
        "the client-forged X-User-ID must be stripped, not forwarded — got: {body}"
    );
}

/// The legitimate case still works: when the auth service *does* return the
/// configured header, its value reaches the upstream (overwriting whatever
/// the client sent, if anything).
#[test]
fn forward_auth_injects_header_the_auth_service_returns() {
    let auth_addr = spawn_mock_auth_server(200); // echoes X-User-ID: mock-user
    let (echo_port, _echo) = common::start_echo_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "forwardAuth": {
                    "url": format!("http://{auth_addr}/auth"),
                    "responseHeaders": ["X-User-ID"],
                    "timeoutMs": 3000
                },
                "proxy": format!("http://127.0.0.1:{echo_port}")
            }]
        }),
    );
    let resp = plain_client()
        .get(srv.url("/"))
        .header("X-User-ID", "forged-admin")
        .send()
        .expect("GET /");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().expect("echo body");
    assert_eq!(
        body["headers"]["x-user-id"], "mock-user",
        "the auth-service-returned value must reach upstream, overwriting the client's, got: {body}"
    );
}

// ── Header Transform tests ────────────────────────────────────────────────────

#[test]
fn response_transform_injects_custom_header() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // Upstream returns a bare 200 with no special headers.
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in upstream.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
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
                "proxy": format!("http://{upstream_addr}"),
                "responseTransform": {
                    "setHeaders": { "X-Served-By": "conduit-test" },
                    "removeHeaders": ["X-Powered-By"]
                }
            }]
        }),
    );
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("x-served-by")
            .and_then(|v| v.to_str().ok()),
        Some("conduit-test"),
        "responseTransform.setHeaders should inject X-Served-By"
    );
}
