mod common;

use serial_test::serial;

/// Start a server with two named virtual hosts plus a catch-all on the same port.
///
/// Returns (server, app_dir, admin_dir, port).  The two tempdir values must
/// stay alive for the duration of the test.
fn two_host_server() -> (
    common::TestServer,
    tempfile::TempDir,
    tempfile::TempDir,
    u16,
) {
    let app_dir = tempfile::tempdir().expect("tempdir");
    let admin_dir = tempfile::tempdir().expect("tempdir");

    std::fs::write(app_dir.path().join("index.html"), "APP SITE").expect("write");
    std::fs::write(admin_dir.path().join("index.html"), "ADMIN SITE").expect("write");

    let port = common::free_port();
    let admin_port = common::free_port();

    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [
                {
                    "host": "app.example.com",
                    "port": port,
                    "static": app_dir.path().to_str().unwrap()
                },
                {
                    "host": "admin.example.com",
                    "port": port,
                    "static": admin_dir.path().to_str().unwrap()
                },
                {
                    // catch-all so wait_ready (127.0.0.1 Host) succeeds
                    "host": "*",
                    "port": port,
                    "healthCheck": true,
                    "fallback": { "status": 404, "body": "Unknown host" }
                }
            ]
        }),
    );

    (srv, app_dir, admin_dir, port)
}

// ── named host routing ────────────────────────────────────────────────────

#[test]
#[serial]
fn request_with_app_host_serves_app_content() {
    let (srv, _app_dir, _admin_dir, _) = two_host_server();

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/index.html"))
        .header("Host", "app.example.com")
        .send()
        .expect("GET");

    assert_eq!(resp.status(), 200);
    let body = resp.text().expect("body");
    assert_eq!(body.trim(), "APP SITE", "wrong virtual host served");
}

#[test]
#[serial]
fn request_with_admin_host_serves_admin_content() {
    let (srv, _app_dir, _admin_dir, _) = two_host_server();

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/index.html"))
        .header("Host", "admin.example.com")
        .send()
        .expect("GET");

    assert_eq!(resp.status(), 200);
    let body = resp.text().expect("body");
    assert_eq!(body.trim(), "ADMIN SITE", "wrong virtual host served");
}

#[test]
#[serial]
fn unknown_host_falls_through_to_catch_all() {
    let (srv, _app_dir, _admin_dir, _) = two_host_server();

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/"))
        .header("Host", "unknown.example.com")
        .send()
        .expect("GET");

    // The catch-all site has fallback: { status: 404, body: "Unknown host" }
    assert_eq!(resp.status().as_u16(), 404);
    let body = resp.text().expect("body");
    assert!(
        body.contains("Unknown host"),
        "catch-all fallback body expected, got: '{body}'"
    );
}

// ── no host field → matches any host ─────────────────────────────────────

#[test]
#[serial]
fn site_without_host_matches_any_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "GENERIC").expect("write");
    let static_dir = dir.path().to_string_lossy().into_owned();
    Box::leak(Box::new(dir));

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "static": static_dir }]
        }),
    );

    // Request with an arbitrary Host header
    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/index.html"))
        .header("Host", "anything.example.com")
        .send()
        .expect("GET");

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().expect("body").trim(), "GENERIC");
}

// ── isolation: site A cannot see site B's files ───────────────────────────

#[test]
#[serial]
fn each_virtual_host_is_isolated() {
    let (srv, _app_dir, _admin_dir, _) = two_host_server();

    // app.example.com should NOT find admin's index.html with different content —
    // we can't check file isolation directly, but we CAN verify the content differs.
    let app_resp = reqwest::blocking::Client::new()
        .get(srv.url("/index.html"))
        .header("Host", "app.example.com")
        .send()
        .expect("GET app");
    let admin_resp = reqwest::blocking::Client::new()
        .get(srv.url("/index.html"))
        .header("Host", "admin.example.com")
        .send()
        .expect("GET admin");

    let app_body = app_resp.text().expect("app body");
    let admin_body = admin_resp.text().expect("admin body");
    assert_ne!(
        app_body.trim(),
        admin_body.trim(),
        "virtual hosts should serve different content"
    );
}
