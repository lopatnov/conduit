mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serial_test::serial;

// ── Mock upstream ─────────────────────────────────────────────────────────────

struct MockUpstream {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl MockUpstream {
    fn start(status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let inner_hits = hits_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let _ = stream.read(&mut buf);
                    inner_hits.fetch_add(1, Ordering::SeqCst);
                    let body = b"ok";
                    let resp = format!(
                        "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(body);
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

// ── Strategy: random ─────────────────────────────────────────────────────────

#[test]
#[serial]
fn strategy_random_distributes_to_upstreams() {
    let up1 = MockUpstream::start(200);
    let up2 = MockUpstream::start(200);
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
                        "targets": [up1.url(), up2.url()],
                        "strategy": "random"
                    }
                }
            }]
        }),
    );

    // Send 20 requests; with pseudo-random selection both upstreams should get hits.
    for _ in 0..20 {
        let resp = reqwest::blocking::get(srv.url("/")).expect("GET");
        assert_eq!(resp.status(), 200);
    }

    let total = up1.hit_count() + up2.hit_count();
    assert_eq!(total, 20, "all requests must reach an upstream");
    // Both must receive at least one request (very unlikely to all land on one).
    assert!(
        up1.hit_count() > 0,
        "upstream 1 should have received some traffic"
    );
    assert!(
        up2.hit_count() > 0,
        "upstream 2 should have received some traffic"
    );
}

// ── Strategy: least-conn ──────────────────────────────────────────────────────

#[test]
#[serial]
fn strategy_least_conn_returns_200() {
    let up1 = MockUpstream::start(200);
    let up2 = MockUpstream::start(200);
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
                        "targets": [up1.url(), up2.url()],
                        "strategy": "least-conn"
                    }
                }
            }]
        }),
    );

    // Send several sequential requests — each completes before the next starts,
    // so both upstreams will have 0 inflight when picked.  Verify all succeed.
    for _ in 0..10 {
        let resp = reqwest::blocking::get(srv.url("/")).expect("GET");
        assert_eq!(resp.status(), 200);
    }

    let total = up1.hit_count() + up2.hit_count();
    assert_eq!(total, 10, "all requests must reach an upstream");
}

// ── Health check: includeUpstreams ────────────────────────────────────────────

#[test]
#[serial]
fn health_include_upstreams_shows_status() {
    let up = MockUpstream::start(200);
    let port = common::free_port();
    let admin_port = common::free_port();

    // Use a very short health check interval so we get at least one probe
    // during the test.  unhealthyThreshold = 99 so the probe result alone
    // cannot flip the state to unhealthy; we're just checking the response shape.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": {
                    "includeUpstreams": true
                },
                "proxy": {
                    "/api": {
                        "targets": [up.url()],
                        "healthCheck": {
                            "path": "/",
                            "intervalSecs": 1,
                            "unhealthyThreshold": 99,
                            "healthyThreshold": 1
                        }
                    }
                }
            }]
        }),
    );

    // Give the first probe time to run.
    std::thread::sleep(Duration::from_millis(1500));

    let resp = reqwest::blocking::get(srv.url("/__health__")).expect("GET health");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("parse JSON");
    assert_eq!(body["status"], "ok", "status must be ok");
    assert!(
        body["upstreams"].is_object(),
        "upstreams field must be present and be an object, got: {body}"
    );
}

#[test]
#[serial]
fn health_without_include_upstreams_omits_field() {
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

    let resp = reqwest::blocking::get(srv.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("parse JSON");
    assert_eq!(body["status"], "ok");
    assert!(
        body.get("upstreams").is_none(),
        "upstreams key must be absent when not configured"
    );
}

// ── Admin API: /upstreams ─────────────────────────────────────────────────────

#[test]
#[serial]
fn admin_upstreams_initially_empty() {
    // Server with no health checks configured — statuses map stays empty.
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

    let resp = reqwest::blocking::get(srv.admin_url("/upstreams")).expect("GET /upstreams");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("parse JSON");
    assert!(body["upstreams"].is_array(), "upstreams must be an array");
    assert_eq!(
        body["upstreams"].as_array().unwrap().len(),
        0,
        "no health checks configured → empty list"
    );
}

#[test]
#[serial]
fn admin_upstreams_populated_after_health_check() {
    let up = MockUpstream::start(200);
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
                        "targets": [up.url()],
                        "healthCheck": {
                            "path": "/",
                            "intervalSecs": 1,
                            "unhealthyThreshold": 2,
                            "healthyThreshold": 1
                        }
                    }
                }
            }]
        }),
    );

    // Wait long enough for at least one probe.
    std::thread::sleep(Duration::from_millis(1500));

    let resp = reqwest::blocking::get(srv.admin_url("/upstreams")).expect("GET /upstreams");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("parse JSON");
    let list = body["upstreams"].as_array().expect("array");
    assert_eq!(list.len(), 1, "one upstream should be tracked");

    let entry = &list[0];
    assert_eq!(entry["url"].as_str().unwrap(), up.url());
    assert!(
        entry["healthy"].as_bool().unwrap(),
        "upstream should be healthy"
    );
    assert!(
        entry["latency_ms"].as_u64().is_some(),
        "latency_ms should be recorded after a successful probe"
    );
}

