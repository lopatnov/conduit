//! JWKS cache refresh behaviour (issue #163): single-flight fetches, stale-while-revalidate, stale fallback on a
//! failing IdP, and the `MAX_STALE` cut-off.
//!
//! Every test gets its own mock server, hence its own URL and its own cache entry — they do not share state.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::jwks_cache::test_support::{age_cache, clear_backoff, is_refreshing};
use super::super::jwks_cache::{get_jwks_keys, request_refresh, KeyMap, MAX_STALE, RETRY_BACKOFF};

const REFRESH_SECS: u64 = 3600;

/// A JWKS endpoint that counts the connections it receives and can be told to fail or to answer slowly.
struct MockIdp {
    url: String,
    hits: Arc<AtomicUsize>,
    down: Arc<AtomicBool>,
    delay_ms: Arc<AtomicU64>,
}

impl MockIdp {
    fn start() -> Self {
        let body = r#"{"keys":[{"kty":"RSA","kid":"k1","n":"AQAB","e":"AQAB"}]}"#;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/jwks", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let down = Arc::new(AtomicBool::new(false));
        let delay_ms = Arc::new(AtomicU64::new(0));
        let (h, d, dl) = (hits.clone(), down.clone(), delay_ms.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                h.fetch_add(1, Ordering::SeqCst);
                let (d, dl) = (d.clone(), dl.clone());
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    if d.load(Ordering::SeqCst) {
                        return; // drop the connection without answering
                    }
                    std::thread::sleep(Duration::from_millis(dl.load(Ordering::SeqCst)));
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes());
                });
            }
        });
        Self {
            url,
            hits,
            down,
            delay_ms,
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Fetch once so the cache is warm, then wait for the bookkeeping to settle.
    fn prime(&self) -> Arc<KeyMap> {
        let keys = get_jwks_keys(&self.url, REFRESH_SECS).expect("first fetch succeeds");
        assert_eq!(self.hits(), 1);
        keys
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn concurrent_cold_requests_share_one_fetch() {
    let idp = MockIdp::start();
    idp.delay_ms.store(300, Ordering::SeqCst);

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let url = idp.url.clone();
            std::thread::spawn(move || get_jwks_keys(&url, REFRESH_SECS))
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap().is_some(), "every waiter gets the keys");
    }
    assert_eq!(idp.hits(), 1, "one fetch must serve all concurrent waiters");
}

#[test]
fn a_stale_cache_answers_at_once_and_refreshes_in_the_background() {
    let idp = MockIdp::start();
    idp.prime();
    age_cache(&idp.url, Duration::from_secs(REFRESH_SECS + 60));
    idp.delay_ms.store(500, Ordering::SeqCst);

    let started = Instant::now();
    let keys = get_jwks_keys(&idp.url, REFRESH_SECS).expect("stale keys still serve");
    assert!(keys.contains_key("k1"));
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "a stale hit must not wait for the slow IdP (took {:?})",
        started.elapsed()
    );

    // Concurrent stale hits start no second refresh.
    for _ in 0..5 {
        assert!(get_jwks_keys(&idp.url, REFRESH_SECS).is_some());
    }
    wait_until("the background refresh to finish", || {
        idp.hits() == 2 && !is_refreshing(&idp.url)
    });
    assert_eq!(idp.hits(), 2, "exactly one background refresh");

    // The refreshed keys are fresh again: no further fetch.
    assert!(get_jwks_keys(&idp.url, REFRESH_SECS).is_some());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(idp.hits(), 2);
}

#[test]
fn an_early_refresh_request_is_single_flight() {
    let idp = MockIdp::start();
    idp.prime();

    // Keys fetched a moment ago: unknown-`kid` tokens (possibly forged) must not hit the IdP again.
    for _ in 0..8 {
        request_refresh(&idp.url);
    }
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(idp.hits(), 1, "no refresh inside the cooldown");

    // Once the keys are older than the cooldown, many unknown-`kid` tokens at once start exactly one refresh.
    age_cache(&idp.url, RETRY_BACKOFF + Duration::from_secs(1));
    idp.delay_ms.store(300, Ordering::SeqCst);
    for _ in 0..8 {
        request_refresh(&idp.url);
    }
    wait_until("the requested refresh to finish", || {
        idp.hits() == 2 && !is_refreshing(&idp.url)
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(idp.hits(), 2);
}

#[test]
fn a_failing_refresh_keeps_serving_the_last_good_keys_and_backs_off() {
    let idp = MockIdp::start();
    idp.prime();
    age_cache(&idp.url, Duration::from_secs(REFRESH_SECS + 60));
    idp.down.store(true, Ordering::SeqCst);

    assert!(
        get_jwks_keys(&idp.url, REFRESH_SECS).is_some(),
        "an outage must not turn into 401s while the old keys are within MAX_STALE"
    );
    wait_until("the failed refresh attempt", || {
        idp.hits() == 2 && !is_refreshing(&idp.url)
    });

    // Backoff: further requests keep serving and do not hit the failing IdP again.
    for _ in 0..5 {
        assert!(get_jwks_keys(&idp.url, REFRESH_SECS).is_some());
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(idp.hits(), 2, "a failed fetch must not be retried at once");

    // Once the backoff has elapsed the next stale request tries again (and recovers when the IdP is back).
    clear_backoff(&idp.url);
    idp.down.store(false, Ordering::SeqCst);
    assert!(get_jwks_keys(&idp.url, REFRESH_SECS).is_some());
    wait_until("the recovery refresh", || {
        idp.hits() == 3 && !is_refreshing(&idp.url)
    });
}

#[test]
fn keys_older_than_max_stale_fail_closed_until_a_fetch_succeeds() {
    let idp = MockIdp::start();
    idp.prime();
    age_cache(
        &idp.url,
        Duration::from_secs(REFRESH_SECS) + MAX_STALE + Duration::from_secs(1),
    );
    idp.down.store(true, Ordering::SeqCst);

    assert!(
        get_jwks_keys(&idp.url, REFRESH_SECS).is_none(),
        "keys past MAX_STALE must no longer verify tokens when the refresh fails"
    );

    idp.down.store(false, Ordering::SeqCst);
    clear_backoff(&idp.url);
    assert!(
        get_jwks_keys(&idp.url, REFRESH_SECS).is_some(),
        "the next successful fetch restores service"
    );
}

#[test]
fn a_cold_cache_with_an_unreachable_idp_fails_closed_then_backs_off() {
    let idp = MockIdp::start();
    idp.down.store(true, Ordering::SeqCst);

    assert!(get_jwks_keys(&idp.url, REFRESH_SECS).is_none());
    let after_first = idp.hits();
    assert_eq!(after_first, 1);
    // Within the backoff window a request fails closed without touching the IdP again.
    assert!(get_jwks_keys(&idp.url, REFRESH_SECS).is_none());
    assert_eq!(idp.hits(), after_first);
}
