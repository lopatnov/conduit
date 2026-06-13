mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

// ── Flaky mock upstream (for stale-if-error tests) ──────────────────────────────

/// Body returned by a healthy `FlakyUpstream` — asserted on after the upstream
/// starts failing to prove the *cached* copy (not a fresh fetch) was served.
const FRESH_BODY: &str = "fresh-cached-body";

/// A mock upstream that serves a cacheable `200` until `fail` is flipped, after
/// which it either returns `500` or drops the connection without responding.
/// Used to exercise the stale-if-error path (#48): warm the cache, let the
/// entry go stale, induce an upstream failure, and assert the stale copy is
/// served instead of the failure.
struct FlakyUpstream {
    port: u16,
    hits: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    /// When `fail` is set: `true` → drop the connection (upstream connection
    /// error), `false` → respond with HTTP 500.
    drop_conn: Arc<AtomicBool>,
}

impl FlakyUpstream {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind flaky upstream");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        let drop_conn = Arc::new(AtomicBool::new(false));
        let (h, f, d) = (hits.clone(), fail.clone(), drop_conn.clone());

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                // Skip transient accept errors rather than tearing down the
                // mock server (which would flake the test).
                let Ok(mut stream) = stream else { continue };
                let (h, f, d) = (h.clone(), f.clone(), d.clone());
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let _ = stream.read(&mut buf);
                    h.fetch_add(1, Ordering::SeqCst);
                    if f.load(Ordering::SeqCst) {
                        if d.load(Ordering::SeqCst) {
                            // Drop the stream without responding — conduit sees
                            // an upstream connection failure.
                            return;
                        }
                        let body = b"upstream is down";
                        let resp = format!(
                            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(body);
                        return;
                    }
                    // Healthy: cacheable 200 (no Set-Cookie / no-store).
                    let body = FRESH_BODY.as_bytes();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(body);
                });
            }
        });

        FlakyUpstream {
            port,
            hits,
            fail,
            drop_conn,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Subsequent requests return HTTP 500.
    fn start_failing_with_500(&self) {
        self.drop_conn.store(false, Ordering::SeqCst);
        self.fail.store(true, Ordering::SeqCst);
    }

    /// Subsequent requests drop the connection without responding.
    fn start_failing_by_dropping(&self) {
        self.drop_conn.store(true, Ordering::SeqCst);
        self.fail.store(true, Ordering::SeqCst);
    }
}

/// Build a single-site config proxying `path_prefix` to `upstream_url` with a
/// 1-second cache TTL and a 300-second stale-if-error window.  `retry` is
/// appended verbatim into the route when `Some`.
fn stale_if_error_config(
    port: u16,
    admin_port: u16,
    path_prefix: &str,
    upstream_url: &str,
    retry: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut route = serde_json::json!({
        "targets": [upstream_url],
        "strategy": "round-robin",
        "cache": {
            "store": "memory",
            "ttlSecs": 1,
            "staleIfErrorSecs": 300
        }
    });
    if let Some(retry) = retry {
        route["retry"] = retry;
    }
    serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            // `(path_prefix)` interpolates the variable's value as the key — a
            // bare `path_prefix` would use the literal string "path_prefix".
            "proxy": { (path_prefix): route }
        }]
    })
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

// ── stale-if-error (#48) ────────────────────────────────────────────────────────
//
// These tests warm the cache, let the entry go stale (ttlSecs = 1, sleep 1.5 s),
// then induce an upstream failure and assert the *stale* cached copy is served
// instead of the failure.  The `hit_count >= 2` assertion is load-bearing: it
// proves the second request actually went stale → revalidated → failed → served
// stale, rather than being a still-fresh cache hit (which would never contact
// the upstream).

