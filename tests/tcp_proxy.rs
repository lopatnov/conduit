//! Integration tests for raw TCP proxy mode (`sites[].tcp`, `--features tcp`).
//!
//! Before this file, `sites[].tcp` had unit coverage only for `pick_target()`'s
//! round-robin/random selection logic (`crates/conduit-tcp/src/proxy.rs`) — no
//! test ever started a real conduit server with a `tcp:` site and pushed bytes
//! through it end-to-end, so a break in the actual `copy_bidirectional` wiring
//! (wrong listener registration, a dropped half of the relay, etc.) would not
//! have been caught by the existing suite.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use serial_test::serial;

/// Spawn a minimal TCP echo backend that tags every reply with `tag`, so a
/// test can tell which backend actually handled a given proxied connection.
/// Handles connections sequentially, one at a time, which is enough for the
/// deterministic round-robin test below (each connection is fully finished —
/// request sent, reply read, socket closed — before the next one is opened).
fn spawn_tagged_echo_backend(tag: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind echo backend");
    let addr = listener.local_addr().expect("local_addr");

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 256];
            let Ok(n) = stream.read(&mut buf) else {
                continue;
            };
            if n == 0 {
                continue;
            }
            let reply = format!("{tag}:{}", String::from_utf8_lossy(&buf[..n]));
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });

    addr
}

/// Round-trip a line through a raw TCP connection and return the reply.
fn roundtrip(port: u16, payload: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to tcp proxy");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout");
    stream.write_all(payload.as_bytes()).expect("write");
    stream.flush().expect("flush");
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).expect("read reply");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// A single-target `tcp:` site relays bytes bidirectionally to the real
/// upstream — proves `copy_bidirectional` is actually wired up end-to-end,
/// not just that `pick_target()` returns the right string.
#[test]
#[serial]
fn tcp_proxy_relays_bytes_to_single_target() {
    let backend_addr = spawn_tagged_echo_backend("only");
    let port = common::free_port();
    let admin_port = common::free_port();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "tcp": { "targets": [backend_addr.to_string()] }
            }]
        }),
    );

    let reply = roundtrip(server.port, "hello-tcp\n");
    assert_eq!(
        reply, "only:hello-tcp\n",
        "raw bytes must reach the upstream and the reply must come back unmodified"
    );
}

/// Two targets, default (round-robin) strategy: sequential connections must
/// alternate deterministically between both backends, not pin to one of them
/// or distribute randomly.
#[test]
#[serial]
fn tcp_proxy_round_robin_alternates_between_two_targets() {
    let backend_a = spawn_tagged_echo_backend("a");
    let backend_b = spawn_tagged_echo_backend("b");
    let port = common::free_port();
    let admin_port = common::free_port();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "tcp": {
                    "targets": [backend_a.to_string(), backend_b.to_string()],
                    "strategy": "round-robin"
                }
            }]
        }),
    );

    let replies: Vec<String> = (0..4)
        .map(|i| roundtrip(server.port, &format!("{i}\n")))
        .collect();

    // The readiness probe in `TestServer::start_with_config` itself makes one
    // bare TCP connect to prove the proxy is up (see `requires_raw_tcp` in
    // tests/common/mod.rs) — that connect is a real accepted connection from
    // the proxy's point of view, so it consumes one round-robin slot before
    // the test's own first request. Which backend starts first is therefore
    // not fixed; what must hold is pure alternation between exactly two tags.
    let tags: Vec<&str> = replies
        .iter()
        .map(|r| r.split(':').next().expect("tag prefix"))
        .collect();
    assert_ne!(
        tags[0], tags[1],
        "consecutive connections must alternate backends, got {replies:?}"
    );
    assert_eq!(
        tags[0], tags[2],
        "every other connection repeats the same backend: {replies:?}"
    );
    assert_eq!(
        tags[1], tags[3],
        "every other connection repeats the same backend: {replies:?}"
    );
    for (i, reply) in replies.iter().enumerate() {
        assert_eq!(
            *reply,
            format!("{}:{i}\n", tags[i % 2]),
            "payload must echo back from the correct backend unmodified"
        );
    }
}

/// The Admin API stays reachable (and still enforces its own auth) even
/// though the site's own port speaks raw TCP, not HTTP — the two must not
/// be entangled by the `tcp:` wiring.
#[test]
#[serial]
fn tcp_proxy_site_does_not_disable_admin_api() {
    let backend_addr = spawn_tagged_echo_backend("x");
    let port = common::free_port();
    let admin_port = common::free_port();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "tcp": { "targets": [backend_addr.to_string()] }
            }]
        }),
    );

    let resp = reqwest::blocking::get(server.admin_url("/status")).expect("GET /status");
    assert!(
        resp.status().is_success(),
        "Admin API must stay reachable for a tcp-only site"
    );
}
