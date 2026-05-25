mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serial_test::serial;

// ── Mock upstream ─────────────────────────────────────────────────────────

struct MockUpstream {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl MockUpstream {
    /// Start a TCP server that returns a fixed HTTP/1.1 response body.
    fn start(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let hits_inner = hits_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let _ = stream.read(&mut buf);
                    hits_inner.fetch_add(1, Ordering::SeqCst);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });

        MockUpstream { port, hits }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

// ── Helper: start conduit with a proxy config ──────────────────────────────

fn proxy_server(proxy_value: serde_json::Value) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "proxy": proxy_value }]
        }),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn proxy_single_string_passthrough() {
    let upstream = MockUpstream::start("hello from upstream");
    let server = proxy_server(serde_json::json!(upstream.url()));

    let resp = reqwest::blocking::get(server.url("/anything")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "hello from upstream");
    assert_eq!(upstream.hit_count(), 1);
}

#[test]
#[serial]
fn proxy_routes_map_path_matching() {
    let api = MockUpstream::start("api response");
    let server = proxy_server(serde_json::json!({
        "/api": api.url()
    }));

    // Path that matches /api → proxied
    let resp = reqwest::blocking::get(server.url("/api/users")).expect("GET /api/users");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "api response");

    // Path that doesn't match → 404 fallback
    let resp = reqwest::blocking::get(server.url("/other")).expect("GET /other");
    assert_eq!(resp.status(), 404);
}

#[test]
#[serial]
fn proxy_round_robin_distributes_requests() {
    let upstream_a = MockUpstream::start("server-a");
    let upstream_b = MockUpstream::start("server-b");
    let server = proxy_server(serde_json::json!({
        "/": [upstream_a.url(), upstream_b.url()]
    }));

    // Send 4 requests — round-robin should split 2/2
    for _ in 0..4 {
        let _ = reqwest::blocking::get(server.url("/ping")).expect("GET");
    }

    let a = upstream_a.hit_count();
    let b = upstream_b.hit_count();
    assert_eq!(a + b, 4, "total hits should be 4");
    assert_eq!(a, 2, "upstream A should get 2 requests");
    assert_eq!(b, 2, "upstream B should get 2 requests");
}

#[test]
#[serial]
fn proxy_strip_prefix_rewrites_path() {
    // Mock upstream captures request path from the HTTP request line
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let received_path = Arc::new(std::sync::Mutex::new(String::new()));
    let received_clone = received_path.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let captured = received_clone.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                // Extract path from "GET /path HTTP/1.1"
                if let Some(line) = request.lines().next() {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        *captured.lock().unwrap() = parts[1].to_string();
                    }
                }
                let body = "ok";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });

    let upstream_url = format!("http://127.0.0.1:{port}");
    let conduit_port = common::free_port();
    let admin_port = common::free_port();
    let server = common::TestServer::start_with_config(
        conduit_port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": conduit_port,
                "proxy": {
                    "/api": {
                        "targets": [upstream_url],
                        "stripPrefix": true
                    }
                }
            }]
        }),
    );

    let resp = reqwest::blocking::get(server.url("/api/users")).expect("GET");
    assert_eq!(resp.status(), 200);

    // Poll until the upstream thread captures the path (up to 1 s).
    let path = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let p = received_path.lock().unwrap().clone();
            if !p.is_empty() || std::time::Instant::now() >= deadline {
                break p;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    assert_eq!(path, "/users", "upstream should see /users, not /api/users");
}

#[test]
#[serial]
fn proxy_x_forwarded_for_header_set() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let xff_received = Arc::new(std::sync::Mutex::new(String::new()));
    let xff_clone = xff_received.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let captured = xff_clone.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                for line in request.lines() {
                    let lower = line.to_lowercase();
                    if lower.starts_with("x-forwarded-for:") {
                        *captured.lock().unwrap() = line
                            .split_once(':')
                            .map(|(_, v)| v.trim().to_string())
                            .unwrap_or_default();
                        break;
                    }
                }
                let body = "ok";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
            });
        }
    });

    let server = proxy_server(serde_json::json!(format!("http://127.0.0.1:{port}")));
    let _ = reqwest::blocking::get(server.url("/check")).expect("GET");

    // Poll until the upstream thread captures the header (up to 1 s).
    let xff = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let v = xff_received.lock().unwrap().clone();
            if !v.is_empty() || std::time::Instant::now() >= deadline {
                break v;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    assert!(!xff.is_empty(), "X-Forwarded-For header should be set");
}

