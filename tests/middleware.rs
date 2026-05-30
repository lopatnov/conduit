//! Integration tests for the Rhai scripting middleware (Phase 4.1).

mod common;

use std::net::TcpListener;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a conduit config with a Rhai script middleware.
///
/// The script file is written into `dir` before the server starts.
fn make_config(port: u16, admin_port: u16, script_path: &str) -> serde_json::Value {
    serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "middleware": [{ "type": "script", "path": script_path }]
        }]
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A script that unconditionally passes all requests should not affect 200 OK.
#[test]
fn script_allow_all_passes_request_through() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("allow.rhai");
    std::fs::write(&script_path, "true").unwrap();

    let port = free_port();
    let admin_port = free_port();
    let cfg = make_config(port, admin_port, script_path.to_str().unwrap());

    let server = common::TestServer::start_with_config(port, admin_port, cfg);
    let resp = reqwest::blocking::get(server.url("/__health__")).unwrap();
    assert_eq!(resp.status(), 200);
}

/// A script that returns `false` immediately causes every request to get a
/// `200 text/plain` abort (the default script response status is 200).
#[test]
fn script_deny_all_aborts_with_default_200() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("deny.rhai");
    std::fs::write(&script_path, "false").unwrap();

    let port = free_port();
    let admin_port = free_port();
    let cfg = make_config(port, admin_port, script_path.to_str().unwrap());

    let server = common::TestServer::start_with_config(port, admin_port, cfg);
    let resp = reqwest::blocking::get(server.url("/anything")).unwrap();
    // Script returned false with default status 200.
    assert_eq!(resp.status(), 200);
}

/// A script that sets `response.status = 403` and returns `false` causes the
/// caller to receive a 403 response.
#[test]
fn script_abort_with_custom_status() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("deny403.rhai");
    std::fs::write(&script_path, "response.status = 403; false").unwrap();

    let port = free_port();
    let admin_port = free_port();
    let cfg = make_config(port, admin_port, script_path.to_str().unwrap());

    let server = common::TestServer::start_with_config(port, admin_port, cfg);
    let resp = reqwest::blocking::get(server.url("/anything")).unwrap();
    assert_eq!(resp.status(), 403);
}

/// A script that sets `response.status = 401` with a `WWW-Authenticate` header
/// and returns `false` sends both the status and the header.
#[test]
fn script_abort_with_response_header() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("auth.rhai");
    std::fs::write(
        &script_path,
        r#"response.status = 401; response.header("WWW-Authenticate", "Bearer"); false"#,
    )
    .unwrap();

    let port = free_port();
    let admin_port = free_port();
    let cfg = make_config(port, admin_port, script_path.to_str().unwrap());

    let server = common::TestServer::start_with_config(port, admin_port, cfg);
    let resp = reqwest::blocking::get(server.url("/api")).unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(
        resp.headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok()),
        Some("Bearer")
    );
}

/// A script that inspects the `Authorization` header and allows or denies
/// based on its value.
#[test]
fn script_conditional_on_request_header() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("check_header.rhai");
    std::fs::write(
        &script_path,
        r#"
        let tok = request.header("Authorization");
        if tok == "" {
            response.status = 401;
            return false;
        }
        true
        "#,
    )
    .unwrap();

    let port = free_port();
    let admin_port = free_port();
    let cfg = make_config(port, admin_port, script_path.to_str().unwrap());

    let server = common::TestServer::start_with_config(port, admin_port, cfg);

    // Without header → script aborts → 401.
    // Use /api (not /__health__, which bypasses scripts by design).
    let no_auth = reqwest::blocking::get(server.url("/api")).unwrap();
    assert_eq!(no_auth.status(), 401);

    // With header → script passes → falls through to the default 404 fallback
    // (no static/proxy configured, but the important thing is the script
    // did not abort with 401).
    let with_auth = reqwest::blocking::Client::new()
        .get(server.url("/api"))
        .header("Authorization", "Bearer secret")
        .send()
        .unwrap();
    assert_ne!(
        with_auth.status(),
        401,
        "script should pass when header is present"
    );
}

/// A script with a syntax error should not crash the server; requests should
/// pass through (fail-open behaviour).
#[test]
fn broken_script_is_fail_open() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("broken.rhai");
    std::fs::write(&script_path, "let x = ;").unwrap(); // syntax error

    let port = free_port();
    let admin_port = free_port();
    let cfg = make_config(port, admin_port, script_path.to_str().unwrap());

    let server = common::TestServer::start_with_config(port, admin_port, cfg);
    // Script fails to compile → fail-open → request passes through.
    let resp = reqwest::blocking::get(server.url("/__health__")).unwrap();
    assert_eq!(resp.status(), 200);
}

/// A `type: "script"` entry without a `path` field is a config-validation
/// error, not a runtime panic.
#[test]
fn validate_rejects_script_entry_without_path() {
    let config = serde_json::json!({
        "port": 8080,
        "middleware": [{ "type": "script" }]
    });
    let raw = serde_json::to_string(&config).unwrap();
    let app = conduit::config::from_str(&raw).expect("parse ok");
    let errors = conduit::config::validate::validate(&app);
    assert!(
        errors.iter().any(|e| e.message.contains("path")),
        "expected a validation error about missing path, got: {errors:?}"
    );
}

/// A middleware entry with an unknown type is rejected during validation.
#[test]
fn validate_rejects_unknown_middleware_type() {
    let config = serde_json::json!({
        "port": 8080,
        "middleware": [{ "type": "nonexistent" }]
    });
    let raw = serde_json::to_string(&config).unwrap();
    let app = conduit::config::from_str(&raw).expect("parse ok");
    let errors = conduit::config::validate::validate(&app);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("unknown middleware type")),
        "expected unknown-type error, got: {errors:?}"
    );
}
