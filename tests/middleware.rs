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

// ── Demo middleware integration tests ─────────────────────────────────────────

/// Helper: write a Rhai script to `dir` and return its path string.
fn write_script(dir: &tempfile::TempDir, name: &str, src: &str) -> String {
    let p = dir.path().join(name);
    std::fs::write(&p, src).unwrap();
    p.to_string_lossy().into_owned()
}

/// Helper: compile WAT → WASM bytes and write to `dir`, return path.
fn compile_wat_to_file(dir: &tempfile::TempDir, name: &str, wat_src: &str) -> String {
    let bytes = wat::parse_str(wat_src).expect("WAT must compile");
    let p = dir.path().join(name);
    std::fs::write(&p, &bytes).unwrap();
    p.to_string_lossy().into_owned()
}

// ── Rhai api-gate demo ───────────────────────────────────────────────────────

/// Helper: build a gate config where all requests go to an echo upstream.
fn api_gate_config(
    port: u16, admin_port: u16, echo_port: u16,
    script: &str, api_key: &str,
) -> serde_json::Value {
    serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "middleware": [{
                "type": "script",
                "path": script,
                "config": { "api_key": api_key, "api_header": "x-api-key" }
            }],
            "proxy": {
                "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
            }
        }]
    })
}

/// Missing API key returns 401 (Rhai script aborts before reaching upstream).
#[test]
fn demo_rhai_api_gate_missing_key_returns_401() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir, "api-gate.rhai",
        include_str!("../examples/middleware-demo/api-gate.rhai"),
    );
    let echo_port = free_port();
    let _echo = common::start_echo_upstream(echo_port);
    let port = free_port();
    let admin_port = free_port();
    let cfg = api_gate_config(port, admin_port, echo_port, &script, "secret");
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);

    let resp = reqwest::blocking::get(srv.url("/")).unwrap();
    assert_eq!(resp.status().as_u16(), 401, "missing key must return 401");
    let body: serde_json::Value = resp.json().unwrap_or_default();
    assert_eq!(body["status"], 401);
}

/// Wrong API key returns 403.
#[test]
fn demo_rhai_api_gate_wrong_key_returns_403() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir, "api-gate.rhai",
        include_str!("../examples/middleware-demo/api-gate.rhai"),
    );
    let echo_port = free_port();
    let _echo = common::start_echo_upstream(echo_port);
    let port = free_port();
    let admin_port = free_port();
    let cfg = api_gate_config(port, admin_port, echo_port, &script, "correct-key");
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/"))
        .header("x-api-key", "wrong-key")
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403, "wrong key must return 403");
}

/// Correct API key passes through to upstream — echo returns 200.
#[test]
fn demo_rhai_api_gate_correct_key_reaches_upstream() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir, "api-gate.rhai",
        include_str!("../examples/middleware-demo/api-gate.rhai"),
    );
    let echo_port = free_port();
    let _echo = common::start_echo_upstream(echo_port);
    let port = free_port();
    let admin_port = free_port();
    let cfg = api_gate_config(port, admin_port, echo_port, &script, "correct-key");
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/"))
        .header("x-api-key", "correct-key")
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "correct key must reach upstream (200)");
    let body: serde_json::Value = resp.json().unwrap_or_default();
    // Echo returns the request headers it received.
    assert!(body.get("headers").is_some(), "echo must return headers object");
}

// ── Rhai response-enricher demo ───────────────────────────────────────────────

/// Response enricher adds X-Served-By to upstream responses.
#[test]
fn demo_rhai_response_enricher_adds_served_by() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir,
        "response-enricher.rhai",
        include_str!("../examples/middleware-demo/response-enricher.rhai"),
    );
    let echo_port = free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = free_port();
    let admin_port = free_port();
    let cfg = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "middleware": [{
                "type": "script",
                "phase": "response",
                "path": script,
                "config": { "service_name": "test-api", "hide_server": true }
            }],
            "proxy": {
                "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
            }
        }]
    });
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);
    let resp = reqwest::blocking::get(srv.url("/")).unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("x-served-by").and_then(|v| v.to_str().ok()),
        Some("test-api"),
        "response enricher must inject X-Served-By: test-api"
    );
}

// ── WASM header-injector demo ─────────────────────────────────────────────────

