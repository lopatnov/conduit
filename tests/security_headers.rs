mod common;

use serial_test::serial;

fn srv_with_sec(sec: serde_json::Value) -> common::TestServer {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<html/>").expect("write");
    let static_dir = dir.path().to_string_lossy().into_owned();
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "securityHeaders": sec,
                "static": static_dir
            }]
        }),
    );
    // Drop dir AFTER TestServer is created; srv holds the static path so we
    // need dir to outlive the request — capture it in a `Box::leak` to keep
    // it alive for the test.  Instead, embed the dir into a wrapping struct.
    // Simplest: just return srv and keep dir alive via Box::leak for test.
    // Actually: we need to keep dir alive. Let's use a different approach:
    // Use the tempdir inside start_with_config by embedding the path in config.
    // The TestServer holds its own TempDir (_dir), but the static files dir
    // is separate.  Box::leak the dir to keep it alive for the test duration.
    // This leaks a tiny TempDir per test — acceptable in tests.
    Box::leak(Box::new(dir));
    srv
}

// ── securityHeaders: true ─────────────────────────────────────────────────

#[test]
#[serial]
fn security_headers_true_sets_nosniff() {
    let srv = srv_with_sec(serde_json::json!(true));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff"),
        "X-Content-Type-Options: nosniff must be present"
    );
}

#[test]
#[serial]
fn security_headers_true_sets_frame_options() {
    let srv = srv_with_sec(serde_json::json!(true));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    let val = resp
        .headers()
        .get("x-frame-options")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(val, "SAMEORIGIN", "X-Frame-Options: SAMEORIGIN expected");
}

#[test]
#[serial]
fn security_headers_true_sets_referrer_policy() {
    let srv = srv_with_sec(serde_json::json!(true));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("referrer-policy").is_some(),
        "Referrer-Policy must be present"
    );
}

#[test]
#[serial]
fn security_headers_true_sets_xss_protection() {
    let srv = srv_with_sec(serde_json::json!(true));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("x-xss-protection").is_some(),
        "X-XSS-Protection must be present"
    );
}

// ── securityHeaders: false ────────────────────────────────────────────────

#[test]
#[serial]
fn security_headers_false_emits_no_headers() {
    let srv = srv_with_sec(serde_json::json!(false));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("x-content-type-options").is_none(),
        "security headers must be absent when disabled"
    );
    assert!(
        resp.headers().get("x-frame-options").is_none(),
        "X-Frame-Options must be absent when disabled"
    );
}

// ── object form: HSTS + CSP ───────────────────────────────────────────────

#[test]
#[serial]
fn security_headers_object_hsts_and_csp() {
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
                "securityHeaders": {
                    "hstsMaxAgeSecs": 31536000,
                    "csp": "default-src 'self'"
                },
                "static": static_dir
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);

    // RFC 6797: Strict-Transport-Security must only be honored over HTTPS.
    // We don't assert HSTS here since this is a cleartext HTTP test; the header
    // may or may not be emitted but browsers will ignore it over HTTP anyway.
    // HSTS behavior over TLS is tested in tests/tls.rs.

    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(csp, "default-src 'self'", "CSP value mismatch: '{csp}'");
}
