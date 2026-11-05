//! `handle_static` end to end over a real `pingora_proxy::Session` (a `tokio::io::duplex` stream carries the
//! request in and the response out), the way `conduit-core`'s and `conduit-acme`'s handlers are tested.
//!
//! These are unit tests on purpose: `serve`, `stream` and the request flow in `handler.rs` are glue around a
//! live `Session`, which the `cargo llvm-cov --lib` run behind SonarCloud's coverage gate only reaches this
//! way, and they pin the order the open-before-headers change (issue #618) depends on: a miss writes nothing,
//! a refused or failed open is answered before any header, and the length, the validators and the body
//! describe one file.

use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// A parsed response: status, headers, and whatever followed the blank line.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn parse(raw: &[u8]) -> Reply {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no end of headers in {:?}", String::from_utf8_lossy(raw)));
    let head = std::str::from_utf8(&raw[..end]).expect("headers are text");
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .expect("a status line");
    let headers = lines
        .map(|l| {
            let (k, v) = l.split_once(':').expect("a header line");
            (k.trim().to_owned(), v.trim().to_owned())
        })
        .collect();
    Reply {
        status,
        headers,
        body: raw[end + 4..].to_vec(),
    }
}

fn handler(root: &Path, options: StaticOptions) -> StaticFileHandler {
    StaticFileHandler {
        roots: vec![root.to_path_buf()],
        options: Arc::new(options),
        strip_prefix: None,
        extra_headers: Vec::new(),
        #[cfg(feature = "compression")]
        compress_opts: None,
        accept_enc: AcceptEncoding::default(),
        fallback: None,
    }
}

/// Run one request through `handle_static` and return what it answered and every byte it wrote.
async fn run(handler: &StaticFileHandler, request: &str) -> (bool, Vec<u8>) {
    let (server_side, mut client_side) = tokio::io::duplex(256 * 1024);
    client_side.write_all(request.as_bytes()).await.unwrap();
    let stream: pingora_core::protocols::Stream = Box::new(server_side);
    let mut session = Session::new_h1(stream);
    session
        .as_downstream_mut()
        .read_request()
        .await
        .expect("read_request");
    let served = handle_static(&mut session, handler)
        .await
        .expect("handle_static");
    drop(session); // closes the server side, so the read below ends
    let mut written = Vec::new();
    client_side.read_to_end(&mut written).await.unwrap();
    (served, written)
}

fn get(path: &str, extra: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: t\r\n{extra}\r\n")
}

fn dir_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in files {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    dir
}

#[tokio::test]
async fn a_file_is_served_with_its_length_validators_and_body() {
    let dir = dir_with(&[("hello.txt", b"hello")]);
    let (served, raw) = run(
        &handler(dir.path(), StaticOptions::default()),
        &get("/hello.txt", ""),
    )
    .await;
    assert!(served);
    let reply = parse(&raw);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-length"), Some("5"));
    assert!(reply
        .header("content-type")
        .is_some_and(|t| t.starts_with("text/plain")));
    assert_eq!(reply.header("accept-ranges"), Some("bytes"));
    assert!(reply.header("last-modified").is_some());
    // The ETag is built from the size and mtime of the same handle the body came from.
    let etag = reply.header("etag").expect("an etag");
    assert!(etag.ends_with("-5\""), "{etag}");
    assert_eq!(reply.body, b"hello");
}

#[tokio::test]
async fn head_sends_the_headers_and_no_body() {
    let dir = dir_with(&[("hello.txt", b"hello")]);
    let (served, raw) = run(
        &handler(dir.path(), StaticOptions::default()),
        "HEAD /hello.txt HTTP/1.1\r\nHost: t\r\n\r\n",
    )
    .await;
    assert!(served);
    let reply = parse(&raw);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-length"), Some("5"));
    assert!(reply.body.is_empty());
}

#[tokio::test]
async fn a_matching_etag_is_a_304_without_a_body() {
    let dir = dir_with(&[("hello.txt", b"hello")]);
    let h = handler(dir.path(), StaticOptions::default());
    let first = parse(&run(&h, &get("/hello.txt", "")).await.1);
    let etag = first.header("etag").unwrap().to_owned();
    let (served, raw) = run(
        &h,
        &get("/hello.txt", &format!("If-None-Match: {etag}\r\n")),
    )
    .await;
    assert!(served);
    let reply = parse(&raw);
    assert_eq!(reply.status, 304);
    assert_eq!(reply.header("etag"), Some(etag.as_str()));
    assert!(reply.body.is_empty());
}

