mod common;

use common::{TestServer, free_port};

// ── helpers ───────────────────────────────────────────────────────────────────

fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

/// Start a conduit server with an upload endpoint configured.
///
/// Returns `(server, upload_dir_path)`.  The upload directory is created
/// inside the server's temp directory and removed when `TestServer` drops.
fn start_upload_server() -> (TestServer, String) {
    let port = free_port();
    let admin_port = free_port();

    // The upload dir path must be absolute so conduit finds it regardless of
    // the working directory.  We use a sub-path inside the temp dir that the
    // test helper creates.
    let dir = tempfile::tempdir().expect("tempdir");
    let upload_dir = dir.path().join("uploads");
    std::fs::create_dir_all(&upload_dir).expect("create upload dir");
    let upload_dir_str = upload_dir.to_string_lossy().into_owned();

    let config = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "healthCheck": true,
            "upload": {
                "path": "/upload",
                "dir": upload_dir_str,
                "maxFileSizeBytes": 1048576,
                "maxTotalSizeBytes": 2097152,
                "maxFiles": 3,
                "allowedMimeTypes": ["image/jpeg", "image/png", "text/plain"]
            }
        }]
    });

    let server = TestServer::start_with_config(port, admin_port, config);

    // Keep the TempDir alive by leaking it — the OS will clean it up on process exit.
    // (This is acceptable in tests; the server's own _dir handles cleanup.)
    std::mem::forget(dir);

    (server, upload_dir_str)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn upload_single_file_returns_ok() {
    let (server, upload_dir) = start_upload_server();
    let client = build_client();

    let form = reqwest::blocking::multipart::Form::new().part(
        "file",
        reqwest::blocking::multipart::Part::bytes(b"hello world".to_vec())
            .file_name("hello.txt")
            .mime_str("text/plain")
            .expect("mime"),
    );

    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(
        resp.status(),
        200,
        "upload should succeed with 200, got {:?}",
        resp.status()
    );

    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["status"], "ok", "body: {body}");
    let files = body["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["originalName"], "hello.txt");
    assert_eq!(files[0]["mimeType"], "text/plain");
    assert_eq!(files[0]["size"], 11);

    // Verify the file actually landed on disk.
    let saved_name = files[0]["name"].as_str().expect("name field");
    let saved_path = std::path::Path::new(&upload_dir).join(saved_name);
    assert!(saved_path.exists(), "saved file should exist at {saved_path:?}");
    let contents = std::fs::read_to_string(&saved_path).expect("read saved file");
    assert_eq!(contents, "hello world");
}

#[test]
fn upload_multiple_files_returns_all_entries() {
    let (server, _dir) = start_upload_server();
    let client = build_client();

    let form = reqwest::blocking::multipart::Form::new()
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(b"file one".to_vec())
                .file_name("a.txt")
                .mime_str("text/plain")
                .expect("mime"),
        )
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(b"file two".to_vec())
                .file_name("b.txt")
                .mime_str("text/plain")
                .expect("mime"),
        );

    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["files"].as_array().expect("array").len(), 2);
}

#[test]
fn upload_disallowed_mime_type_returns_415() {
    let (server, _dir) = start_upload_server();
    let client = build_client();

    let form = reqwest::blocking::multipart::Form::new().part(
        "file",
        reqwest::blocking::multipart::Part::bytes(b"<script>alert(1)</script>".to_vec())
            .file_name("evil.html")
            .mime_str("text/html")
            .expect("mime"),
    );

    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(resp.status(), 415, "HTML should be rejected as disallowed MIME");
}

#[test]
fn upload_no_files_returns_400() {
    let (server, _dir) = start_upload_server();
    let client = build_client();

    // Send a multipart body but with a different field name than "file".
    let form = reqwest::blocking::multipart::Form::new().part(
        "data",
        reqwest::blocking::multipart::Part::bytes(b"ignored".to_vec())
            .file_name("ignored.txt")
            .mime_str("text/plain")
            .expect("mime"),
    );

    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(resp.status(), 400, "wrong field name → no files → 400");
}

