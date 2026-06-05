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