#[tokio::test]
async fn a_range_is_a_206_and_one_past_the_end_is_clamped_or_a_416() {
    let dir = dir_with(&[("r.txt", b"0123456789")]);
    let h = handler(dir.path(), StaticOptions::default());
    for (range, status, content_range, body) in [
        ("bytes=2-4", 206, Some("bytes 2-4/10"), "234"),
        ("bytes=-3", 206, Some("bytes 7-9/10"), "789"),
        ("bytes=4-999", 206, Some("bytes 4-9/10"), "456789"),
        ("bytes=10-20", 416, Some("bytes */10"), ""),
    ] {
        let (served, raw) = run(&h, &get("/r.txt", &format!("Range: {range}\r\n"))).await;
        assert!(served, "{range}");
        let reply = parse(&raw);
        assert_eq!(reply.status, status, "{range}");
        assert_eq!(reply.header("content-range"), content_range, "{range}");
        assert_eq!(reply.body, body.as_bytes(), "{range}");
    }
    // HEAD of a range: the headers of the 206, no body.
    let (_, raw) = run(
        &h,
        "HEAD /r.txt HTTP/1.1\r\nHost: t\r\nRange: bytes=2-4\r\n\r\n",
    )
    .await;
    let reply = parse(&raw);
    assert_eq!(reply.status, 206);
    assert_eq!(reply.header("content-length"), Some("3"));
    assert!(reply.body.is_empty());
}

#[tokio::test]
async fn a_miss_writes_nothing_so_the_caller_can_run_the_fallback() {
    let dir = dir_with(&[("hello.txt", b"hello")]);
    let h = handler(dir.path(), StaticOptions::default());
    let (served, raw) = run(&h, &get("/missing.txt", "")).await;
    assert!(!served);
    assert!(raw.is_empty(), "wrote {:?}", String::from_utf8_lossy(&raw));
    // A directory with no index file is a miss too.
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let (served, raw) = run(&h, &get("/sub", "")).await;
    assert!(!served && raw.is_empty());
}

#[tokio::test]
async fn the_index_file_answers_for_the_root_and_for_a_directory() {
    let dir = dir_with(&[("index.html", b"<h1>root</h1>")]);
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("index.html"), "sub").unwrap();
    let h = handler(dir.path(), StaticOptions::default());
    for (path, body) in [("/", "<h1>root</h1>"), ("/sub", "sub")] {
        let reply = parse(&run(&h, &get(path, "")).await.1);
        assert_eq!(reply.status, 200, "{path}");
        assert_eq!(reply.body, body.as_bytes(), "{path}");
    }
}

#[tokio::test]
async fn dot_files_are_ignored_denied_or_allowed_by_policy() {
    let dir = dir_with(&[(".secret", b"s")]);
    for (policy, served, status, body) in [
        ("ignore", false, None, ""),
        ("deny", true, Some(403), "Forbidden"),
        ("allow", true, Some(200), "s"),
    ] {
        let options = StaticOptions {
            dot_files: Some(policy.to_owned()),
            ..Default::default()
        };
        let (was_served, raw) = run(&handler(dir.path(), options), &get("/.secret", "")).await;
        assert_eq!(was_served, served, "{policy}");
        match status {
            None => assert!(raw.is_empty(), "{policy}"),
            Some(code) => {
                let reply = parse(&raw);
                assert_eq!(reply.status, code, "{policy}");
                assert_eq!(reply.body, body.as_bytes(), "{policy}");
            }
        }
    }
}

#[tokio::test]
async fn extra_headers_ride_along_and_a_configured_vary_is_merged() {
    let dir = dir_with(&[("app.js", b"plain"), ("app.js.gz", b"GZ")]);
    let mut h = handler(
        dir.path(),
        StaticOptions {
            pre_compressed: Some(true),
            ..Default::default()
        },
    );
    h.extra_headers = vec![
        ("x-served-by".to_owned(), "conduit".to_owned()),
        ("Vary".to_owned(), "Origin".to_owned()),
    ];
    let reply = parse(&run(&h, &get("/app.js", "")).await.1);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("x-served-by"), Some("conduit"));
    let vary = reply.header("vary").expect("a vary header").to_lowercase();
    assert!(
        vary.contains("accept-encoding") && vary.contains("origin"),
        "{vary}"
    );
}

