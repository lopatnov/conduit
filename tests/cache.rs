mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serial_test::serial;

// ── Mock upstream ─────────────────────────────────────────────────────────────

struct MockUpstream {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl MockUpstream {
    /// Start a mock upstream. When `delay_ms > 0`, the upstream pauses that
    /// long before sending its response — this lets concurrent requests pile
    /// up so that the cache lock can be exercised.
    fn start_with_delay(delay_ms: u64) -> Self {
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
                    if delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    }
                    let body = b"hello from upstream";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(body);
                });
            }
        });

        MockUpstream { port, hits }
    }

    fn start() -> Self {
        Self::start_with_delay(0)
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// First request fetches from upstream; second identical request is served
/// from the in-memory cache (upstream hit count must not increase).
#[test]
#[serial]
fn cache_second_request_served_from_memory() {
    let upstream = MockUpstream::start();

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
                    "/cached": {
                        "targets": [upstream.url()],
                        "strategy": "round-robin",
                        "cache": {
                            "store": "memory",
                            "ttlSecs": 30
                        }
                    }
                }
            }]
        }),
    );

    // First request — cache miss; upstream is contacted.
    let r1 = reqwest::blocking::get(srv.url("/cached/item")).expect("GET 1");
    assert_eq!(r1.status(), 200, "first request must return 200");
    assert_eq!(upstream.hit_count(), 1, "first request must reach upstream");

    // Second identical request — must be a cache hit; upstream not contacted again.
    let r2 = reqwest::blocking::get(srv.url("/cached/item")).expect("GET 2");
    assert_eq!(r2.status(), 200, "second request must return 200");
    assert_eq!(
        upstream.hit_count(),
        1,
        "second request must be served from cache, not from upstream"
    );
}

/// With `skipIfCookie: true`, a request carrying a Cookie header bypasses the
/// cache and contacts the upstream directly.
#[test]
#[serial]
fn cache_skip_if_cookie_bypasses_cache() {
    let upstream = MockUpstream::start();

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
                    "/skip-cookie": {
                        "targets": [upstream.url()],
                        "strategy": "round-robin",
                        "cache": {
                            "store": "memory",
                            "ttlSecs": 30,
                            "skipIfCookie": true
                        }
                    }
                }
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();

    // First request — no cookie; goes to upstream and is cached.
    let r1 = client
        .get(srv.url("/skip-cookie/page"))
        .send()
        .expect("GET no-cookie");
    assert_eq!(r1.status(), 200);
    assert_eq!(upstream.hit_count(), 1, "first request must hit upstream");

    // Second request — with cookie; must bypass cache and hit upstream again.
    let r2 = client
        .get(srv.url("/skip-cookie/page"))
        .header("cookie", "session=abc123")
        .send()
        .expect("GET with-cookie");
    assert_eq!(r2.status(), 200);
    assert_eq!(
        upstream.hit_count(),
        2,
        "request with cookie must bypass cache and hit upstream"
    );
}

/// Paths matching a `skipPaths` pattern bypass the cache entirely.
#[test]
#[serial]
fn cache_skip_paths_bypasses_cache() {
    let upstream = MockUpstream::start();

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
                    "/data": {
                        "targets": [upstream.url()],
                        "strategy": "round-robin",
                        "cache": {
                            "store": "memory",
                            "ttlSecs": 30,
                            "skipPaths": ["/data/private/**"]
                        }
                    }
                }
            }]
        }),
    );

    // Request to a private path — must NOT be cached.
    let _ = reqwest::blocking::get(srv.url("/data/private/secret")).expect("GET private 1");
    assert_eq!(
        upstream.hit_count(),
        1,
        "first private request must hit upstream"
    );

    let _ = reqwest::blocking::get(srv.url("/data/private/secret")).expect("GET private 2");
    assert_eq!(
        upstream.hit_count(),
        2,
        "second private request must also hit upstream (not cached)"
    );

    // Request to a cacheable path — must be cached after the first hit.
    let _ = reqwest::blocking::get(srv.url("/data/public/item")).expect("GET public 1");
    assert_eq!(
        upstream.hit_count(),
        3,
        "first public request must hit upstream"
    );

    let _ = reqwest::blocking::get(srv.url("/data/public/item")).expect("GET public 2");
    assert_eq!(
        upstream.hit_count(),
        3,
        "second public request must be served from cache"
    );
}

/// When `ttlSecs` is not set (defaults to 0), no caching occurs and every
/// request reaches the upstream.
#[test]
#[serial]
fn cache_zero_ttl_disables_caching() {
    let upstream = MockUpstream::start();

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
                    "/no-cache": {
                        "targets": [upstream.url()],
                        "strategy": "round-robin",
                        "cache": {
                            "store": "memory"
                            // ttlSecs intentionally omitted (defaults to 0)
                        }
                    }
                }
            }]
        }),
    );

    for i in 1..=3_u16 {
        let r = reqwest::blocking::get(srv.url("/no-cache/item")).expect("GET");
        assert_eq!(r.status(), 200);
        assert_eq!(
            upstream.hit_count(),
            i as usize,
            "with ttl=0 every request must reach the upstream (i={i})"
        );
    }
}

/// Cache lock (thundering herd prevention): when N concurrent requests arrive
/// for the same uncached URL, only ONE should reach the upstream — the rest
/// wait for the cache lock and are served from the stored response.
///
/// The mock upstream introduces a 200 ms delay so that all N threads are
/// genuinely concurrent when the first one starts fetching.
#[test]
#[serial]
fn cache_lock_prevents_thundering_herd() {
    // Slow upstream: 200 ms delay ensures threads are concurrent during fetch.
    let upstream = MockUpstream::start_with_delay(200);

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
                    "/locked": {
                        "targets": [upstream.url()],
                        "strategy": "round-robin",
                        "cache": {
                            "store": "memory",
                            "ttlSecs": 30
                        }
                    }
                }
            }]
        }),
    );

    let url = srv.url("/locked/item");
    let concurrency = 5;

    // Fire N threads all hitting the same URL simultaneously.
    let handles: Vec<_> = (0..concurrency)
        .map(|_| {
            let url = url.clone();
            std::thread::spawn(move || reqwest::blocking::get(&url).expect("GET").status().as_u16())
        })
        .collect();

    let statuses: Vec<u16> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All responses must be 200.
    for (i, &status) in statuses.iter().enumerate() {
        assert_eq!(status, 200, "thread {i} must receive 200");
    }

    // With the cache lock active, the upstream must be contacted exactly once —
    // all other threads waited on the read lock and served the cached copy.
    assert_eq!(
        upstream.hit_count(),
        1,
        "cache lock must prevent thundering herd: expected 1 upstream hit, got {}",
        upstream.hit_count()
    );
}
