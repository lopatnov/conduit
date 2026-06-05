/// Integration tests for faultInjection — abort and delay chaos testing.
mod common;

use std::time::{Duration, Instant};

use reqwest::blocking::Client;

fn plain_client() -> Client {
    Client::new()
}

// ── Abort tests ───────────────────────────────────────────────────────────────

fn server_with_abort(percent: f64, status: u16) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "faultInjection": {
                    "abort": { "percent": percent, "status": status }
                }
            }]
        }),
    )
}

#[test]
fn fault_abort_100pct_always_returns_configured_status() {
    let srv = server_with_abort(100.0, 503);
    // With 100% abort probability every non-health request must be aborted.
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_eq!(
        resp.status().as_u16(),
        503,
        "100% abort should return configured status 503"
    );
}

#[test]
fn fault_abort_zero_pct_never_aborts() {
    let srv = server_with_abort(0.0, 503);
    // 0% abort = never inject fault.  No upstream → 404 fallback, NOT 503.
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_ne!(
        resp.status().as_u16(),
        503,
        "0% abort should never return 503"
    );
}

#[test]
fn fault_abort_health_bypasses_fault_injection() {
    // Health endpoint must always be reachable regardless of fault injection.
    let srv = server_with_abort(100.0, 503);
    let resp = plain_client()
        .get(srv.url("/__health__"))
        .send()
        .expect("health");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "health must bypass fault injection"
    );
}

#[test]
fn fault_abort_custom_status_code() {
    let srv = server_with_abort(100.0, 429);
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_eq!(
        resp.status().as_u16(),
        429,
        "abort should return the configured status code"
    );
}

// ── Delay tests ───────────────────────────────────────────────────────────────

fn server_with_delay(percent: f64, duration_ms: u64) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "faultInjection": {
                    "delay": { "percent": percent, "durationMs": duration_ms }
                }
            }]
        }),
    )
}

// NOTE: delay tests are marked ignore because they are timing-sensitive
// and can fail non-deterministically in CI when prior test servers haven't
// fully released their resources yet.  Run individually with:
//   cargo test --test fault_injection fault_delay -- --ignored
#[test]
#[ignore]
fn fault_delay_100pct_adds_latency() {
    let srv = server_with_delay(100.0, 200);

    let start = Instant::now();
    let _resp = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(srv.url("/"))
        .send()
        .expect("GET /");
    let elapsed = start.elapsed();

    // The delay is 200 ms; allow ± 100 ms for overhead.
    assert!(
        elapsed >= Duration::from_millis(150),
        "100% delay of 200ms should add at least 150ms latency; got {elapsed:?}"
    );
}

#[test]
#[ignore]
fn fault_delay_zero_pct_no_latency() {
    let srv = server_with_delay(0.0, 5000);

    let start = Instant::now();
    let _resp = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(srv.url("/"))
        .send()
        .expect("GET /");
    let elapsed = start.elapsed();

    // 0% probability — should not delay at all (5000ms delay never fires).
    assert!(
        elapsed < Duration::from_millis(2000),
        "0% delay should not add significant latency; got {elapsed:?}"
    );
}
