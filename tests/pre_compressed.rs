mod common;

use serial_test::serial;

// Pre-built compressed payloads used across tests.
//
// These are real brotli / gzip streams for "hello world" produced by the
// respective command-line tools.  The server must stream them verbatim
// without re-encoding.

/// Brotli-encoded `"hello world"` (11 bytes → 16 bytes compressed).
const BR_HELLO: &[u8] = &[
    0x0b, 0x05, 0x00, 0x00, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x03,
];

/// Gzip-encoded `"hello world"` — a minimal valid gzip stream.
///
/// Header: magic (1f 8b), deflate (08), flags (00), mtime (0×4), xfl (00),
/// OS (ff=unknown).  Followed by deflate payload + CRC32 + size.
const GZ_HELLO: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, // gzip header
    0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01, 0x00, // deflate
    0x56, 0xb1, 0x17, 0x4a, // CRC32
    0x0b, 0x00, 0x00, 0x00, // size (11)
];

/// Create a test server with `staticOptions.preCompressed: true`.
fn make_server_with_precompressed(
    files: &[(&str, &[u8])],
) -> (common::TestServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, content) in files {
        std::fs::write(dir.path().join(name), content).expect("write file");
    }

    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = dir.path().to_str().expect("utf8").to_owned();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_path,
                "staticOptions": { "preCompressed": true }
            }]
        }),
    );
    (server, dir)
}

// ── Brotli ────────────────────────────────────────────────────────────────────

/// A client that accepts `br` receives the `.br` sibling with the correct
/// `Content-Encoding` and the original resource's `Content-Type`.
#[test]
#[serial]
fn pre_compressed_br_served_when_accepted() {
    let (srv, _dir) = make_server_with_precompressed(&[
        ("index.html", b"<html>hello world</html>"),
        ("index.html.br", BR_HELLO),
    ]);

    let client = reqwest::blocking::Client::builder()
        .no_gzip() // disable reqwest's auto-decompression
        .build()
        .expect("client");
    let resp = client
        .get(srv.url("/index.html"))
        .header("accept-encoding", "br, gzip")
        .send()
        .expect("GET index.html");

    assert_eq!(resp.status().as_u16(), 200);

    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("html"),
        "Content-Type must reflect the original file, not .br; got: {ct}"
    );

    let ce = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ce, "br", "Content-Encoding must be br; got: {ce}");

    let vary = resp
        .headers()
        .get("vary")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        vary.to_ascii_lowercase().contains("accept-encoding"),
        "Vary must include accept-encoding; got: {vary}"
    );

    // The raw bytes must be the verbatim pre-compressed file contents.
    let body = resp.bytes().expect("body bytes");
    assert_eq!(
        body.as_ref(),
        BR_HELLO,
        "body must be the verbatim .br bytes, not re-encoded"
    );
}

// ── Gzip ──────────────────────────────────────────────────────────────────────

/// A client that accepts `gzip` (but not `br`) receives the `.gz` sibling.
#[test]
#[serial]
fn pre_compressed_gz_served_when_br_not_accepted() {
    let (srv, _dir) = make_server_with_precompressed(&[
        ("style.css", b"body { color: red; }"),
        ("style.css.gz", GZ_HELLO),
    ]);

    let client = reqwest::blocking::Client::builder()
        .no_gzip()
        .build()
        .expect("client");
    let resp = client
        .get(srv.url("/style.css"))
        .header("accept-encoding", "gzip") // no br
        .send()
        .expect("GET style.css");

    assert_eq!(resp.status().as_u16(), 200);

    let ce = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ce, "gzip", "Content-Encoding must be gzip; got: {ce}");

    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("css"),
        "Content-Type must be css, not gz; got: {ct}"
    );

    let body = resp.bytes().expect("body bytes");
    assert_eq!(body.as_ref(), GZ_HELLO, "body must be verbatim .gz bytes");
}

// ── Fallback when no sibling exists ──────────────────────────────────────────

