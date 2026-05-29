mod common;

use serial_test::serial;

fn srv_with_headers(headers: serde_json::Value) -> (common::TestServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<html/>").expect("write");
    let port = common::free_port();
    let admin_port = common::free_port();
    let static_dir = dir.path().to_string_lossy().into_owned();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "headers": headers,
                "static": static_dir
            }]
        }),
    );
    (srv, dir)
}

// ── custom headers on static responses ───────────────────────────────────

#[test]
#[serial]
fn custom_header_present_on_static_response() {
    let (srv, _dir) = srv_with_headers(serde_json::json!({ "X-Powered-By": "Conduit" }));
    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("x-powered-by")
            .and_then(|v| v.to_str().ok()),
        Some("Conduit"),
        "custom header X-Powered-By must be present"
    );
}

#[test]
#[serial]
fn multiple_custom_headers_all_present() {
    let (srv, _dir) = srv_with_headers(serde_json::json!({
        "X-Version": "1.0",
        "X-Environment": "test",
        "Cache-Control": "no-store"
    }));

    let resp = reqwest::blocking::get(srv.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);

    assert_eq!(
        resp.headers()
            .get("x-version")
            .and_then(|v| v.to_str().ok()),
        Some("1.0")
    );
    assert_eq!(
        resp.headers()
            .get("x-environment")
            .and_then(|v| v.to_str().ok()),
        Some("test")
    );
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
}

// ── custom headers on health endpoint ────────────────────────────────────

#[test]
#[serial]
fn custom_header_present_on_health_response() {
    let (srv, _dir) = srv_with_headers(serde_json::json!({ "X-Node": "node-1" }));
    let resp = reqwest::blocking::get(srv.url("/__health__")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("x-node").and_then(|v| v.to_str().ok()),
        Some("node-1"),
        "custom header must also appear on /__health__"
    );
}

// ── custom headers on 404 fallback ───────────────────────────────────────

#[test]
#[serial]
fn custom_header_present_on_404_fallback() {
    let (srv, _dir) = srv_with_headers(serde_json::json!({ "X-Served-By": "conduit" }));
    let resp = reqwest::blocking::get(srv.url("/no-such-file")).expect("GET");
    // 404 from the fallback handler
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.headers()
            .get("x-served-by")
            .and_then(|v| v.to_str().ok()),
        Some("conduit"),
        "custom header must appear on 404 fallback responses too"
    );
}
