mod common;

use serial_test::serial;

/// Helper: build the standard full-form config used across reload tests.
fn base_config(port: u16, admin_port: u16) -> serde_json::Value {
    serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "headers": { "X-Version": "v1" }
        }]
    })
}

// ── Hot reload — success cases ────────────────────────────────────────────────

/// A hot-field change (custom response header) is applied without restart.
#[test]
#[serial]
fn reload_hot_field_applies_without_restart() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let srv =
        common::TestServer::start_with_config(port, admin_port, base_config(port, admin_port));

    // Confirm v1 header before reload.
    let r1 = reqwest::blocking::get(srv.url("/__health__")).expect("GET before reload");
    assert_eq!(
        r1.headers().get("x-version").and_then(|v| v.to_str().ok()),
        Some("v1")
    );

    // Write new config with v2 header and reload.
    srv.rewrite_config(serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "headers": { "X-Version": "v2" }
        }]
    }));

    let reload_resp = srv.reload();
    assert_eq!(
        reload_resp["status"], "ok",
        "reload must succeed for hot field: {reload_resp}"
    );

    // Allow the new config to propagate (ArcSwap is wait-free but the handler
    // must pick up the new Arc on the next request).
    std::thread::sleep(std::time::Duration::from_millis(100));

    let r2 = reqwest::blocking::get(srv.url("/__health__")).expect("GET after reload");
    assert_eq!(
        r2.headers().get("x-version").and_then(|v| v.to_str().ok()),
        Some("v2"),
        "new header value must be visible after reload"
    );
}

/// Reloading with an identical config returns `status: ok`.
#[test]
#[serial]
fn reload_identical_config_is_ok() {
    let port = common::free_port();
    let admin_port = common::free_port();
    let cfg = base_config(port, admin_port);

    let srv = common::TestServer::start_with_config(port, admin_port, cfg.clone());

    // Rewrite with the exact same content.
    srv.rewrite_config(cfg);
    let resp = srv.reload();
    assert_eq!(resp["status"], "ok", "identical reload must be ok: {resp}");
}

/// Reloading with a broken JSON file returns a parse error.
#[test]
#[serial]
fn reload_invalid_config_returns_error() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let srv =
        common::TestServer::start_with_config(port, admin_port, base_config(port, admin_port));

    // Write garbage JSON.
    std::fs::write(&srv.cfg_path, b"{ not valid json !!").expect("write bad config");

    let resp = srv.reload();
    assert_eq!(
        resp["status"], "error",
        "malformed config must return error: {resp}"
    );
    assert!(
        resp["message"]
            .as_str()
            .unwrap_or("")
            .contains("failed to parse config"),
        "error message must mention parse failure: {resp}"
    );
}

// ── Hot reload — cold-field rejection ────────────────────────────────────────

/// Changing `sites[0].port` is a cold field — reload must be rejected.
#[test]
#[serial]
fn reload_cold_field_port_rejected() {
    let port = common::free_port();
    let other_port = common::free_port();
    let admin_port = common::free_port();

    let srv =
        common::TestServer::start_with_config(port, admin_port, base_config(port, admin_port));

    srv.rewrite_config(serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": other_port,   // ← changed
            "headers": { "X-Version": "v2" }
        }]
    }));

    let resp = srv.reload();
    assert_eq!(
        resp["status"], "error",
        "cold field change must be rejected: {resp}"
    );
    let cold: Vec<String> = serde_json::from_value(resp["cold_fields"].clone()).unwrap_or_default();
    assert!(
        cold.iter().any(|f| f.contains("port")),
        "cold_fields must mention port: {cold:?}"
    );
}

/// Changing `global.workers` is a cold field — reload must be rejected.
#[test]
#[serial]
fn reload_cold_field_workers_rejected() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let srv =
        common::TestServer::start_with_config(port, admin_port, base_config(port, admin_port));

    srv.rewrite_config(serde_json::json!({
        "global": {
            "admin": { "bind": format!("127.0.0.1:{admin_port}") },
            "workers": 8   // ← cold field
        },
        "sites": [{ "port": port }]
    }));

    let resp = srv.reload();
    assert_eq!(
        resp["status"], "error",
        "workers change must be rejected: {resp}"
    );
    let cold: Vec<String> = serde_json::from_value(resp["cold_fields"].clone()).unwrap_or_default();
    assert!(
        cold.iter().any(|f| f.contains("workers")),
        "cold_fields must mention workers: {cold:?}"
    );
}