#[tokio::test]
async fn a_pre_compressed_sibling_is_served_with_its_own_length() {
    let dir = dir_with(&[
        ("app.js", b"console.log(1)"),
        ("app.js.gz", b"\x1f\x8bgzbytes"),
    ]);
    let mut h = handler(
        dir.path(),
        StaticOptions {
            pre_compressed: Some(true),
            ..Default::default()
        },
    );
    h.accept_enc = AcceptEncoding {
        gzip: true,
        ..Default::default()
    };
    let (served, raw) = run(&h, &get("/app.js", "")).await;
    assert!(served);
    let reply = parse(&raw);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-encoding"), Some("gzip"));
    assert_eq!(reply.header("accept-ranges"), Some("none"));
    // Content-Length is the sibling's, the ETag describes the original (the representation's validators).
    assert_eq!(reply.header("content-length"), Some("9"));
    assert_eq!(reply.body, b"\x1f\x8bgzbytes");
    assert!(reply.header("etag").is_some_and(|e| e.ends_with("-e\"")));

    // HEAD: the same headers, no body.
    let (_, raw) = run(
        &h,
        "HEAD /app.js HTTP/1.1\r\nHost: t\r\nAccept-Encoding: gzip\r\n\r\n",
    )
    .await;
    let reply = parse(&raw);
    assert_eq!(reply.header("content-length"), Some("9"));
    assert!(reply.body.is_empty());

    // The client that does not accept gzip gets the plain file, and so does one whose sibling is a
    // directory (it cannot be opened as a file).
    let mut plain = handler(
        dir.path(),
        StaticOptions {
            pre_compressed: Some(true),
            ..Default::default()
        },
    );
    let reply = parse(&run(&plain, &get("/app.js", "")).await.1);
    assert_eq!(reply.header("content-encoding"), None);
    assert_eq!(reply.body, b"console.log(1)");
    std::fs::remove_file(dir.path().join("app.js.gz")).unwrap();
    std::fs::create_dir(dir.path().join("app.js.gz")).unwrap();
    plain.accept_enc = AcceptEncoding {
        gzip: true,
        ..Default::default()
    };
    let reply = parse(&run(&plain, &get("/app.js", "")).await.1);
    assert_eq!(reply.header("content-encoding"), None);
    assert_eq!(reply.body, b"console.log(1)");
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn a_compressible_file_is_compressed_on_the_fly_when_the_client_accepts_it() {
    let text = "the quick brown fox ".repeat(200);
    let dir = dir_with(&[("big.txt", text.as_bytes())]);
    let mut h = handler(dir.path(), StaticOptions::default());
    h.compress_opts = Some(conduit_compression::logic::CompressOptions {
        algorithms: vec!["gzip".to_owned(), "deflate".to_owned()],
        level: 6,
        min_bytes: 100,
        types: vec!["*".to_owned()],
    });
    h.accept_enc = AcceptEncoding {
        gzip: true,
        deflate: true,
        ..Default::default()
    };
    let (served, raw) = run(&h, &get("/big.txt", "")).await;
    assert!(served);
    let reply = parse(&raw);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-encoding"), Some("gzip"));
    assert_eq!(reply.header("content-length"), None);
    assert!(reply.header("vary").is_some());
    // The harness session sends the stream as it is (the proxy adds the chunking on a real connection).
    let body = &reply.body;
    assert_eq!(&body[..2], &[0x1f, 0x8b], "not a gzip stream");
    assert!(body.len() < text.len() / 4, "{} bytes", body.len());

    // HEAD announces the encoding and sends no body; a file below `minBytes` is not compressed.
    let (_, raw) = run(
        &h,
        "HEAD /big.txt HTTP/1.1\r\nHost: t\r\nAccept-Encoding: gzip\r\n\r\n",
    )
    .await;
    let reply = parse(&raw);
    assert_eq!(reply.header("content-encoding"), Some("gzip"));
    assert!(reply.body.is_empty());
    std::fs::write(dir.path().join("tiny.txt"), "tiny").unwrap();
    let reply = parse(&run(&h, &get("/tiny.txt", "")).await.1);
    assert_eq!(reply.header("content-encoding"), None);
    assert_eq!(reply.body, b"tiny");
}

/// Issue #618: a file the server cannot read is answered `403` *before any header*, never a `200` whose
/// body is cut off. A mode-000 file is readable by root, so the test skips there.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_file_is_a_complete_403_not_a_cut_off_200() {
    use std::os::unix::fs::PermissionsExt;
    let dir = dir_with(&[("secret.txt", b"classified")]);
    let path = dir.path().join("secret.txt");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&path).is_ok() {
        eprintln!("skipping: this account can read a mode-000 file (root)");
        return;
    }
    let h = handler(dir.path(), StaticOptions::default());
    for request in [
        get("/secret.txt", ""),
        "HEAD /secret.txt HTTP/1.1\r\nHost: t\r\n\r\n".to_owned(),
        get("/secret.txt", "Range: bytes=0-3\r\n"),
    ] {
        let (served, raw) = run(&h, &request).await;
        assert!(served, "{request:?}");
        let reply = parse(&raw);
        assert_eq!(reply.status, 403, "{request:?}");
        assert_eq!(reply.header("content-length"), Some("9"));
        if request.starts_with("GET") {
            assert_eq!(reply.body, b"Forbidden");
        }
    }
}

