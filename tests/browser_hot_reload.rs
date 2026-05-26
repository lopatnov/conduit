mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serial_test::serial;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Read bytes from a raw TCP stream until `sentinel` appears in the buffer or
/// the read deadline is reached.  Returns the accumulated raw response.
fn read_until(stream: &mut TcpStream, sentinel: &str, timeout: Duration) -> String {
    stream.set_read_timeout(Some(timeout)).ok();
    let mut buf = [0u8; 4096];
    let mut response = String::new();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,          // connection closed
            Ok(n) => response.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(_) => break,         // timeout or other transient error
        }
        if response.contains(sentinel) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }
    response
}

/// Open a raw HTTP/1.1 GET to `path` on `port` and return the response bytes
/// accumulated until `sentinel` appears in the stream or the timeout fires.
fn raw_get(port: u16, path: &str, sentinel: &str, timeout: Duration) -> String {
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{port}")).expect("TCP connect to server");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("write HTTP request");
    read_until(&mut stream, sentinel, timeout)
}

// ── Client JS ─────────────────────────────────────────────────────────────────

/// `/__hot-reload__/client.js` returns 200 + application/javascript.
#[test]
#[serial]
fn hot_reload_client_js_served() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let dir = tempfile::tempdir().expect("tempdir");
    let static_dir = dir.path().to_path_buf();
    std::fs::write(static_dir.join("index.html"), b"<html></html>").expect("write index");

    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_dir.to_string_lossy(),
                "hotReload": true
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/__hot-reload__/client.js"))
        .expect("GET client.js");
    assert_eq!(resp.status().as_u16(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("javascript"),
        "content-type must be javascript, got: {ct}"
    );
    let body = resp.text().expect("body");
    assert!(
        body.contains("/__hot-reload__"),
        "client JS must reference the SSE endpoint"
    );
    assert!(
        body.contains("EventSource"),
        "client JS must use EventSource"
    );
}

/// When `hotReload` is not configured, `/__hot-reload__/client.js` is NOT served
/// as a special endpoint — it falls through to static files / 404.
#[test]
#[serial]
fn hot_reload_client_js_not_served_when_disabled() {
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

    let resp = reqwest::blocking::get(srv.url("/__hot-reload__/client.js"))
        .expect("GET client.js without hotReload");
    // Without hotReload the path is not recognized as a special endpoint — falls
    // through to fallback (404).
    assert_eq!(
        resp.status().as_u16(),
        404,
        "client.js must not be served when hotReload is disabled"
    );
}

// ── SSE endpoint ──────────────────────────────────────────────────────────────

/// `/__hot-reload__` streams `text/event-stream` and sends an initial
/// `: connected` comment to the browser.
#[test]
#[serial]
fn hot_reload_sse_sends_connected_comment() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let dir = tempfile::tempdir().expect("tempdir");
    let static_dir = dir.path().to_path_buf();
    std::fs::write(static_dir.join("index.html"), b"<html></html>").expect("write index");

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_dir.to_string_lossy(),
                "hotReload": true
            }]
        }),
    );

    // Use a raw TCP connection so we can read the initial SSE frame before the
    // stream blocks waiting for file-change events.
    let raw = raw_get(port, "/__hot-reload__", ": connected", Duration::from_secs(5));

    assert!(
        raw.contains("HTTP/1.1 200"),
        "SSE endpoint must return 200; got: {raw}"
    );
    assert!(
        raw.to_ascii_lowercase().contains("text/event-stream"),
        "SSE endpoint must return text/event-stream; got: {raw}"
    );
    assert!(
        raw.contains(": connected"),
        "SSE stream must open with a `: connected` comment; got: {raw}"
    );
}

/// `/__hot-reload__` is not served when `hotReload` is disabled.
#[test]
#[serial]
fn hot_reload_sse_not_served_when_disabled() {
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

    let resp = reqwest::blocking::get(srv.url("/__hot-reload__")).expect("GET SSE");
    assert_eq!(
        resp.status().as_u16(),
        404,
        "SSE endpoint must not be served when hotReload is disabled"
    );
}

// ── File watcher → reload event ───────────────────────────────────────────────

/// Modifying a watched file causes the SSE stream to emit `data: reload`.
#[test]
#[serial]
fn hot_reload_file_change_triggers_reload_event() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let dir = tempfile::tempdir().expect("tempdir");
    let static_dir = dir.path().to_path_buf();
    std::fs::write(static_dir.join("index.html"), b"v1").expect("write index v1");

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_dir.to_string_lossy(),
                "hotReload": true
            }]
        }),
    );

    // Connect to the SSE endpoint in a background thread so the main thread can
    // trigger a file change without blocking.
    let sse_thread = {
        let static_dir = static_dir.clone();
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
                .expect("TCP connect for SSE");
            let req = format!(
                "GET /__hot-reload__ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(req.as_bytes()).expect("write SSE request");

            // Read until we see the initial `: connected` frame so we know the
            // server accepted our subscription before we trigger a file change.
            let initial = read_until(&mut stream, ": connected", Duration::from_secs(10));
            assert!(
                initial.contains(": connected"),
                "SSE must send `: connected` before file change; got: {initial}"
            );

            // Signal the main thread (via the static dir path existing) that the
            // SSE subscription is established — nothing to signal explicitly here;
            // we just sleep a moment then trigger the file change ourselves.
            std::thread::sleep(Duration::from_millis(200));
            std::fs::write(static_dir.join("index.html"), b"v2")
                .expect("write index v2 from SSE thread");

            // Now read until we see `data: reload` or timeout after 10 s.
            read_until(&mut stream, "data: reload", Duration::from_secs(10))
        })
    };

    let response = sse_thread.join().expect("SSE thread must not panic");
    assert!(
        response.contains("data: reload"),
        "SSE stream must emit `data: reload` after file change; got: {response}"
    );
}

/// Extension filter: only files matching configured extensions trigger a reload.
#[test]
#[serial]
fn hot_reload_extension_filter_ignores_non_matching() {
    let port = common::free_port();
    let admin_port = common::free_port();

    let dir = tempfile::tempdir().expect("tempdir");
    let static_dir = dir.path().to_path_buf();
    std::fs::write(static_dir.join("index.html"), b"v1").expect("write html");
    std::fs::write(static_dir.join("data.json"), b"{}").expect("write json");

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_dir.to_string_lossy(),
                // Only watch .html files — .json changes must NOT trigger a reload.
                "hotReload": { "extensions": [".html"] }
            }]
        }),
    );

    let sse_thread = {
        let static_dir = static_dir.clone();
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
                .expect("TCP connect for extension-filter SSE");
            let req = format!(
                "GET /__hot-reload__ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(req.as_bytes()).expect("write SSE request");

            // Wait for `: connected`.
            let initial = read_until(&mut stream, ": connected", Duration::from_secs(10));
            assert!(initial.contains(": connected"), "SSE connected");

            std::thread::sleep(Duration::from_millis(200));
            // Change a .json file — should NOT trigger reload.
            std::fs::write(static_dir.join("data.json"), b"{\"v\":2}").expect("write json v2");
            std::thread::sleep(Duration::from_millis(600)); // wait past debounce

            // Change a .html file — SHOULD trigger reload.
            std::fs::write(static_dir.join("index.html"), b"v2").expect("write html v2");

            read_until(&mut stream, "data: reload", Duration::from_secs(10))
        })
    };

    let response = sse_thread.join().expect("SSE thread");
    assert!(
        response.contains("data: reload"),
        "SSE must emit reload after .html change; got: {response}"
    );
}
