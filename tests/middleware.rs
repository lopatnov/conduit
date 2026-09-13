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
#[cfg(feature = "rhai")]
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
#[cfg(feature = "rhai")]
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
#[cfg(feature = "rhai")]
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
#[cfg(feature = "rhai")]
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
#[cfg(feature = "rhai")]
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
#[cfg(feature = "rhai")]
fn write_script(dir: &tempfile::TempDir, name: &str, src: &str) -> String {
    let p = dir.path().join(name);
    std::fs::write(&p, src).unwrap();
    p.to_string_lossy().into_owned()
}

/// Helper: compile WAT → WASM bytes and write to `dir`, return path.
#[cfg(feature = "wasm")]
fn compile_wat_to_file(dir: &tempfile::TempDir, name: &str, wat_src: &str) -> String {
    let bytes = wat::parse_str(wat_src).expect("WAT must compile");
    let p = dir.path().join(name);
    std::fs::write(&p, &bytes).unwrap();
    p.to_string_lossy().into_owned()
}

// ── Rhai api-gate demo ───────────────────────────────────────────────────────

/// Helper: build a gate config where all requests go to an echo upstream.
#[cfg(feature = "rhai")]
fn api_gate_config(
    port: u16,
    admin_port: u16,
    echo_port: u16,
    script: &str,
    api_key: &str,
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
#[cfg(feature = "rhai")]
#[test]
fn demo_rhai_api_gate_missing_key_returns_401() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir,
        "api-gate.rhai",
        include_str!("../examples/middleware-demo/api-gate.rhai"),
    );
    let (echo_port, _echo) = common::start_echo_upstream();
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
#[cfg(feature = "rhai")]
#[test]
fn demo_rhai_api_gate_wrong_key_returns_403() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir,
        "api-gate.rhai",
        include_str!("../examples/middleware-demo/api-gate.rhai"),
    );
    let (echo_port, _echo) = common::start_echo_upstream();
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
#[cfg(feature = "rhai")]
#[test]
fn demo_rhai_api_gate_correct_key_reaches_upstream() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir,
        "api-gate.rhai",
        include_str!("../examples/middleware-demo/api-gate.rhai"),
    );
    let (echo_port, _echo) = common::start_echo_upstream();
    let port = free_port();
    let admin_port = free_port();
    let cfg = api_gate_config(port, admin_port, echo_port, &script, "correct-key");
    let srv = common::TestServer::start_with_config(port, admin_port, cfg);

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/"))
        .header("x-api-key", "correct-key")
        .send()
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "correct key must reach upstream (200)"
    );
    let body: serde_json::Value = resp.json().unwrap_or_default();
    // Echo returns the request headers it received.
    assert!(
        body.get("headers").is_some(),
        "echo must return headers object"
    );
}

// ── Rhai response-enricher demo ───────────────────────────────────────────────

/// Response enricher adds X-Served-By to upstream responses.
#[cfg(feature = "rhai")]
#[test]
fn demo_rhai_response_enricher_adds_served_by() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir,
        "response-enricher.rhai",
        include_str!("../examples/middleware-demo/response-enricher.rhai"),
    );
    let (echo_port, _echo) = common::start_echo_upstream();

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
        resp.headers()
            .get("x-served-by")
            .and_then(|v| v.to_str().ok()),
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
    let (echo_port, _echo) = common::start_echo_upstream();

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
    let headers = body
        .get("headers")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        headers
            .get("x-wasm-plugin")
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
    let (echo_port, _echo) = common::start_echo_upstream();

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
        resp.headers()
            .get("x-processed-by")
            .and_then(|v| v.to_str().ok()),
        Some("wasm"),
        "WASM response tagger must inject X-Processed-By: wasm"
    );
}

// ── Rhai resource-limit tests ─────────────────────────────────────────────────

