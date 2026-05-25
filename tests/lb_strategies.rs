mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serial_test::serial;

// ── Mock upstream with hit counting ──────────────────────────────────────────

struct MockUpstream {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl MockUpstream {
    fn start(label: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let hits_inner = hits_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    hits_inner.fetch_add(1, Ordering::SeqCst);
                    let body = label;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
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

fn lb_server(proxy_value: serde_json::Value) -> common::TestServer {
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

// ── Weighted round-robin ──────────────────────────────────────────────────────

#[test]
#[serial]
fn wrr_distributes_by_weight() {
    let a = MockUpstream::start("a");
    let b = MockUpstream::start("b");

    let server = lb_server(serde_json::json!({
        "/": {
            "targets": [
                { "url": a.url(), "weight": 3 },
                { "url": b.url(), "weight": 1 }
            ],
            "strategy": "weighted-round-robin"
        }
    }));

    let client = reqwest::blocking::Client::new();
    // Send 8 requests — a should get 6, b should get 2 (3:1 ratio).
    for _ in 0..8 {
        client.get(server.url("/")).send().expect("GET");
    }

    assert_eq!(a.hit_count(), 6, "a should receive 3/4 of requests");
    assert_eq!(b.hit_count(), 2, "b should receive 1/4 of requests");
}

// ── IP-hash ───────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn ip_hash_routes_consistently() {
    let a = MockUpstream::start("a");
    let b = MockUpstream::start("b");

    let server = lb_server(serde_json::json!({
        "/": {
            "targets": [a.url(), b.url()],
            "strategy": "ip-hash",
            "hash_key": "ip"
        }
    }));

    let client = reqwest::blocking::Client::new();
    // All requests come from the same IP (127.0.0.1), so they must all land
    // on the same upstream.  After N calls exactly one upstream has all hits.
    for _ in 0..6 {
        client.get(server.url("/")).send().expect("GET");
    }

    let total = a.hit_count() + b.hit_count();
    assert_eq!(total, 6);
    // One of them must have all 6 hits (same IP → same bucket).
    assert!(
        a.hit_count() == 6 || b.hit_count() == 6,
        "ip-hash must route all requests from 127.0.0.1 to the same upstream \
         (a={}, b={})",
        a.hit_count(),
        b.hit_count()
    );
}

// ── Consistent-hash ───────────────────────────────────────────────────────────

#[test]
#[serial]
fn consistent_hash_routes_consistently() {
    let a = MockUpstream::start("a");
    let b = MockUpstream::start("b");

    let server = lb_server(serde_json::json!({
        "/": {
            "targets": [a.url(), b.url()],
            "strategy": "consistent-hash",
            "hash_key": "ip"
        }
    }));

    let client = reqwest::blocking::Client::new();
    for _ in 0..6 {
        client.get(server.url("/")).send().expect("GET");
    }

    let total = a.hit_count() + b.hit_count();
    assert_eq!(total, 6);
    assert!(
        a.hit_count() == 6 || b.hit_count() == 6,
        "consistent-hash must route all requests from 127.0.0.1 to the same upstream"
    );
}

// ── Least-response-time ───────────────────────────────────────────────────────

#[test]
#[serial]
fn lrt_falls_back_gracefully_without_probe_data() {
    // Without health check configured there is no probe data — LRT should
    // fall back to round-robin and still distribute load.
    let a = MockUpstream::start("a");
    let b = MockUpstream::start("b");

    let server = lb_server(serde_json::json!({
        "/": {
            "targets": [a.url(), b.url()],
            "strategy": "least-response-time"
        }
    }));

    let client = reqwest::blocking::Client::new();
    for _ in 0..6 {
        client.get(server.url("/")).send().expect("GET");
    }

    let total = a.hit_count() + b.hit_count();
    assert_eq!(total, 6, "all requests must be forwarded");
    assert!(
        a.hit_count() > 0,
        "round-robin fallback must distribute to a"
    );
    assert!(
        b.hit_count() > 0,
        "round-robin fallback must distribute to b"
    );
}

// ── URL-hash (consistent-hash with hash_key: url) ────────────────────────────

#[test]
#[serial]
fn url_hash_routes_same_path_to_same_upstream() {
    let a = MockUpstream::start("a");
    let b = MockUpstream::start("b");

    let server = lb_server(serde_json::json!({
        "/": {
            "targets": [a.url(), b.url()],
            "strategy": "consistent-hash",
            "hash_key": "url"
        }
    }));

    let client = reqwest::blocking::Client::new();
    // Same path "/same" must always go to the same upstream.
    for _ in 0..6 {
        client.get(server.url("/same")).send().expect("GET");
    }

    let total = a.hit_count() + b.hit_count();
    assert_eq!(total, 6);
    assert!(
        a.hit_count() == 6 || b.hit_count() == 6,
        "same URL must always route to the same upstream"
    );
}
