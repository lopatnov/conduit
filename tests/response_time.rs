mod common;

use serial_test::serial;

fn srv_with_response_time(enabled: serde_json::Value) -> common::TestServer {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<html/>").expect("write");
    let static_dir = dir.path().to_string_lossy().into_owned();
    Box::leak(Box::new(dir));

    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "responseTime": enabled,
                "static": static_dir
            }]
        }),
    )
}

// ── responseTime: true ────────────────────────────────────────────────────

#[test]
#[serial]
fn response_time_header_present_when_enabled() {
    let srv = srv_with_response_time(serde_json::json!(true));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("x-response-time").is_some(),
        "X-Response-Time header must be present when responseTime: true"
    );
}

#[test]
#[serial]
fn response_time_value_is_numeric_ms() {
    let srv = srv_with_response_time(serde_json::json!(true));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);

    let val = resp
        .headers()
        .get("x-response-time")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Value is e.g. "1.23ms" or "0.456ms"
    assert!(
        val.ends_with("ms"),
        "X-Response-Time must end with 'ms', got: '{val}'"
    );
    let numeric: &str = val.trim_end_matches("ms");
    assert!(
        numeric.parse::<f64>().is_ok(),
        "X-Response-Time must contain a numeric value, got: '{val}'"
    );
    let ms: f64 = numeric.parse().unwrap();
    assert!(
        ms >= 0.0 && ms < 60_000.0,
        "X-Response-Time value unreasonable: {ms} ms"
    );
}

#[test]
#[serial]
fn response_time_on_health_endpoint() {
    // X-Response-Time should also appear on the built-in health endpoint.
    let srv = srv_with_response_time(serde_json::json!(true));
    let resp = reqwest::blocking::get(srv.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("x-response-time").is_some(),
        "X-Response-Time must be present on /__health__ too"
    );
}

// ── responseTime: false ───────────────────────────────────────────────────

#[test]
#[serial]
fn response_time_header_absent_when_disabled() {
    let srv = srv_with_response_time(serde_json::json!(false));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("x-response-time").is_none(),
        "X-Response-Time must be absent when responseTime: false"
    );
}

// ── object form with custom digits ────────────────────────────────────────

#[test]
#[serial]
fn response_time_object_form_digits() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<html/>").expect("write");
    let static_dir = dir.path().to_string_lossy().into_owned();
    Box::leak(Box::new(dir));

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "responseTime": { "digits": 0 },
                "static": static_dir
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);

    let val = resp
        .headers()
        .get("x-response-time")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // digits: 0 → integer ms, e.g. "2ms"
    assert!(
        val.ends_with("ms"),
        "X-Response-Time should end with ms, got: '{val}'"
    );
    let numeric = val.trim_end_matches("ms");
    assert!(
        !numeric.contains('.'),
        "digits: 0 should produce integer ms (no decimal), got: '{val}'"
    );
}