#[test]
#[serial]
fn proxy_health_not_forwarded_to_upstream() {
    let upstream = MockUpstream::start("upstream body");
    let server = proxy_server(serde_json::json!(upstream.url()));

    // Health check should be served locally, not forwarded
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET /__health__");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON");
    assert_eq!(body["status"], "ok");
    // Upstream should NOT have been hit
    assert_eq!(
        upstream.hit_count(),
        0,
        "health endpoint must not reach upstream"
    );
}

// ── Retry tests ───────────────────────────────────────────────────────────────

/// Helper: start Conduit with a route that has retry config.
fn proxy_server_with_retry(
    targets: serde_json::Value,
    conditions: &[&str],
    attempts: u32,
) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/": {
                        "targets": targets,
                        "retry": {
                            "attempts": attempts,
                            "conditions": conditions
                        }
                    }
                }
            }]
        }),
    )
}

#[test]
#[serial]
fn proxy_retry_connection_error_falls_through_to_working_upstream() {
    // Pick a port that is not bound — connections to it will be refused immediately.
    let dead_port = common::free_port();
    let working = MockUpstream::start("retry-ok");

    let server = proxy_server_with_retry(
        serde_json::json!([format!("http://127.0.0.1:{dead_port}"), working.url()]),
        &["connection_error"],
        2,
    );

    let resp = reqwest::blocking::get(server.url("/test")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "retry-ok");
    assert_eq!(
        working.hit_count(),
        1,
        "working upstream should be hit once after retry"
    );
}

#[test]
#[serial]
fn proxy_retry_no_retry_without_condition() {
    // Both dead_port and working are in the targets list, but no retry is
    // configured.  Round-robin always starts at index 0 (dead_port), so the
    // first — and only — attempt fails with 502.  working must not be contacted.
    let dead_port = common::free_port();
    let working = MockUpstream::start("should-not-reach");

    let port = common::free_port();
    let admin_port = common::free_port();
    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/": {
                        "targets": [
                            format!("http://127.0.0.1:{dead_port}"),
                            working.url()
                        ]
                    }
                }
            }]
        }),
    );

    let resp = reqwest::blocking::get(server.url("/test")).expect("GET");
    // Without retry, Pingora returns 502 Bad Gateway for a refused connection.
    assert_eq!(resp.status(), 502);
    assert_eq!(
        working.hit_count(),
        0,
        "working upstream must not be reached"
    );
}

#[test]
#[serial]
fn proxy_retry_on_5xx_falls_through_to_working_upstream() {
    // First upstream always returns 500.
    let bad_listener = TcpListener::bind("127.0.0.1:0").expect("bind bad upstream");
    let bad_port = bad_listener.local_addr().unwrap().port();
    let bad_hit = Arc::new(AtomicBool::new(false));
    let bad_hit_clone = bad_hit.clone();

    std::thread::spawn(move || {
        for stream in bad_listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let flag = bad_hit_clone.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                flag.store(true, Ordering::SeqCst);
                let _ = stream.write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            });
        }
    });

    let good = MockUpstream::start("5xx-retry-ok");

    let server = proxy_server_with_retry(
        serde_json::json!([format!("http://127.0.0.1:{bad_port}"), good.url()]),
        &["5xx"],
        2,
    );

    let resp = reqwest::blocking::get(server.url("/anything")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "5xx-retry-ok");
    assert!(
        bad_hit.load(Ordering::SeqCst),
        "bad upstream should have been hit"
    );
    assert_eq!(
        good.hit_count(),
        1,
        "good upstream should be hit after 5xx retry"
    );
}
