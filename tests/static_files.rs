mod common;

use serial_test::serial;
use std::fs;

fn make_static_server(files: &[(&str, &str)]) -> (common::TestServer, tempfile::TempDir) {
    let static_dir = tempfile::tempdir().expect("tempdir");
    for (name, content) in files {
        fs::write(static_dir.path().join(name), content).expect("write file");
    }

    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = static_dir.path().to_str().expect("utf8 path").to_owned();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "static": static_path }]
        }),
    );
    (server, static_dir)
}

#[test]
#[serial]
fn static_200_serves_file() {
    let (server, _dir) = make_static_server(&[("hello.txt", "Hello, world!")]);
    let resp = reqwest::blocking::get(server.url("/hello.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "Hello, world!");
}

#[test]
#[serial]
fn static_content_type_html() {
    let (server, _dir) = make_static_server(&[("index.html", "<h1>hi</h1>")]);
    let resp = reqwest::blocking::get(server.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.starts_with("text/html"), "unexpected content-type: {ct}");
}

#[test]
#[serial]
fn static_index_html_at_root() {
    let (server, _dir) = make_static_server(&[("index.html", "root index")]);
    let resp = reqwest::blocking::get(server.url("/")).expect("GET /");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "root index");
}

#[test]
#[serial]
fn static_404_for_missing_file() {
    let (server, _dir) = make_static_server(&[("exists.txt", "yes")]);
    let resp = reqwest::blocking::get(server.url("/nope.txt")).expect("GET");
    assert_eq!(resp.status(), 404);
}

#[test]
#[serial]
fn static_304_on_matching_etag() {
    let (server, _dir) = make_static_server(&[("data.txt", "content")]);

    // First request to get ETag
    let first = reqwest::blocking::get(server.url("/data.txt")).expect("GET");
    assert_eq!(first.status(), 200);
    let etag = first
        .headers()
        .get("etag")
        .expect("etag header")
        .to_str()
        .unwrap()
        .to_owned();

    // Second request with If-None-Match
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/data.txt"))
        .header("if-none-match", &etag)
        .send()
        .expect("GET with If-None-Match");
    assert_eq!(resp.status(), 304);
}

#[test]
#[serial]
fn static_200_on_mismatched_etag() {
    let (server, _dir) = make_static_server(&[("data.txt", "content")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/data.txt"))
        .header("if-none-match", "\"wrong-etag\"")
        .send()
        .expect("GET");
    assert_eq!(resp.status(), 200);
}

#[test]
#[serial]
fn static_206_range_request() {
    let (server, _dir) = make_static_server(&[("range.txt", "0123456789")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/range.txt"))
        .header("range", "bytes=2-5")
        .send()
        .expect("GET with Range");
    assert_eq!(resp.status(), 206);
    let content_range = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(resp.text().unwrap(), "2345");
    assert_eq!(
        content_range, "bytes 2-5/10",
        "Content-Range header mismatch"
    );
}

#[test]
#[serial]
fn static_206_range_suffix() {
    let (server, _dir) = make_static_server(&[("range.txt", "0123456789")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/range.txt"))
        .header("range", "bytes=-3")
        .send()
        .expect("GET suffix range");
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.text().unwrap(), "789");
}

#[test]
#[serial]
fn static_416_invalid_range() {
    let (server, _dir) = make_static_server(&[("small.txt", "hi")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/small.txt"))
        .header("range", "bytes=100-200")
        .send()
        .expect("GET bad range");
    assert_eq!(resp.status(), 416);
}

#[test]
#[serial]
fn static_404_for_dotfile_default() {
    let (server, _dir) = make_static_server(&[(".hidden", "secret")]);
    let resp = reqwest::blocking::get(server.url("/.hidden")).expect("GET");
    assert_eq!(resp.status(), 404);
}

#[test]
#[serial]
fn static_head_returns_no_body() {
    let (server, _dir) = make_static_server(&[("file.txt", "body content")]);
    let client = reqwest::blocking::Client::new();
    let resp = client.head(server.url("/file.txt")).send().expect("HEAD");
    assert_eq!(resp.status(), 200);
    let body = resp.text().unwrap();
    assert!(body.is_empty(), "HEAD should return no body");
}

#[test]
#[serial]
fn static_etag_and_last_modified_present() {
    let (server, _dir) = make_static_server(&[("file.txt", "data")]);
    let resp = reqwest::blocking::get(server.url("/file.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().contains_key("etag"), "missing etag");
    assert!(
        resp.headers().contains_key("last-modified"),
        "missing last-modified"
    );
    assert!(
        resp.headers().contains_key("cache-control"),
        "missing cache-control"
    );
}

#[test]
#[serial]
fn static_cache_control_max_age() {
    let port = common::free_port();
    let admin_port = common::free_port();
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("f.txt"), "x").unwrap();
    let static_path = dir.path().to_str().unwrap().to_owned();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_path,
                "staticOptions": { "maxAge": "1h" }
            }]
        }),
    );

    let resp = reqwest::blocking::get(server.url("/f.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    let cc = resp
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        cc.contains("max-age=3600"),
        "expected max-age=3600, got: {cc}"
    );
}
