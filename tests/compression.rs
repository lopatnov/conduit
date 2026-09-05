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

// ── metrics endpoint (issue #338: compress_bytes() was never wired in) ─────

fn metrics_compression_server(compression_val: serde_json::Value) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "metrics": { "path": "/__metrics__" },
                "compression": compression_val
            }]
        }),
    )
}

#[test]
#[serial]
fn metrics_response_compressed_when_accepted() {
    // minBytes: 0 so this doesn't depend on how large the default Prometheus
    // exposition happens to be for a freshly started server.
    let srv =
        metrics_compression_server(serde_json::json!({ "algorithms": ["gzip"], "minBytes": 0 }));

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/__metrics__"))
        .header("Accept-Encoding", "gzip")
        .send()
        .expect("send");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "metrics response should be gzip-compressed when accepted"
    );
    assert!(
        resp.headers()
            .get("vary")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase()
            .contains("accept-encoding"),
        "Vary must include Accept-Encoding when compression is applied"
    );
    let body = resp.bytes().expect("bytes");
    assert!(
        body.len() >= 2 && body[0] == 0x1f && body[1] == 0x8b,
        "body should start with gzip magic bytes"
    );
}

#[test]
#[serial]
fn metrics_response_respects_default_min_bytes_threshold() {
    // Documents the resolved behavior for issue #338's open question: the
    // default 1024-byte minBytes threshold applies to metrics exactly like
    // everywhere else — no metrics-specific carve-out. Measure the real
    // uncompressed size first (a plain request, no Accept-Encoding) so this
    // doesn't guess at how large a freshly-started server's exposition is.
    let srv = metrics_compression_server(serde_json::json!(true));
    let client = reqwest::blocking::Client::new();

    let plain = client.get(srv.url("/__metrics__")).send().expect("send");
    let uncompressed_len = plain.bytes().expect("bytes").len();

    let resp = client
        .get(srv.url("/__metrics__"))
        .header("Accept-Encoding", "gzip")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 200);
    let compressed = resp.headers().get("content-encoding").is_some();

    assert_eq!(
        compressed,
        uncompressed_len >= 1024,
        "compression should trigger iff the real exposition ({uncompressed_len} bytes) \
         is at/above the default 1024-byte minBytes threshold"
    );
}

// ── fallback body (issue #338) ──────────────────────────────────────────────

fn fallback_compression_server(
    fallback: serde_json::Value,
    compression_val: serde_json::Value,
) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "fallback": fallback,
                "compression": compression_val
            }]
        }),
    )
}

#[test]
#[serial]
fn fallback_body_compressed_when_accepted() {
    let large_body = "x".repeat(2000);
    let srv = fallback_compression_server(
        serde_json::json!({ "status": 404, "body": { "message": large_body } }),
        serde_json::json!({ "algorithms": ["gzip"], "minBytes": 0 }),
    );

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/missing"))
        .header("Accept-Encoding", "gzip")
        .send()
        .expect("send");

    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "fallback JSON body should be gzip-compressed when accepted"
    );
    let body = resp.bytes().expect("bytes");
    assert!(
        body.len() >= 2 && body[0] == 0x1f && body[1] == 0x8b,
        "body should start with gzip magic bytes"
    );
}

#[test]
#[serial]
fn fallback_body_not_compressed_without_accept_encoding() {
    let large_body = "x".repeat(2000);
    let srv = fallback_compression_server(
        serde_json::json!({ "status": 404, "body": { "message": large_body } }),
        serde_json::json!({ "algorithms": ["gzip"], "minBytes": 0 }),
    );

    let resp = reqwest::blocking::Client::new()
        .get(srv.url("/missing"))
        .send()
        .expect("send");

    assert_eq!(resp.status(), 404);
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "no Content-Encoding without a client Accept-Encoding header"
    );
    // Compression is configured for this site, so the representation still
    // varies by Accept-Encoding even though this particular response wasn't
    // compressed — a shared cache must not store this body under a key that
    // ignores it (issue found reviewing PR #347/#348: Vary was previously
    // only emitted when compression was actually applied).
    assert_eq!(
        resp.headers().get("vary").map(|v| v.to_str().unwrap()),
        Some("accept-encoding"),
        "Vary must be present whenever compression is configured, not only when applied"
    );
    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["message"], large_body);
}