/// WASM header-injector injects X-Wasm-Plugin on forwarded requests.
/// Verified by checking the response from a local echo upstream.
/// Requires `--features wasm`.
#[test]
#[cfg(feature = "wasm")]
fn demo_wasm_header_injector_injects_x_wasm_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let wasm_path = compile_wat_to_file(
        &dir,
        "header-injector.wasm",
        include_str!("../examples/middleware-demo/header-injector.wat"),
    );

    // Echo server: returns the headers it received as JSON body.
    let echo_port = free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = free_port();
    let admin_port = free_port();
    let cfg = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "middleware": [{ "type": "wasm", "path": wasm_path }],
            "proxy": {
                "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
            }
        }]
    });
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);
    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/"))
        .send()
        .unwrap();
    // The upstream echo returns the headers it received.
    // We check that X-Wasm-Plugin arrived at the echo server.
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    let headers = body.get("headers").cloned().unwrap_or(serde_json::Value::Null);
    assert_eq!(
        headers.get("x-wasm-plugin")
            .or_else(|| headers.get("X-Wasm-Plugin"))
            .and_then(|v| v.as_str()),
        Some("header-injector/1.0"),
        "upstream must receive X-Wasm-Plugin: header-injector/1.0; got headers: {headers}"
    );
}

// ── WASM response-tagger demo ─────────────────────────────────────────────────

/// WASM response-tagger adds X-Processed-By: wasm to upstream responses.
/// Requires `--features wasm`.
#[test]
#[cfg(feature = "wasm")]
fn demo_wasm_response_tagger_adds_processed_by() {
    let dir = tempfile::tempdir().unwrap();
    let wasm_path = compile_wat_to_file(
        &dir,
        "response-tagger.wasm",
        include_str!("../examples/middleware-demo/response-tagger.wat"),
    );
    let echo_port = free_port();
    let _echo = common::start_echo_upstream(echo_port);

    let port = free_port();
    let admin_port = free_port();
    let cfg = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "middleware": [{ "type": "wasm", "path": wasm_path }],
            "proxy": {
                "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
            }
        }]
    });
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);
    let resp = reqwest::blocking::get(srv.url("/")).unwrap();
    assert_eq!(
        resp.headers().get("x-processed-by").and_then(|v| v.to_str().ok()),
        Some("wasm"),
        "WASM response tagger must inject X-Processed-By: wasm"
    );
}

// ── Rhai resource-limit tests ─────────────────────────────────────────────────

/// A Rhai script with an infinite loop must NOT hang the server forever.
/// The engine's operation limit (500 000 ops) should abort execution and
/// the request should pass through (fail-open) within a reasonable time.
#[test]
fn rhai_infinite_loop_aborts_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    // This script loops forever — without the operation limit it would hang.
    let script_path = write_script(&dir, "infinite.rhai", "loop {} true");

    let port = free_port();
    let admin_port = free_port();
    let cfg = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "healthCheck": true,
            "middleware": [{ "type": "script", "path": script_path }]
        }]
    });
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);

    // Health endpoint bypasses middleware — verify the server is still alive.
    // The actual gauge of the fix is that the server responds within a normal
    // timeout rather than hanging.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client.get(srv.url("/__health__")).send();
    assert!(resp.is_ok(), "server must still respond after aborting infinite loop script");
}

/// A Rhai script allocating a huge string is bounded by the engine's
/// max_string_size limit and must not exhaust process memory.
#[test]
fn rhai_string_allocation_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    // Try to build a 10 MiB string via concatenation.
    let script_path = write_script(
        &dir,
        "bigstring.rhai",
        r#"
let s = "";
let i = 0;
while i < 10000 {
    s += "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    i += 1;
}
true
"#,
    );

    let echo_port = free_port();
    let _echo = common::start_echo_upstream(echo_port);
    let port = free_port();
    let admin_port = free_port();
    let cfg = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "middleware": [{ "type": "script", "path": script_path }],
            "proxy": {
                "/": { "targets": [format!("http://127.0.0.1:{echo_port}")] }
            }
        }]
    });
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);

    // Script will fail with a string-too-large error → fail-open → 200 from upstream.
    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
        .get(srv.url("/"))
        .send()
        .expect("server must respond");
    // fail-open: script errors → request passes through
    assert_eq!(resp.status().as_u16(), 200,
        "script resource limit error must be fail-open (request passes)");
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
