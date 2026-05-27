mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serial_test::serial;

// ── Mock upstream that records the request path ───────────────────────────────

struct PathCapture {
    port: u16,
    last_path: Arc<Mutex<String>>,
}

impl PathCapture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let last_path = Arc::new(Mutex::new(String::new()));
        let lp = last_path.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let lp2 = lp.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    // Parse the request-line to extract the path.
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_owned();
                    *lp2.lock().unwrap() = path;
                    let body = "ok";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                });
            }
        });

        PathCapture { port, last_path }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn last_path(&self) -> String {
        std::thread::sleep(std::time::Duration::from_millis(80));
        self.last_path.lock().unwrap().clone()
    }
}

fn rewrite_server(upstream_url: &str, rules: serde_json::Value) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/": {
                        "targets": [upstream_url],
                        "rewrite": rules
                    }
                }
            }]
        }),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn rewrite_simple_prefix_replacement() {
    let upstream = PathCapture::start();
    let server = rewrite_server(
        &upstream.url(),
        serde_json::json!([{ "from": "^/old/(.+)$", "to": "/new/$1" }]),
    );

    let client = reqwest::blocking::Client::new();
    client.get(server.url("/old/page")).send().expect("request");

    assert_eq!(
        upstream.last_path(),
        "/new/page",
        "rewrite must transform /old/page → /new/page"
    );
}

#[test]
#[serial]
fn rewrite_strips_version_prefix() {
    let upstream = PathCapture::start();
    let server = rewrite_server(
        &upstream.url(),
        serde_json::json!([{ "from": "^/v[0-9]+/(.*)$", "to": "/$1" }]),
    );

    let client = reqwest::blocking::Client::new();
    client.get(server.url("/v3/users")).send().expect("request");

    assert_eq!(
        upstream.last_path(),
        "/users",
        "rewrite must strip /v3 prefix"
    );
}

#[test]
#[serial]
fn rewrite_first_rule_wins() {
    let upstream = PathCapture::start();
    // Two rules — only the first matching one should apply.
    let server = rewrite_server(
        &upstream.url(),
        serde_json::json!([
            { "from": "^/a$", "to": "/first" },
            { "from": "^/a$", "to": "/second" }
        ]),
    );

    let client = reqwest::blocking::Client::new();
    client.get(server.url("/a")).send().expect("request");

    assert_eq!(
        upstream.last_path(),
        "/first",
        "first matching rule must win"
    );
}

#[test]
#[serial]
fn no_rewrite_when_no_rules_match() {
    let upstream = PathCapture::start();
    let server = rewrite_server(
        &upstream.url(),
        serde_json::json!([{ "from": "^/specific$", "to": "/other" }]),
    );

    let client = reqwest::blocking::Client::new();
    client
        .get(server.url("/anything-else"))
        .send()
        .expect("request");

    assert_eq!(
        upstream.last_path(),
        "/anything-else",
        "path must be unchanged when no rule matches"
    );
}

#[test]
#[serial]
fn rewrite_and_strip_prefix_coexist() {
    // stripPrefix removes the route prefix, then rewrite further transforms the path.
    let upstream = PathCapture::start();
    let port = common::free_port();
    let admin_port = common::free_port();
    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": {
                    "/api": {
                        "targets": [upstream.url()],
                        "stripPrefix": true,
                        "rewrite": [{ "from": "^/users/(.+)$", "to": "/members/$1" }]
                    }
                }
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();
    client
        .get(server.url("/api/users/42"))
        .send()
        .expect("request");

    // stripPrefix removes "/api" → "/users/42"
    // rewrite transforms "/users/42" → "/members/42"
    assert_eq!(
        upstream.last_path(),
        "/members/42",
        "strip-prefix then rewrite must both apply"
    );
}
