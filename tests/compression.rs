mod common;

use serial_test::serial;

/// Write a file large enough (>1024 bytes default min_bytes) to trigger compression.
fn make_static_dir_with_large_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let content = "Hello, compressible world! ".repeat(80); // ~2.1 KB
    std::fs::write(dir.path().join("data.txt"), &content).expect("write");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

fn compression_server(
    static_dir: &str,
    compression_val: serde_json::Value,
) -> (common::TestServer, u16, u16) {
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "compression": compression_val,
                "static": static_dir
            }]
        }),
    );
    (srv, port, admin_port)
}

// ── gzip ──────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn gzip_returned_when_accepted() {
    let (_dir, static_dir) = make_static_dir_with_large_file();
    let (srv, _, _) = compression_server(&static_dir, serde_json::json!(true));

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/data.txt"))
        .header("Accept-Encoding", "gzip")
        .send()
        .expect("send");

    assert_eq!(resp.status(), 200);
    let ce = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ce, "gzip", "expected Content-Encoding: gzip");

    let vary = resp
        .headers()
        .get("vary")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        vary.to_lowercase().contains("accept-encoding"),
        "Vary must include Accept-Encoding, got: '{vary}'"
    );
}

#[test]
#[serial]
fn gzip_body_starts_with_magic_bytes() {
    let (_dir, static_dir) = make_static_dir_with_large_file();
    let (srv, _, _) = compression_server(&static_dir, serde_json::json!(true));

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/data.txt"))
        .header("Accept-Encoding", "gzip")
        .send()
        .expect("send");

    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    // gzip magic: 0x1f 0x8b
    let body = resp.bytes().expect("bytes");
    assert!(
        body.len() >= 2 && body[0] == 0x1f && body[1] == 0x8b,
        "body should start with gzip magic bytes 1f 8b, got {:02x} {:02x}",
        body.first().copied().unwrap_or(0),
        body.get(1).copied().unwrap_or(0)
    );
}

// ── brotli ────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn brotli_preferred_when_both_accepted() {
    let (_dir, static_dir) = make_static_dir_with_large_file();
    let (srv, _, _) = compression_server(&static_dir, serde_json::json!(true));

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/data.txt"))
        .header("Accept-Encoding", "br, gzip")
        .send()
        .expect("send");

    assert_eq!(resp.status(), 200);
    let ce = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ce, "br", "brotli should be preferred over gzip");
}

// ── no compression ────────────────────────────────────────────────────────

#[test]
#[serial]
fn no_compression_without_accept_encoding() {
    let (_dir, static_dir) = make_static_dir_with_large_file();
    let (srv, _, _) = compression_server(&static_dir, serde_json::json!(true));

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/data.txt"))
        .send()
        .expect("send");

    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "no Content-Encoding without Accept-Encoding"
    );
}

#[test]
#[serial]
fn no_compression_when_disabled() {
    let (_dir, static_dir) = make_static_dir_with_large_file();
    let (srv, _, _) = compression_server(&static_dir, serde_json::json!(false));

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/data.txt"))
        .header("Accept-Encoding", "gzip, br")
        .send()
        .expect("send");

    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "no compression when compression: false"
    );
}

#[test]
#[serial]
fn no_compression_below_min_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 10 bytes — well below the 1024-byte default min_bytes
    std::fs::write(dir.path().join("tiny.txt"), "1234567890").expect("write");
    let static_dir = dir.path().to_string_lossy().into_owned();
    let (srv, _, _) = compression_server(&static_dir, serde_json::json!(true));

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/tiny.txt"))
        .header("Accept-Encoding", "gzip")
        .send()
        .expect("send");

    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "files smaller than min_bytes must not be compressed"
    );
}

// ── custom options ────────────────────────────────────────────────────────

#[test]
#[serial]
fn compression_options_form_works() {
    let (_dir, static_dir) = make_static_dir_with_large_file();
    let (srv, _, _) = compression_server(
        &static_dir,
        serde_json::json!({ "algorithms": ["gzip"], "level": 1, "minBytes": 0 }),
    );

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/data.txt"))
        .header("Accept-Encoding", "gzip")
        .send()
        .expect("send");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "custom options form should compress with gzip"
    );
}
