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
        Self::start_with(label, hold, 0)
    }

    /// Like `start`, but answers with a body of `body_len` bytes instead of the label.
    fn start_sized(label: &'static str, body_len: usize) -> Self {
        Self::start_with(label, std::time::Duration::ZERO, body_len)
    }

    /// The general constructor: answer with the label or with a body of `body_len` bytes, after holding for `hold`.
    fn start_with(label: &'static str, hold: std::time::Duration, body_len: usize) -> Self {
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
                    let body = if body_len == 0 {
                        label.to_owned()
                    } else {
                        "x".repeat(body_len)
                    };
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

    let client = common::local_client();
    // Send 8 requests — a should get 6, b should get 2 (3:1 ratio).
    for _ in 0..8 {
        client.get(server.url("/")).send().expect("GET");
    }

    assert_eq!(a.hit_count(), 6, "a should receive 3/4 of requests");
    assert_eq!(b.hit_count(), 2, "b should receive 1/4 of requests");
}

// ── Weighted least-conn (#506) ────────────────────────────────────────────────

/// Sixteen requests held open for 2 s are all in flight when routed: weights 3:1 give the heavy upstream about 12.
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

    let client = common::local_client();
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

// ── Least-bandwidth (#627) ────────────────────────────────────────────────────

/// One upstream answers 256 KiB, the other a few bytes: after the first windows nearly every request goes to the small one.
#[test]
#[serial]
fn least_bandwidth_steers_requests_away_from_the_upstream_moving_more_bytes() {
    // One upstream answers with 256 KiB, the other with a few bytes. Requests are paced over about
    // 3 seconds so that the traffic meters (refreshed at most every 250 ms) see the difference.
    // round-robin would split 50/50; here the big one keeps only the requests that arrived before
    // its first refresh showed its rate.
    let big = MockUpstream::start_sized("big", 256 * 1024);
    let small = MockUpstream::start("small");
    let server = lb_server(serde_json::json!({
        "/": { "targets": [big.url(), small.url()], "strategy": "least-bandwidth" }
    }));
    let client = common::local_client();
    for _ in 0..120 {
        client
            .get(server.url("/"))
            .send()
            .expect("GET")
            .bytes()
            .expect("body");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let (hits_big, hits_small) = (big.hit_count(), small.hit_count());
    eprintln!("least-bandwidth split: big={hits_big}, small={hits_small}");
    assert_eq!(hits_big + hits_small, 120);
    assert!(
        hits_big <= 40 && hits_small >= 80,
        "the upstream moving 256 KiB per request should get far fewer requests (big={hits_big}, small={hits_small})"
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
    slot_is_held_while_in_flight_and_released_after("p2c");
}

/// Issue #627: `least-bandwidth` counts requests in flight like `p2c`, so it must release them too.
#[test]
#[serial]
fn least_bandwidth_holds_a_slot_while_a_request_is_in_flight_and_releases_it_after() {
    slot_is_held_while_in_flight_and_released_after("least-bandwidth");
}

/// Warm up, hold eight requests and read `active_connections` from the admin API (8 while held), then wait for it to return to 0.
fn slot_is_held_while_in_flight_and_released_after(strategy: &str) {
    let a = MockUpstream::start("a");
    let b = MockUpstream::start("b");
    let server = lb_server(serde_json::json!({
        "/": { "targets": [a.url(), b.url()], "strategy": strategy }
    }));
    let client = common::local_client();

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
    assert_eq!(
        seen_during, 8,
        "{strategy} must count each request in flight"
    );

    // Every response is done; the slots are released when the request is logged, a moment later.
    let mut left = u64::MAX;
    for _ in 0..40 {
        left = active_connections(&server, &client);
        if left == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(left, 0, "{strategy} must release every slot it took");
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

    let client = common::local_client();
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

    let client = common::local_client();
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

    let client = common::local_client();
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

    let client = common::local_client();
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
