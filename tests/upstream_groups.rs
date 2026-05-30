mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serial_test::serial;

// ── Labeled mock upstream ─────────────────────────────────────────────────────

struct MockUpstream {
    port: u16,
    hits: Arc<AtomicUsize>,
    label: &'static str,
}

impl MockUpstream {
    fn start(label: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let h = hits_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    h.fetch_add(1, Ordering::SeqCst);
                    let body = label;
                    let r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = s.write_all(r.as_bytes());
                });
            }
        });
        MockUpstream { port, hits, label }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn grouped_server(
    group_a_urls: Vec<String>,
    group_b_urls: Vec<String>,
    group_strategy: &str,
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
                        "groups": [
                            { "name": "group-a", "targets": group_a_urls, "strategy": "round-robin" },
                            { "name": "group-b", "targets": group_b_urls, "strategy": "round-robin" }
                        ],
                        "groupStrategy": group_strategy
                    }
                }
            }]
        }),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn groups_round_robin_distributes_across_groups() {
    let a1 = MockUpstream::start("a1");
    let b1 = MockUpstream::start("b1");

    let server = grouped_server(vec![a1.url()], vec![b1.url()], "round-robin");

    let client = reqwest::blocking::Client::new();
    // 4 requests: round-robin across 2 groups → 2 to each
    for _ in 0..4 {
        client.get(server.url("/")).send().expect("GET");
    }

    assert_eq!(a1.hits() + b1.hits(), 4, "all requests must be forwarded");
    assert!(
        a1.hits() > 0 && b1.hits() > 0,
        "group round-robin must distribute across groups (a={}, b={})",
        a1.hits(),
        b1.hits()
    );
}

#[test]
#[serial]
fn groups_ip_hash_routes_consistently_to_same_group() {
    let a1 = MockUpstream::start("a1");
    let b1 = MockUpstream::start("b1");

    let server = grouped_server(vec![a1.url()], vec![b1.url()], "ip-hash");

    let client = reqwest::blocking::Client::new();
    // All requests from 127.0.0.1 must land in the same group.
    for _ in 0..6 {
        client.get(server.url("/")).send().expect("GET");
    }

    let total = a1.hits() + b1.hits();
    assert_eq!(total, 6, "all 6 requests must be forwarded");
    assert!(
        a1.hits() == 6 || b1.hits() == 6,
        "ip-hash group strategy must route all requests to the same group \
         (a={}, b={})",
        a1.hits(),
        b1.hits()
    );
}

#[test]
#[serial]
fn groups_intra_group_round_robin_across_targets() {
    // Group A has two targets; all requests must be distributed within the group.
    let a1 = MockUpstream::start("a1");
    let a2 = MockUpstream::start("a2");
    let b1 = MockUpstream::start("b1_unused");

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
                        "groups": [
                            {
                                "name": "group-a",
                                "targets": [a1.url(), a2.url()],
                                "strategy": "round-robin"
                            },
                            { "name": "group-b", "targets": [b1.url()], "strategy": "round-robin" }
                        ],
                        // ip-hash with 2 groups → 127.0.0.1 always lands in one group
                        "groupStrategy": "ip-hash"
                    }
                }
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();
    for _ in 0..6 {
        client.get(server.url("/")).send().expect("GET");
    }

    // All 6 must land in whichever group 127.0.0.1 hashes to.
    let total = a1.hits() + a2.hits() + b1.hits();
    assert_eq!(total, 6);

    // One group got all 6 — within that group they must be distributed.
    if a1.hits() + a2.hits() == 6 {
        assert!(
            a1.hits() > 0 && a2.hits() > 0,
            "intra-group round-robin must spread load within group-a"
        );
    } else {
        assert_eq!(b1.hits(), 6, "all requests in group-b must go to b1");
    }
}