#[test]
fn upload_file_exceeds_per_file_limit_returns_413() {
    let port = free_port();
    let admin_port = free_port();

    let dir = tempfile::tempdir().expect("tempdir");
    let upload_dir = dir.path().join("uploads");
    std::fs::create_dir_all(&upload_dir).expect("create upload dir");
    let upload_dir_str = upload_dir.to_string_lossy().into_owned();

    let config = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "healthCheck": true,
            "upload": {
                "path": "/upload",
                "dir": upload_dir_str,
                "maxFileSizeBytes": 10
            }
        }]
    });

    let server = TestServer::start_with_config(port, admin_port, config);
    std::mem::forget(dir);

    let client = build_client();
    let big_data = vec![b'x'; 100]; // 100 bytes > 10-byte limit
    let form = reqwest::blocking::multipart::Form::new().part(
        "file",
        reqwest::blocking::multipart::Part::bytes(big_data)
            .file_name("big.txt")
            .mime_str("text/plain")
            .expect("mime"),
    );

    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(resp.status(), 413, "file over maxFileSizeBytes → 413");
}

#[test]
fn upload_exceeds_max_files_returns_400() {
    let port = free_port();
    let admin_port = free_port();

    let dir = tempfile::tempdir().expect("tempdir");
    let upload_dir = dir.path().join("uploads");
    std::fs::create_dir_all(&upload_dir).expect("create upload dir");
    let upload_dir_str = upload_dir.to_string_lossy().into_owned();

    let config = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "port": port,
            "healthCheck": true,
            "upload": {
                "path": "/upload",
                "dir": upload_dir_str,
                "maxFiles": 2
            }
        }]
    });

    let server = TestServer::start_with_config(port, admin_port, config);
    std::mem::forget(dir);

    let client = build_client();

    // Send 3 files when maxFiles = 2.
    let form = reqwest::blocking::multipart::Form::new()
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(b"a".to_vec())
                .file_name("a.txt")
                .mime_str("text/plain")
                .expect("mime"),
        )
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(b"b".to_vec())
                .file_name("b.txt")
                .mime_str("text/plain")
                .expect("mime"),
        )
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(b"c".to_vec())
                .file_name("c.txt")
                .mime_str("text/plain")
                .expect("mime"),
        );

    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(resp.status(), 400, "3 files when maxFiles=2 → 400");
}

#[test]
fn upload_path_not_reachable_when_upload_not_configured() {
    // A server without upload config should return 404 for the /upload path.
    let server = TestServer::start_minimal();
    let client = build_client();

    let form = reqwest::blocking::multipart::Form::new().part(
        "file",
        reqwest::blocking::multipart::Part::bytes(b"hello".to_vec())
            .file_name("hello.txt")
            .mime_str("text/plain")
            .expect("mime"),
    );

    // The server has no upload config → the path falls through to the fallback
    // handler which returns 404.
    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(
        resp.status(),
        404,
        "no upload config → 404, got {:?}",
        resp.status()
    );
}

#[test]
fn upload_uuid_filename_has_correct_extension() {
    let (server, upload_dir) = start_upload_server();
    let client = build_client();

    let form = reqwest::blocking::multipart::Form::new().part(
        "file",
        reqwest::blocking::multipart::Part::bytes(vec![0xFFu8, 0xD8, 0xFF])
            .file_name("photo.jpg")
            .mime_str("image/jpeg")
            .expect("mime"),
    );

    let resp = client
        .post(server.url("/upload"))
        .multipart(form)
        .send()
        .expect("POST /upload");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON body");
    let saved_name = body["files"][0]["name"].as_str().expect("name field");

    assert!(
        saved_name.ends_with(".jpg"),
        "saved name should preserve .jpg extension, got: {saved_name}"
    );

    // UUID part (before the extension) should be 36 characters.
    let stem = saved_name.strip_suffix(".jpg").unwrap();
    assert_eq!(
        stem.len(),
        36,
        "UUID stem should be 36 chars, got: {stem}"
    );

    let saved_path = std::path::Path::new(&upload_dir).join(saved_name);
    assert!(saved_path.exists());
}