/// Changing `sites[0].tls.cert`/`.key` is a cold field — reload must be
/// rejected. Uses placeholder (non-existent) cert/key paths: cold-field
/// detection compares the configured path strings before any file is ever
/// read, so no real PEM content is needed to trigger it (mirrors how
/// `check_cert_expiry` itself silently skips a missing file at validation
/// time — see `src/config/validate.rs`).
#[test]
#[serial]
fn reload_cold_field_tls_cert_rejected() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let srv =
        common::TestServer::start_with_config(port, admin_port, base_config(port, admin_port));

    srv.rewrite_config(serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "tls": {
                "cert": "/nonexistent/cert.pem",   // ← cold field, first set
                "key": "/nonexistent/key.pem"
            }
        }]
    }));

    let resp = srv.reload();
    assert_eq!(
        resp["status"], "error",
        "tls.cert/tls.key change must be rejected: {resp}"
    );
    let cold: Vec<String> = serde_json::from_value(resp["cold_fields"].clone()).unwrap_or_default();
    assert!(
        cold.iter().any(|f| f.contains("tls.cert")),
        "cold_fields must mention tls.cert: {cold:?}"
    );
    assert!(
        cold.iter().any(|f| f.contains("tls.key")),
        "cold_fields must mention tls.key: {cold:?}"
    );
}

// ── Hot reload — CORS config change ──────────────────────────────────────────

/// Adding CORS on reload causes the CORS header to appear in subsequent responses.
#[test]
#[serial]
fn reload_cors_change_takes_effect() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "healthCheck": true }]
        }),
    );

    // Before reload: no CORS headers.
    let r1 = reqwest::blocking::Client::new()
        .get(srv.url("/__health__"))
        .header("Origin", "https://example.com")
        .send()
        .expect("GET before reload");
    assert!(
        r1.headers().get("access-control-allow-origin").is_none(),
        "should have no CORS header before reload"
    );

    // Enable CORS and reload.
    srv.rewrite_config(serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "healthCheck": true,
            "cors": {
                "origins": ["https://example.com"],
                "credentials": false
            }
        }]
    }));
    let reload_resp = srv.reload();
    assert_eq!(
        reload_resp["status"], "ok",
        "reload must succeed: {reload_resp}"
    );

    std::thread::sleep(std::time::Duration::from_millis(100));

    let r2 = reqwest::blocking::Client::new()
        .get(srv.url("/__health__"))
        .header("Origin", "https://example.com")
        .send()
        .expect("GET after reload");
    let acao = r2
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        acao,
        Some("https://example.com"),
        "CORS header must appear after reload"
    );
}

// ── Hot reload — rate limit change ───────────────────────────────────────────

/// Removing rate limiting on reload allows previously-blocked requests through.
///
/// Note: `/__health__` bypasses rate limiting by design, so this test uses the
/// regular fallback path `/` which goes through the full filter pipeline.
#[test]
#[serial]
fn reload_rate_limit_removal_takes_effect() {
    let port = common::free_port();
    let admin_port = common::free_port();

    // Start with a very tight rate limit: 1 req / 60 s.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "rateLimit": { "windowSecs": 60, "limit": 1 }
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();

    // First request to a non-health path: should succeed.
    let r1 = client.get(srv.url("/")).send().expect("first GET");
    assert_eq!(
        r1.status(),
        404,
        "first request should reach the fallback (no 429 yet)"
    );

    // Second request on same IP: should be rate-limited (429).
    let r2 = client.get(srv.url("/")).send().expect("second GET");
    assert_eq!(r2.status(), 429, "second request must be rate-limited");

    // Remove rate limit and reload.
    srv.rewrite_config(serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{ "port": port, "healthCheck": true }]
    }));
    let reload_resp = srv.reload();
    assert_eq!(
        reload_resp["status"], "ok",
        "reload must succeed: {reload_resp}"
    );

    std::thread::sleep(std::time::Duration::from_millis(100));

    // After reload: rate limiter is reset, no limit configured → regular 404 (not 429).
    let r3 = client.get(srv.url("/")).send().expect("GET after reload");
    assert_eq!(
        r3.status(),
        404,
        "request must reach the fallback (not be rate-limited) after reload"
    );
}

// ── Hot reload — log file switch ─────────────────────────────────────────────

/// Reload with a new logging.file path causes the server to write logs there.
#[test]
#[serial]
fn reload_log_file_switch_takes_effect() {
    use std::io::{BufRead, BufReader};

    let port = common::free_port();
    let admin_port = common::free_port();
    let log_dir = tempfile::tempdir().expect("tempdir");
    let log_path = log_dir.path().join("access.log");
    let log_path_str = log_path.to_string_lossy().into_owned();

    // Start without logging.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "healthCheck": true }]
        }),
    );

    // Enable file logging and reload.
    srv.rewrite_config(serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "healthCheck": true,
            "logging": { "format": "combined", "file": log_path_str }
        }]
    }));
    let reload_resp = srv.reload();
    assert_eq!(
        reload_resp["status"], "ok",
        "reload must succeed: {reload_resp}"
    );

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send a request — should produce a log entry.
    reqwest::blocking::get(srv.url("/__health__")).expect("GET after reload");
    std::thread::sleep(std::time::Duration::from_millis(100));

    assert!(log_path.exists(), "log file must be created after reload");
    let lines: Vec<String> = BufReader::new(std::fs::File::open(&log_path).expect("open log"))
        .lines()
        .collect::<std::io::Result<Vec<_>>>()
        .expect("read log");
    assert!(
        !lines.is_empty(),
        "log file must contain at least one entry"
    );
    assert!(
        lines[0].contains("/__health__"),
        "log entry must mention the request path: {:?}",
        lines[0]
    );
}
