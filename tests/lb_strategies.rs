mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serial_test::serial;

// ── Mock upstream with hit counting ──────────────────────────────────────────

struct MockUpstream {
    port: u16,
    hits: Arc<AtomicUsize>,
    hold_ms: Arc<AtomicU64>,
}

impl MockUpstream {
    fn start(label: &'static str) -> Self {
        Self::start_holding(label, std::time::Duration::ZERO)
    }

    /// Like `start`, but holds every request for `hold` before answering, so that a burst of
    /// concurrent requests is all in flight at once.
    fn start_holding(label: &'static str, hold: std::time::Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let hold_ms = Arc::new(AtomicU64::new(hold.as_millis() as u64));
        let hold_clone = hold_ms.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let hits_inner = hits_clone.clone();
                let hold_inner = hold_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    hits_inner.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(
                        hold_inner.load(Ordering::SeqCst),
                    ));
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

        MockUpstream {
            port,
            hits,
            hold_ms,
        }
    }

    /// Change how long requests are held from now on.
    fn set_hold(&self, hold: std::time::Duration) {
        self.hold_ms
            .store(hold.as_millis() as u64, Ordering::SeqCst);
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

// ── Weighted least-conn (#506) ────────────────────────────────────────────────

#[test]
#[serial]
fn least_conn_shares_concurrent_requests_by_weight() {
    // Both upstreams hold every request for 2 s, so the 16 requests below are all in flight at the
    // moment each one is routed and none has finished: the in-flight counts only grow. With weights
    // 3:1 the split is then decided by the strategy alone: 12 and 4. Two requests routed in the same
    // instant can read the same counts (the pick and the increment are two steps), so allow one or
    // two to land on the other side; without weights the split would be 8 and 8.
    let hold = std::time::Duration::from_secs(2);
    let a = MockUpstream::start_holding("a", hold);
    let b = MockUpstream::start_holding("b", hold);

    let server = lb_server(serde_json::json!({
        "/": {
            "targets": [
                { "url": a.url(), "weight": 3 },
                { "url": b.url(), "weight": 1 }
            ],
            "strategy": "least-conn"
        }
    }));

    let client = reqwest::blocking::Client::new();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                client.get(server.url("/")).send().expect("GET");
            });
        }
    });

    let (hits_a, hits_b) = (a.hit_count(), b.hit_count());
    assert_eq!(hits_a + hits_b, 16, "every request must be forwarded");
    assert!(
        (11..=13).contains(&hits_a),
        "least-conn with weights 3:1 must give the weight-3 upstream about three quarters of the in-flight requests (a={hits_a}, b={hits_b})"
    );
}

// ── P2C counts requests in flight (#628) ──────────────────────────────────────

/// The sum of `active_connections` over every upstream `GET /upstreams` lists.
fn active_connections(server: &common::TestServer, client: &reqwest::blocking::Client) -> u64 {
    let body = client
        .get(server.admin_url("/upstreams"))
        .send()
        .expect("GET /upstreams")
        .text()
        .expect("body");
    let json: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    json["upstreams"]
        .as_array()
        .expect("upstreams list")
        .iter()
        .map(|u| u["active_connections"].as_u64().unwrap_or(0))
        .sum()
}

#[test]
#[serial]
fn p2c_holds_a_slot_while_a_request_is_in_flight_and_releases_it_after() {
    let a = MockUpstream::start("a");
    let b = MockUpstream::start("b");
    let server = lb_server(serde_json::json!({
        "/": { "targets": [a.url(), b.url()], "strategy": "p2c" }
    }));
    let client = reqwest::blocking::Client::new();

    // Warm up, so that both upstreams are listed by the admin API (an upstream is listed once it has a
    // recorded response). With two peers and equal start values the 20 picks all landing on one of them
    // has probability 2^-19.
    for _ in 0..20 {
        client.get(server.url("/")).send().expect("GET");
    }
    assert_eq!(
        active_connections(&server, &client),
        0,
        "nothing in flight yet"
    );

    // Eight requests, held for 2 s: while they run, the admin API must count eight in flight.
    a.set_hold(std::time::Duration::from_secs(2));
    b.set_hold(std::time::Duration::from_secs(2));
    let mut seen_during = 0;
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                client.get(server.url("/")).send().expect("GET");
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(700));
        seen_during = active_connections(&server, &client);
    });
    assert_eq!(seen_during, 8, "p2c must count each request in flight");

    // Every response is done; the slots are released when the request is logged, a moment later.
    let mut left = u64::MAX;
    for _ in 0..40 {
        left = active_connections(&server, &client);
        if left == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(left, 0, "p2c must release every slot it took");
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