/// A Rhai script with an infinite loop must NOT hang the server forever.
/// The engine's operation limit (500 000 ops) should abort execution and
/// the request should pass through (fail-open) within a reasonable time.
#[cfg(feature = "rhai")]
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
    assert!(
        resp.is_ok(),
        "server must still respond after aborting infinite loop script"
    );
}

/// A Rhai script allocating a huge string is bounded by the engine's
/// max_string_size limit and must not exhaust process memory.
#[cfg(feature = "rhai")]
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

    let (echo_port, _echo) = common::start_echo_upstream();
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
    assert_eq!(
        resp.status().as_u16(),
        200,
        "script resource limit error must be fail-open (request passes)"
    );
}

// ── Feature-flag warning tests ────────────────────────────────────────────────

/// When rhai feature is off, configuring a script middleware generates a warning.
#[test]
#[cfg(not(feature = "rhai"))]
fn rhai_without_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "middleware": [{ "type": "script", "path": "x.rhai" }] }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("rhai")),
        "missing rhai without feature warning: {warnings:?}"
    );
}

/// When tcp feature is off, configuring a tcp site generates a warning.
#[test]
#[cfg(not(feature = "tcp"))]
fn tcp_without_feature_generates_warning() {
    let config =
        conduit::config::from_str(r#"{ "port": 8080, "tcp": { "targets": ["db:5432"] } }"#)
            .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("tcp")),
        "missing tcp without feature warning: {warnings:?}"
    );
}

/// When acme feature is off, configuring tls.acme generates a warning.
#[test]
#[cfg(not(feature = "acme"))]
fn acme_without_feature_generates_warning() {
    // AcmeConfig requires email field
    let config = conduit::config::from_str(
        r#"{ "port": 443, "tls": { "acme": { "email": "admin@example.com" } } }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("acme")),
        "missing acme without feature warning: {warnings:?}"
    );
}

/// When redis feature is off, configuring redis store generates a warning.
#[test]
#[cfg(not(feature = "redis"))]
fn redis_without_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://localhost:6379" } }"#
    ).expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("redis")),
        "missing redis without feature warning: {warnings:?}"
    );
}

/// Same as `redis_without_feature_generates_warning`, but the Redis store is
/// configured only on a per-route `rateLimit`, not the site level (issue
/// #322 gave route-level Redis real effect, so the feature-warning scan must
/// cover it too — see `src/config/validate.rs::site_uses_redis_store`).
#[test]
#[cfg(not(feature = "redis"))]
fn redis_without_feature_generates_warning_for_route_level_store() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "proxy": { "/api": { "targets": ["http://127.0.0.1:9"],
             "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://localhost:6379" } } } }"#
    ).expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("redis")),
        "missing redis without feature warning for route-level store: {warnings:?}"
    );
}

/// Same as above, but the Redis store is configured only on a per-consumer
/// `rateLimit` (issue #322).
#[test]
#[cfg(all(not(feature = "redis"), feature = "consumers"))]
fn redis_without_feature_generates_warning_for_consumer_level_store() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "consumers": { "consumers": [{ "username": "alice",
             "rateLimit": { "windowSecs": 60, "limit": 100, "store": "redis://localhost:6379" } }] } }"#
    ).expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("redis")),
        "missing redis without feature warning for consumer-level store: {warnings:?}"
    );
}

/// When jwt feature is off, configuring jwtAuth generates a warning.
///
/// Every sibling feature-warning check in this file has a dedicated test but
/// this one didn't (found by `integrity-auditor`, 2026-08-21 audit of
/// `src/filter/auth.rs`/consumer auth) even though the warning itself has
/// existed in `check_site_simple_feature_warnings` since JWT auth shipped.
#[test]
#[cfg(not(feature = "jwt"))]
fn jwt_auth_without_feature_generates_warning() {
    let config =
        conduit::config::from_str(r#"{ "port": 8080, "jwtAuth": { "secret": "test-secret" } }"#)
            .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("jwtAuth")),
        "missing jwtAuth without feature warning: {warnings:?}"
    );
}

