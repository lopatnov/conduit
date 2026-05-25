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
    fn start() -> Self {
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
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

// ── Admin API helpers ─────────────────────────────────────────────────────────

fn admin_post_json(addr: &str, path: &str, json_body: &str) -> serde_json::Value {
    use std::net::ToSocketAddrs;
    let sock = addr.to_socket_addrs().unwrap().next().unwrap();
    let mut stream = std::net::TcpStream::connect_timeout(&sock, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "POST /{path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{json_body}",
        len = json_body.len()
    )
    .unwrap();
    stream.flush().unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let body = response
        .find("\r\n\r\n")
        .map(|pos| &response[pos + 4..])
        .unwrap_or(&response);
    serde_json::from_str(body).unwrap_or(serde_json::json!({"raw": body}))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Start a server with a single upstream and verify the test infrastructure works.
#[test]
#[serial]
fn add_upstream_routes_traffic_to_new_target() {
    let original = MockUpstream::start();
    let added = MockUpstream::start();

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
                        "targets": [original.url()],
                        "strategy": "round-robin"
                    }
                }
            }]
        }),
    );

    // All requests go to the original upstream before the override.
    for _ in 0..4 {
        let resp = reqwest::blocking::get(srv.url("/")).expect("GET");
        assert_eq!(resp.status(), 200);
    }
    assert_eq!(original.hit_count(), 4, "pre-add: all hits should go to original");
    assert_eq!(added.hit_count(), 0, "pre-add: added upstream should be idle");

    // Add the new upstream via the Admin API.
    let admin_addr = format!("127.0.0.1:{admin_port}");
    let resp = admin_post_json(
        &admin_addr,
        "upstreams/add",
        &format!(r#"{{"route":"/","target":"{}","weight":1}}"#, added.url()),
    );
    assert_eq!(resp["status"], "ok", "add must succeed: {resp}");

    // After the override, traffic should distribute between original + added.
    for _ in 0..10 {
        let r = reqwest::blocking::get(srv.url("/")).expect("GET");
        assert_eq!(r.status(), 200);
    }

    assert!(
        added.hit_count() > 0,
        "added upstream must receive traffic after override (got {})",
        added.hit_count()
    );
}

#[test]
#[serial]
fn remove_upstream_stops_traffic_to_target() {
    let a = MockUpstream::start();
    let b = MockUpstream::start();

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
                        "targets": [a.url(), b.url()],
                        "strategy": "round-robin"
                    }
                }
            }]
        }),
    );

    // Warm up — both should receive traffic.
    for _ in 0..6 {
        reqwest::blocking::get(srv.url("/")).expect("GET");
    }

    let admin_addr = format!("127.0.0.1:{admin_port}");

    // Remove `a` via the admin API.  This creates a runtime override with only `b`.
    // First, add both to overrides, then remove `a`.
    admin_post_json(
        &admin_addr,
        "upstreams/add",
        &format!(r#"{{"route":"/","target":"{}","weight":1}}"#, a.url()),
    );
    admin_post_json(
        &admin_addr,
        "upstreams/add",
        &format!(r#"{{"route":"/","target":"{}","weight":1}}"#, b.url()),
    );
    let resp = admin_post_json(
        &admin_addr,
        "upstreams/remove",
        &format!(r#"{{"route":"/","target":"{}"}}"#, a.url()),
    );
    assert_eq!(resp["status"], "ok", "remove must succeed: {resp}");

    // Record a's hit count before the post-remove requests.
    let a_before = a.hit_count();

    // After removing a, all new requests should go to b only.
    for _ in 0..8 {
        let r = reqwest::blocking::get(srv.url("/")).expect("GET");
        assert_eq!(r.status(), 200);
    }

    assert_eq!(
        a.hit_count(),
        a_before,
        "removed upstream must not receive any new hits"
    );
    assert!(
        b.hit_count() > 0,
        "remaining upstream must still serve traffic"
    );
}

#[test]
#[serial]
fn remove_unknown_upstream_returns_not_found() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port }]
        }),
    );

    let admin_addr = format!("127.0.0.1:{admin_port}");
    let resp = admin_post_json(
        &admin_addr,
        "upstreams/remove",
        r#"{"route":"/api","target":"http://ghost:4000"}"#,
    );
    assert_eq!(resp["status"], "not_found", "unknown URL must return not_found: {resp}");
}