#[test]
#[serial]
fn upstream_marked_unhealthy_after_threshold_failures() {
    // Use a mock that always returns HTTP 500 — the health probe treats any
    // 5xx response as a failure without relying on TCP-level refusal.
    let bad_up = MockUpstream::start(500);
    let port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/api": {
                        "targets": [bad_up.url()],
                        "healthCheck": {
                            "path": "/",
                            // threshold = 1: first failure immediately marks unhealthy.
                            // This avoids races caused by uncertain task-startup timing.
                            "intervalSecs": 1,
                            "unhealthyThreshold": 1,
                            "healthyThreshold": 99
                        }
                    }
                }
            }]
        }),
    );

    // Give the health check task enough time to run at least one probe.
    // The first tick fires immediately (tokio::time::interval fires instantly),
    // so 1.5 s is more than enough even with late task scheduling.
    std::thread::sleep(Duration::from_millis(1500));

    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{admin_port}/upstreams"))
        .expect("GET /upstreams");
    let body: serde_json::Value = resp.json().expect("parse JSON");
    let list = body["upstreams"].as_array().expect("array");

    // The bad upstream must appear and be marked unhealthy.
    let entry = list
        .iter()
        .find(|e| e["url"].as_str() == Some(bad_up.url().as_str()))
        .expect("bad upstream should appear in the list");

    assert!(
        !entry["healthy"].as_bool().unwrap(),
        "upstream returning HTTP 500 should be marked unhealthy; entry: {entry}"
    );
    assert!(
        entry["consecutive_failures"].as_u64().unwrap() >= 1,
        "should have at least 1 consecutive failure"
    );
}

// ── Circuit Breaker: maxConnectionsPerUpstream ────────────────────────────────

/// Upstream that holds connections open until signalled (simulates long requests).
struct SlowUpstream {
    port: u16,
}

impl SlowUpstream {
    fn start(hold_ms: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let hold = hold_ms;
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    std::thread::sleep(Duration::from_millis(hold));
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                });
            }
        });
        SlowUpstream { port }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

#[test]
fn circuit_breaker_limits_connections() {
    // Upstream holds each connection for 3 seconds.
    let slow = SlowUpstream::start(3000);

    let port = common::free_port();
    let admin_port = common::free_port();
    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/": {
                        "targets": [ slow.url() ],
                        "healthCheck": { "maxConnectionsPerUpstream": 1 }
                    }
                }
            }]
        }),
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Fire the first request in the background — it will hold the connection.
    let url = format!("http://127.0.0.1:{port}/");
    let url2 = url.clone();
    std::thread::spawn(move || {
        let _ = client.get(&url2).send();
    });

    // Give the first request time to be in-flight.
    std::thread::sleep(Duration::from_millis(200));

    // Second request should hit the circuit breaker → 503.
    let client2 = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client2.get(&url).send().expect("GET /");
    assert_eq!(
        resp.status().as_u16(),
        503,
        "circuit breaker should return 503 when upstream is at max connections"
    );
}

#[test]
fn circuit_breaker_allows_when_under_limit() {
    // Upstream responds immediately.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
            });
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
                        "targets": [ format!("http://127.0.0.1:{upstream_port}") ],
                        "healthCheck": { "maxConnectionsPerUpstream": 10 }
                    }
                }
            }]
        }),
    );
    // Single request well under the limit — should succeed.
    let resp = reqwest::blocking::get(srv.url("/")).expect("GET /");
    assert_eq!(resp.status().as_u16(), 200, "under limit: should succeed");
}
