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
    let resp = common::get(srv.url("/index.html")).expect("GET");
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
    let resp = common::get(srv.url("/index.html")).expect("GET");
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
    let resp = common::get(srv.url("/index.html")).expect("GET");
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
    let resp = common::get(srv.url("/index.html")).expect("GET");
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
    let resp = common::get(srv.url("/index.html")).expect("GET");
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

    let resp = common::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);

    // RFC 6797 section 7.2: no Strict-Transport-Security over plain HTTP (#723); the over-TLS and
    // X-Forwarded-Proto cases are below and in tests/tls.rs.
    assert!(
        resp.headers().get("strict-transport-security").is_none(),
        "HSTS must not be sent over plain HTTP"
    );

    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(csp, "default-src 'self'", "CSP value mismatch: '{csp}'");
}

// ── HSTS and the transport (RFC 6797 section 7.2, #723) ───────────────────

fn hsts_of(sec: serde_json::Value, forwarded_proto: Option<&str>) -> Option<String> {
    let srv = srv_with_sec(sec);
    let mut req = common::local_client().get(srv.url("/index.html"));
    if let Some(p) = forwarded_proto {
        req = req.header("x-forwarded-proto", p);
    }
    let resp = req.send().expect("GET");
    assert_eq!(resp.status(), 200);
    resp.headers()
        .get("strict-transport-security")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

#[test]
#[serial]
fn hsts_is_sent_behind_a_tls_terminating_balancer_that_says_so() {
    let sec = serde_json::json!({ "hstsMaxAgeSecs": 63072000 });
    assert_eq!(hsts_of(sec.clone(), None), None, "plain HTTP: no HSTS");
    assert_eq!(
        hsts_of(sec.clone(), Some("http")),
        None,
        "X-Forwarded-Proto: http: no HSTS"
    );
    assert_eq!(
        hsts_of(sec.clone(), Some("HTTPS")).as_deref(),
        Some("max-age=63072000; includeSubDomains"),
        "X-Forwarded-Proto: https (any case): HSTS"
    );
}

#[test]
#[serial]
fn hsts_over_http_opts_in_for_a_balancer_that_sends_no_forwarded_proto() {
    let sec = serde_json::json!({ "hstsMaxAgeSecs": 300, "hstsOverHttp": true });
    assert_eq!(
        hsts_of(sec, None).as_deref(),
        Some("max-age=300; includeSubDomains")
    );
}
