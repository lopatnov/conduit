mod common;

use serial_test::serial;

fn fallback_server(
    fallback: serde_json::Value,
    extra_site_fields: serde_json::Value,
) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    let mut site = serde_json::json!({
        "port": port,
        "fallback": fallback
    });
    // Merge any extra fields (e.g. static root) into the site config.
    if let (Some(site_map), Some(extra_map)) = (site.as_object_mut(), extra_site_fields.as_object())
    {
        for (k, v) in extra_map {
            site_map.insert(k.clone(), v.clone());
        }
    }
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [site]
        }),
    )
}

// ── fallback: { status, body (string) } ──────────────────────────────────

#[test]
#[serial]
fn fallback_custom_status_and_text_body() {
    let srv = fallback_server(
        serde_json::json!({ "status": 503, "body": "Maintenance mode" }),
        serde_json::json!({}),
    );

    let resp = reqwest::blocking::get(srv.url("/any-path")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        503,
        "custom fallback status must be used"
    );
    let body = resp.text().expect("body");
    assert!(
        body.contains("Maintenance mode"),
        "custom fallback body expected, got: '{body}'"
    );
}

// ── fallback: { status, body (JSON object) } ─────────────────────────────

#[test]
#[serial]
fn fallback_json_body_is_returned() {
    let srv = fallback_server(
        serde_json::json!({ "status": 404, "body": { "error": "Not Found", "code": 404 } }),
        serde_json::json!({}),
    );

    let resp = reqwest::blocking::get(srv.url("/missing")).expect("GET");
    assert_eq!(resp.status().as_u16(), 404);

    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["error"], "Not Found");
    assert_eq!(body["code"], 404);
}

// ── fallback: { status: 200, file: "..." } ───────────────────────────────

#[test]
#[serial]
fn fallback_serves_file_with_200() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<h1>SPA Shell</h1>").expect("write");
    let file_path = dir
        .path()
        .join("index.html")
        .to_string_lossy()
        .into_owned();
    Box::leak(Box::new(dir));

    let srv = fallback_server(
        serde_json::json!({ "status": 200, "file": file_path }),
        serde_json::json!({}),
    );

    // Any path not found falls back to serving the SPA shell.
    let resp = reqwest::blocking::get(srv.url("/deep/route/here")).expect("GET");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().expect("body");
    assert!(
        body.contains("SPA Shell"),
        "SPA fallback file should be served, got: '{body}'"
    );
}

// ── default fallback (no config) → 404 ───────────────────────────────────

#[test]
#[serial]
fn no_fallback_config_returns_404() {
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/no-such-path")).expect("GET");
    assert_eq!(resp.status().as_u16(), 404, "default is 404 without fallback config");
}

// ── fallback: byAccept ────────────────────────────────────────────────────

#[test]
#[serial]
fn fallback_by_accept_html_returns_spa() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("shell.html"), "<html>SPA</html>").expect("write");
    let shell_path = dir
        .path()
        .join("shell.html")
        .to_string_lossy()
        .into_owned();
    Box::leak(Box::new(dir));

    let srv = fallback_server(
        serde_json::json!({
            "byAccept": {
                "html": { "status": 200, "file": shell_path },
                "json": { "status": 404, "body": { "error": "Not Found" } },
                "*":    { "status": 404, "body": "Not Found" }
            }
        }),
        serde_json::json!({}),
    );

    let client = reqwest::blocking::Client::new();

    // HTML client → SPA shell
    let html_resp = client
        .get(srv.url("/deep/path"))
        .header("Accept", "text/html,application/xhtml+xml")
        .send()
        .expect("GET html");
    assert_eq!(html_resp.status().as_u16(), 200);
    assert!(html_resp.text().expect("body").contains("SPA"));

    // JSON client → 404 JSON error
    let json_resp = client
        .get(srv.url("/deep/path"))
        .header("Accept", "application/json")
        .send()
        .expect("GET json");
    assert_eq!(json_resp.status().as_u16(), 404);
    let err: serde_json::Value = json_resp.json().expect("JSON");
    assert_eq!(err["error"], "Not Found");
}

// ── fallback custom headers ───────────────────────────────────────────────

#[test]
#[serial]
fn fallback_custom_headers_are_sent() {
    let srv = fallback_server(
        serde_json::json!({
            "status": 404,
            "body": "gone",
            "headers": { "X-Fallback": "yes" }
        }),
        serde_json::json!({}),
    );

    let resp = reqwest::blocking::get(srv.url("/missing")).expect("GET");
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(
        resp.headers()
            .get("x-fallback")
            .and_then(|v| v.to_str().ok()),
        Some("yes"),
        "fallback custom header must be present"
    );
}
