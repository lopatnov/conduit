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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serial_test::serial;

/// Read bytes from `stream` until a `\n` delimiter is seen or the peer
/// closes the connection, returning everything read so far (delimiter
/// included, if present). A single `read()` call can return fewer bytes
/// than a complete message — this never assumes one call is enough, unlike
/// a bare `read(&mut buf)` would.
fn read_line(stream: &mut impl Read) -> Vec<u8> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break, // EOF
            Ok(_) => {
                out.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    out
}

/// Spawn a minimal TCP echo backend that tags every reply with `tag`, so a
/// test can tell which backend actually handled a given proxied connection.
/// Handles connections sequentially, one at a time, which is enough for the
/// deterministic round-robin test below (each connection is fully finished —
/// request sent, reply read, socket closed — before the next one is opened).
///
/// Also returns a count of the connections the backend has accepted, including
/// ones that sent nothing (the proxy opens an upstream connection for every
/// downstream one, even the readiness probe's bare connect).
fn spawn_tagged_echo_backend(tag: &'static str) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind echo backend");
    let addr = listener.local_addr().expect("local_addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_in_thread = Arc::clone(&accepted);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            accepted_in_thread.fetch_add(1, Ordering::SeqCst);
            let request = read_line(&mut stream);
            if request.is_empty() {
                continue;
            }
            let reply = format!("{tag}:{}", String::from_utf8_lossy(&request));
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });

    (addr, accepted)
}

/// Round-trip a line through a raw TCP connection and return the reply.
fn roundtrip(port: u16, payload: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to tcp proxy");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout");
    stream.write_all(payload.as_bytes()).expect("write");
    stream.flush().expect("flush");
    let reply = read_line(&mut stream);
    String::from_utf8_lossy(&reply).into_owned()
}

/// A single-target `tcp:` site relays bytes bidirectionally to the real
/// upstream — proves `copy_bidirectional` is actually wired up end-to-end,
/// not just that `pick_target()` returns the right string.
#[test]
#[serial]
fn tcp_proxy_relays_bytes_to_single_target() {
    let (backend_addr, _) = spawn_tagged_echo_backend("only");
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
    let (backend_a, accepted_a) = spawn_tagged_echo_backend("a");
    let (backend_b, accepted_b) = spawn_tagged_echo_backend("b");
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

    // The readiness probe's bare connect (see below) is handled in its own
    // task, and the proxy takes the round-robin slot inside that task. If the
    // task runs late, the test's first request takes the slot before it, and
    // the replies look like `a, a, b, a` (seen on CI, run 43764377417). The
    // proxy opens the upstream connection right after taking the slot, so wait
    // until one backend has accepted it: from then on the probe's slot is
    // spent and the test's own requests get consecutive slots.
    let deadline = Instant::now() + Duration::from_secs(5);
    while accepted_a.load(Ordering::SeqCst) + accepted_b.load(Ordering::SeqCst) < 1 {
        assert!(
            Instant::now() < deadline,
            "the readiness probe's connection never reached a backend"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let replies: Vec<String> = (0..4)
        .map(|i| roundtrip(server.port, &format!("{i}\n")))
        .collect();

    // The readiness probe in `TestServer::start_with_config` itself makes one
    // bare TCP connect to prove the proxy is up (see `requires_raw_tcp`/
    // `probe_proxy`'s `raw_tcp` branch in tests/common/mod.rs, which skips
    // the HTTP/HTTPS attempts entirely for a `tcp:` site and goes straight to
    // a single bare connect) — that connect is a real accepted connection
    // from the proxy's point of view, so it consumes one round-robin slot
    // before the test's own first request. Which backend starts first is
    // therefore not fixed; what must hold is pure alternation between
    // exactly two tags.
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

/// The Admin API stays reachable, and still enforces its own token auth,
/// even though the site's own port speaks raw TCP, not HTTP — the two must
/// not be entangled by the `tcp:` wiring.
#[test]
#[serial]
fn tcp_proxy_site_does_not_disable_admin_api() {
    let (backend_addr, _) = spawn_tagged_echo_backend("x");
    let port = common::free_port();
    let admin_port = common::free_port();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": {
                "admin": {
                    "bind": format!("127.0.0.1:{admin_port}"),
                    "token": "tcp-site-admin-token"
                }
            },
            "sites": [{
                "port": port,
                "tcp": { "targets": [backend_addr.to_string()] }
            }]
        }),
    );

    let unauthenticated = common::get(server.admin_url("/status")).expect("GET /status (no token)");
    assert_eq!(
        unauthenticated.status().as_u16(),
        401,
        "Admin API must still require its token for a tcp-only site"
    );

    let authenticated = common::local_client()
        .get(server.admin_url("/status"))
        .header("authorization", "Bearer tcp-site-admin-token")
        .send()
        .expect("GET /status (with token)");
    assert!(
        authenticated.status().is_success(),
        "Admin API must stay reachable, with the right token, for a tcp-only site"
    );
}