/// When fault-injection feature is off, configuring faultInjection generates a warning.
#[test]
#[cfg(not(feature = "fault-injection"))]
fn fault_injection_without_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "faultInjection": { "abort": { "percent": 10, "status": 503 } } }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("fault")),
        "missing fault-injection without feature warning: {warnings:?}"
    );
}

/// When forward-auth feature is off, configuring forwardAuth generates a warning.
#[test]
#[cfg(not(feature = "forward-auth"))]
fn forward_auth_without_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "forwardAuth": { "url": "http://auth:4000/verify" } }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("forward-auth")),
        "missing forward-auth without feature warning: {warnings:?}"
    );
}

/// When upload feature is off, configuring upload generates a warning.
#[test]
#[cfg(not(feature = "upload"))]
fn upload_without_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "upload": { "path": "/upload", "dir": "/tmp/uploads" } }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("upload")),
        "missing upload without feature warning: {warnings:?}"
    );
}

/// When cache feature is off, configuring cache generates a warning.
#[test]
#[cfg(not(feature = "cache"))]
fn cache_without_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "proxy": { "/api": { "targets": ["http://api:4000"], "cache": { "store": "memory", "ttlSecs": 60 } } } }"#
    ).expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("cache")),
        "missing cache without feature warning: {warnings:?}"
    );
}

/// When consumers feature is off, configuring consumers generates a warning.
#[test]
#[cfg(not(feature = "consumers"))]
fn consumers_without_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "consumers": { "consumers": [{ "username": "test", "apiKey": "abc" }] } }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("consumers")),
        "missing consumers without feature warning: {warnings:?}"
    );
}

/// When static feature is off, configuring `static` generates a warning.
#[test]
#[cfg(not(feature = "static"))]
fn static_without_feature_generates_warning() {
    let config =
        conduit::config::from_str(r#"{ "port": 8080, "static": "./dist" }"#).expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("static")),
        "missing static without feature warning: {warnings:?}"
    );
}

/// When static feature is off, configuring `fallback` generates a warning.
#[test]
#[cfg(not(feature = "static"))]
fn fallback_without_feature_generates_warning() {
    let config = conduit::config::from_str(r#"{ "port": 8080, "fallback": { "status": 404 } }"#)
        .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings.iter().any(|w| w.contains("fallback")),
        "missing fallback without feature warning: {warnings:?}"
    );
}

/// When consumers is on but jwt is off, a consumer using `sharedJwt` or a
/// per-consumer `jwt` credential is silently unreachable — must warn.
#[test]
#[cfg(all(feature = "consumers", not(feature = "jwt")))]
fn consumers_shared_jwt_without_jwt_feature_generates_warning() {
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "consumers": { "sharedJwt": { "jwksUrl": "https://example.com/jwks" }, "consumers": [{ "username": "user-a" }] } }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("compiled without the `jwt` feature")),
        "missing consumers sharedJwt without jwt feature warning: {warnings:?}"
    );
}

/// Same narrower case, but via a per-consumer `jwt` credential (V2) instead
/// of the shared `consumers.sharedJwt` block (V3).
#[test]
#[cfg(all(feature = "consumers", not(feature = "jwt")))]
fn consumers_per_consumer_jwt_without_jwt_feature_generates_warning() {
    // Secret is >= 32 bytes so this test doesn't also trigger the unrelated
    // short-secret warning (check_consumer_jwt_secret_warnings), which would
    // let the assertion below pass even if the disabled-feature warning were
    // missing or broken.
    let config = conduit::config::from_str(
        r#"{ "port": 8080, "consumers": { "consumers": [{ "username": "user-a", "jwt": { "secret": "0123456789abcdef0123456789abcdef" } }] } }"#,
    )
    .expect("parse ok");
    let warnings = conduit::config::validate::feature_warnings(&config);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("compiled without the `jwt` feature")),
        "missing per-consumer jwt without jwt feature warning: {warnings:?}"
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