/// When no pre-compressed sibling is found the server falls back to the
/// original uncompressed file (no `Content-Encoding` header).
#[test]
#[serial]
fn pre_compressed_falls_back_to_original_when_no_sibling() {
    let (srv, _dir) = make_server_with_precompressed(&[
        ("data.txt", b"raw content"),
        // No data.txt.br or data.txt.gz
    ]);

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(srv.url("/data.txt"))
        .header("accept-encoding", "br, gzip")
        .send()
        .expect("GET data.txt");

    assert_eq!(resp.status().as_u16(), 200);
    // No Content-Encoding — served as plain text.
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "no Content-Encoding when no pre-compressed sibling exists"
    );
    assert_eq!(resp.text().expect("body"), "raw content");
}

// ── Disabled ──────────────────────────────────────────────────────────────────

/// When `preCompressed` is not enabled, the `.gz` sibling is ignored and the
/// original file is served.
#[test]
#[serial]
fn pre_compressed_not_used_when_disabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("page.html"), b"original content").expect("write html");
    std::fs::write(dir.path().join("page.html.gz"), GZ_HELLO).expect("write gz");

    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = dir.path().to_str().expect("utf8").to_owned();

    // No preCompressed option — server must ignore the .gz sibling.
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "static": static_path }]
        }),
    );

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(srv.url("/page.html"))
        .header("accept-encoding", "gzip")
        .send()
        .expect("GET page.html");

    assert_eq!(resp.status().as_u16(), 200);
    // reqwest decodes gzip automatically; if no Content-Encoding, we get raw bytes.
    // Either way the body must be the plain HTML.
    let body = resp.text().expect("body");
    assert!(
        body.contains("original content"),
        "must serve original file when preCompressed is disabled; got: {body}"
    );
}

// ── Brotli preferred over gzip ────────────────────────────────────────────────

/// When both `.br` and `.gz` siblings exist and the client accepts both,
/// the `.br` sibling takes priority.
#[test]
#[serial]
fn pre_compressed_br_preferred_over_gz() {
    let (srv, _dir) = make_server_with_precompressed(&[
        ("app.js", b"console.log('hi')"),
        ("app.js.br", BR_HELLO),
        ("app.js.gz", GZ_HELLO),
    ]);

    let client = reqwest::blocking::Client::builder()
        .no_gzip()
        .build()
        .expect("client");
    let resp = client
        .get(srv.url("/app.js"))
        .header("accept-encoding", "br, gzip")
        .send()
        .expect("GET app.js");

    let ce = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        ce, "br",
        "br must be preferred over gzip when both exist; got: {ce}"
    );

    // Body must be the br bytes, not gz.
    let body = resp.bytes().expect("body bytes");
    assert_eq!(body.as_ref(), BR_HELLO);
}

// ── No br available, gz accepted ──────────────────────────────────────────────

/// When only a `.gz` sibling exists but the client accepts both br and gzip,
/// the server falls back to `.gz` (rather than serving uncompressed).
#[test]
#[serial]
fn pre_compressed_falls_back_to_gz_when_no_br() {
    let (srv, _dir) = make_server_with_precompressed(&[
        ("bundle.js", b"console.log('bundle')"),
        // only .gz, no .br
        ("bundle.js.gz", GZ_HELLO),
    ]);

    let client = reqwest::blocking::Client::builder()
        .no_gzip()
        .build()
        .expect("client");
    let resp = client
        .get(srv.url("/bundle.js"))
        .header("accept-encoding", "br, gzip")
        .send()
        .expect("GET bundle.js");

    let ce = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        ce, "gzip",
        "must fall back to .gz when .br sibling is absent; got: {ce}"
    );
}

// ── HEAD request ──────────────────────────────────────────────────────────────

/// HEAD requests work correctly: headers include Content-Encoding but no body
/// is sent.
#[test]
#[serial]
fn pre_compressed_head_returns_headers_only() {
    let (srv, _dir) =
        make_server_with_precompressed(&[("file.css", b"body{}"), ("file.css.br", BR_HELLO)]);

    let client = reqwest::blocking::Client::builder()
        .no_gzip()
        .build()
        .expect("client");
    let resp = client
        .head(srv.url("/file.css"))
        .header("accept-encoding", "br")
        .send()
        .expect("HEAD file.css");

    assert_eq!(resp.status().as_u16(), 200);
    let ce = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ce, "br");
    // HEAD must return 0-length body.
    let body = resp.bytes().expect("body");
    assert!(body.is_empty(), "HEAD response must have empty body");
}