/// A file another process holds exclusively (a copy or a rename in flight) fails the open with a sharing
/// violation. It is a complete `500` before any header, not a `200` whose body is cut off; it used to be
/// the cut-off. Found by the swap test in `tests/static_files.rs` on `windows-latest`.
#[cfg(windows)]
#[tokio::test]
async fn a_file_held_exclusively_is_a_complete_500_not_a_cut_off_200() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = dir_with(&[("busy.txt", b"contents")]);
    let _writer = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(0) // nobody else may open it while this handle lives
        .open(dir.path().join("busy.txt"))
        .unwrap();
    let h = handler(dir.path(), StaticOptions::default());
    for request in [
        get("/busy.txt", ""),
        "HEAD /busy.txt HTTP/1.1\r\nHost: t\r\n\r\n".to_owned(),
    ] {
        let (served, raw) = run(&h, &request).await;
        assert!(served, "{request:?}");
        let reply = parse(&raw);
        assert_eq!(reply.status, 500, "{request:?}");
        assert_eq!(reply.header("content-length"), Some("21"));
        if request.starts_with("GET") {
            assert_eq!(reply.body, b"Internal Server Error");
        }
    }
}

/// Issue #617: the NTFS 8.3 alias of a hidden file is a miss, with nothing written.
#[cfg(windows)]
#[tokio::test]
async fn a_short_alias_shaped_request_is_a_miss_on_windows() {
    let dir = dir_with(&[("HTPASS~1", b"x"), ("report~1.html", b"ok")]);
    let h = handler(dir.path(), StaticOptions::default());
    let (served, raw) = run(&h, &get("/HTPASS~1", "")).await;
    assert!(!served && raw.is_empty());
    let reply = parse(&run(&h, &get("/report~1.html", "")).await.1);
    assert_eq!(reply.status, 200);
}

/// A route prefix is stripped before the lookup.
#[tokio::test]
async fn the_route_prefix_is_stripped_before_the_lookup() {
    let dir = dir_with(&[("a.txt", b"under the prefix")]);
    let mut h = handler(dir.path(), StaticOptions::default());
    h.strip_prefix = Some("/assets".to_owned());
    let reply = parse(&run(&h, &get("/assets/a.txt", "")).await.1);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"under the prefix");
}

/// The `LocalHandlerImpl` entry: a hit is served, a miss runs the fallback.
#[tokio::test]
async fn the_handler_serves_a_hit_and_hands_a_miss_to_the_fallback() {
    let dir = dir_with(&[("a.txt", b"hit")]);
    let mut h = handler(dir.path(), StaticOptions::default());
    h.fallback = Some(FallbackConfig {
        status: Some(404),
        body: Some(serde_json::json!({"error": "nope"})),
        ..Default::default()
    });
    for (path, status) in [("/a.txt", 200), ("/gone.txt", 404)] {
        let (server_side, mut client_side) = tokio::io::duplex(64 * 1024);
        client_side
            .write_all(get(path, "Accept: application/json\r\n").as_bytes())
            .await
            .unwrap();
        let stream: pingora_core::protocols::Stream = Box::new(server_side);
        let mut session = Session::new_h1(stream);
        session
            .as_downstream_mut()
            .read_request()
            .await
            .expect("read_request");
        h.handle(&mut session).await.expect("handle");
        drop(session);
        let mut raw = Vec::new();
        client_side.read_to_end(&mut raw).await.unwrap();
        assert_eq!(parse(&raw).status, status, "{path}");
    }
}
