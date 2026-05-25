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