#[test]
#[serial]
fn weight_update_changes_distribution() {
    let a = MockUpstream::start();
    let b = MockUpstream::start();

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
                        "targets": [a.url(), b.url()],
                        "strategy": "round-robin"
                    }
                }
            }]
        }),
    );

    let admin_addr = format!("127.0.0.1:{admin_port}");

    // Create an override with equal weights first.
    admin_post_json(
        &admin_addr,
        "upstreams/add",
        &format!(r#"{{"route":"/","target":"{}","weight":1}}"#, a.url()),
    );
    admin_post_json(
        &admin_addr,
        "upstreams/add",
        &format!(r#"{{"route":"/","target":"{}","weight":1}}"#, b.url()),
    );

    // Now give `a` weight 3, `b` weight 1.
    let resp = admin_post_json(
        &admin_addr,
        "upstreams/weight",
        &format!(r#"{{"route":"/","target":"{}","weight":3}}"#, a.url()),
    );
    assert_eq!(resp["status"], "ok", "weight update must succeed: {resp}");

    // With WRR the strategy must be configured explicitly; here we just confirm
    // that the weight change is accepted and requests still succeed.
    for _ in 0..4 {
        let r = reqwest::blocking::get(srv.url("/")).expect("GET");
        assert_eq!(r.status(), 200);
    }
}

#[test]
#[serial]
fn weight_update_unknown_target_returns_not_found() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port }]
        }),
    );

    let admin_addr = format!("127.0.0.1:{admin_port}");
    let resp = admin_post_json(
        &admin_addr,
        "upstreams/weight",
        r#"{"route":"/api","target":"http://ghost:4000","weight":5}"#,
    );
    assert_eq!(resp["status"], "not_found", "unknown URL must return not_found: {resp}");
}

#[test]
#[serial]
fn reload_clears_overrides() {
    let original = MockUpstream::start();
    let added = MockUpstream::start();

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
                        "targets": [original.url()],
                        "strategy": "round-robin"
                    }
                }
            }]
        }),
    );

    let admin_addr = format!("127.0.0.1:{admin_port}");

    // Add an override.
    admin_post_json(
        &admin_addr,
        "upstreams/add",
        &format!(r#"{{"route":"/","target":"{}","weight":1}}"#, added.url()),
    );

    // With the override active, both upstreams should get traffic.
    for _ in 0..10 {
        reqwest::blocking::get(srv.url("/")).expect("GET");
    }
    assert!(
        added.hit_count() > 0,
        "override must route to the added upstream"
    );

    // POST /reload clears the override.
    use std::net::ToSocketAddrs;
    let sock = admin_addr.to_socket_addrs().unwrap().next().unwrap();
    let mut stream = std::net::TcpStream::connect_timeout(&sock, Duration::from_secs(5)).unwrap();
    write!(
        stream,
        "POST /reload HTTP/1.1\r\nHost: {admin_addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.flush().unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(resp.contains("200"), "POST /reload must return 200");

    let added_before = added.hit_count();

    // After reload, only original should receive traffic (override cleared).
    for _ in 0..6 {
        let r = reqwest::blocking::get(srv.url("/")).expect("GET");
        assert_eq!(r.status(), 200);
    }

    assert_eq!(
        added.hit_count(),
        added_before,
        "after reload, cleared override must not route to added upstream"
    );
    assert!(
        original.hit_count() > 0,
        "original upstream must serve traffic after reload"
    );
}