/// A TCP site runs no HTTP filters, so an access-control key on it must be refused, not silently ignored: an
/// operator who adds `ipFilter` to a database port would believe it is protected (security review note N1).
#[test]
fn tcp_site_refuses_keys_that_would_be_ignored() {
    for (key, value) in [
        ("ipFilter", r#"{ "allow": ["10.0.0.0/8"] }"#),
        ("rateLimit", r#"{ "windowSecs": 60, "limit": 10 }"#),
        ("basicAuth", r#"{ "users": { "u": "p" } }"#),
        ("apiKey", r#"{ "keys": ["k"] }"#),
        (
            "jwtAuth",
            r#"{ "secret": "0123456789abcdef0123456789abcdef" }"#,
        ),
        (
            "forwardAuth",
            r#"{ "url": "http://auth.internal:9000/check" }"#,
        ),
    ] {
        let json =
            format!(r#"{{ "port": 5432, "tcp": {{ "targets": ["db:5432"] }}, "{key}": {value} }}"#);
        let config = conduit::config::from_str(&json).expect("parses");
        let errors = conduit::config::validate::validate(&config);
        assert!(
            errors
                .iter()
                .any(|e| e.path.ends_with(key) && e.message.contains("tcp site")),
            "{key} on a tcp site was not refused: {errors:?}"
        );
    }
    // the same site without the extra key is valid
    let config =
        conduit::config::from_str(r#"{ "port": 5432, "tcp": { "targets": ["db:5432"] } }"#)
            .expect("parses");
    assert!(conduit::config::validate::validate(&config).is_empty());
}

/// `proxyProtocol` and `tcp.sendProxyProtocol` on a real server (issue #757): the header from the (trusted) client is
/// consumed, its client address goes on to the upstream in a header of the configured version, and a connection that
/// opens without a header gets nothing.
#[test]
#[serial]
fn tcp_site_reads_the_proxy_protocol_and_sends_it_on() {
    // An upstream that answers with every byte it was sent, once it has seen the payload line.
    let upstream = TcpListener::bind("127.0.0.1:0").expect("bind upstream");
    let upstream_addr = upstream.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for stream in upstream.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.ends_with(b"hi\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
            }
            let _ = stream.write_all(&got);
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxyProtocol": { "trustedSources": ["127.0.0.0/8"], "timeoutMs": 1000 },
                "tcp": { "targets": [upstream_addr.to_string()], "sendProxyProtocol": "v1" }
            }]
        }),
    );

    let read_all = |mut s: TcpStream| {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut out = Vec::new();
        // The connection must end with EOF, or a reset because the proxy closed with our bytes unread. Our own read timing
        // out on a connection it left open, or an unrelated error, would let "closed, nothing sent" pass without proof.
        if let Err(e) = s.read_to_end(&mut out) {
            assert!(
                matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ),
                "expected EOF or a reset, got {e:?}"
            );
        }
        String::from_utf8_lossy(&out).into_owned()
    };

    // v1 in, v1 out: the upstream sees the client of the header, not 127.0.0.1, and then only the payload
    let mut c = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    c.write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\nhi\n")
        .unwrap();
    assert_eq!(
        read_all(c),
        "PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\nhi\n"
    );

    // v2 in (binary), v1 out: the version is converted
    let mut hdr = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
    hdr.extend_from_slice(&[0x21, 0x11, 0x00, 0x0c, 198, 51, 100, 7, 192, 0, 2, 1]);
    hdr.extend_from_slice(&40000u16.to_be_bytes());
    hdr.extend_from_slice(&443u16.to_be_bytes());
    let mut c = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    c.write_all(&hdr).unwrap();
    c.write_all(b"hi\n").unwrap();
    assert_eq!(
        read_all(c),
        "PROXY TCP4 198.51.100.7 192.0.2.1 40000 443\r\nhi\n"
    );

    // no header: closed, and nothing reaches the upstream
    let mut c = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    c.write_all(b"hi\n").unwrap();
    assert_eq!(
        read_all(c),
        "",
        "a connection without a header gets nothing"
    );
}