/// Helper: warm the cache for `path`, returning once the entry is stored.
fn warm_cache(srv: &common::TestServer, upstream: &FlakyUpstream, path: &str) {
    let r1 = reqwest::blocking::get(srv.url(path)).expect("warm GET");
    assert_eq!(r1.status(), 200, "warm-up request must return 200");
    assert_eq!(
        r1.text().unwrap(),
        FRESH_BODY,
        "warm-up body must be cached body"
    );
    assert_eq!(
        upstream.hit_count(),
        1,
        "warm-up must reach the upstream once"
    );
    // Let the cached entry go stale (ttlSecs = 1).
    std::thread::sleep(std::time::Duration::from_millis(1500));
}

/// stale-if-error on a 5xx with NO retry configured: the stale entry must be
/// served instead of the upstream 500.
#[test]
#[serial]
fn stale_if_error_serves_stale_on_5xx_without_retry() {
    let upstream = FlakyUpstream::start();
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        stale_if_error_config(port, admin_port, "/sie-5xx", &upstream.url(), None),
    );

    warm_cache(&srv, &upstream, "/sie-5xx/item");
    upstream.start_failing_with_500();

    let r2 = reqwest::blocking::get(srv.url("/sie-5xx/item")).expect("GET 2");
    let status = r2.status();
    let body = r2.text().unwrap();
    assert_eq!(
        status, 200,
        "stale-if-error must serve the cached 200, not the upstream 500"
    );
    assert_eq!(
        body, FRESH_BODY,
        "served body must be the stale cached copy"
    );
    assert_eq!(
        upstream.hit_count(),
        2,
        "exactly one warm-up hit + one revalidation hit (no retry configured)"
    );
}

/// stale-if-error when the retry budget is exhausted (issue #48): retry is
/// configured for 5xx, every attempt fails, and once retries are spent the
/// stale cached copy must be served instead of surfacing the 500.
#[test]
#[serial]
fn stale_if_error_serves_stale_when_retry_exhausted_on_5xx() {
    let upstream = FlakyUpstream::start();
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        stale_if_error_config(
            port,
            admin_port,
            "/sie-retry",
            &upstream.url(),
            Some(serde_json::json!({ "attempts": 2, "conditions": ["5xx"] })),
        ),
    );

    warm_cache(&srv, &upstream, "/sie-retry/item");
    upstream.start_failing_with_500();

    let r2 = reqwest::blocking::get(srv.url("/sie-retry/item")).expect("GET 2");
    let status = r2.status();
    let body = r2.text().unwrap();
    assert_eq!(
        status, 200,
        "after retries are exhausted, stale-if-error must serve the cached 200"
    );
    assert_eq!(
        body, FRESH_BODY,
        "served body must be the stale cached copy"
    );
    // Loose bound on purpose: the exact upstream-hit count here depends on
    // retry-attempt internals (warm-up + initial revalidation + N retries),
    // which this test does not pin — retry counting is covered elsewhere. All
    // this test asserts is that revalidation reached the failing upstream and
    // the stale copy was still served.
    assert!(
        upstream.hit_count() >= 2,
        "the retry attempts must have reached the failing upstream"
    );
}

/// stale-if-error on an upstream *connection* failure (not a 5xx response):
/// the upstream drops the connection during revalidation, and the stale cached
/// copy must still be served.
#[test]
#[serial]
fn stale_if_error_serves_stale_on_connection_error() {
    let upstream = FlakyUpstream::start();
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        stale_if_error_config(port, admin_port, "/sie-conn", &upstream.url(), None),
    );

    warm_cache(&srv, &upstream, "/sie-conn/item");
    upstream.start_failing_by_dropping();

    let r2 = reqwest::blocking::get(srv.url("/sie-conn/item")).expect("GET 2");
    let status = r2.status();
    let body = r2.text().unwrap();
    assert_eq!(
        status, 200,
        "stale-if-error must serve the cached 200 on an upstream connection error"
    );
    assert_eq!(
        body, FRESH_BODY,
        "served body must be the stale cached copy"
    );
    assert_eq!(
        upstream.hit_count(),
        2,
        "exactly one warm-up hit + one revalidation hit (no retry configured)"
    );
}
