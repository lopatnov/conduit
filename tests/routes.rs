mod common;

use serial_test::serial;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Start a conduit server with `routes` configured.
///
/// `routes_json` is a serde_json `Value` that becomes the site's `routes` array.
fn make_routes_server(routes_json: serde_json::Value) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": routes_json
            }]
        }),
    )
}

/// Start a conduit server with `routes` + an upstream echo server.
///
/// Returns `(TestServer, upstream_port)`.
fn make_routes_server_with_upstream(routes_json: serde_json::Value) -> (common::TestServer, u16) {
    let upstream_port = common::free_port();
    let server_port = common::free_port();
    let admin_port = common::free_port();

    // Spawn a simple upstream that echoes the path and method.
    let listener =
        std::net::TcpListener::bind(format!("127.0.0.1:{upstream_port}")).expect("bind upstream");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let first_line = req.lines().next().unwrap_or("");
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");
            let method = first_line.split_whitespace().next().unwrap_or("GET");
            let body = format!("{{\"path\":\"{path}\",\"method\":\"{method}\"}}");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut s, resp.as_bytes()).ok();
        }
    });

    let srv = common::TestServer::start_with_config(
        server_port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": server_port,
                "routes": routes_json
            }]
        }),
    );
    (srv, upstream_port)
}

// ── Path glob routing ─────────────────────────────────────────────────────────

/// A route with `match.path` glob routes to the correct upstream.
#[test]
#[serial]
fn routes_path_glob_matches() {
    let upstream_port = common::free_port();
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{upstream_port}")).expect("bind");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            std::io::Read::read(&mut s, &mut buf).ok();
            let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nOK";
            std::io::Write::write_all(&mut s, resp.as_bytes()).ok();
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [
                    {
                        "match": { "path": "/api/**" },
                        "proxy": format!("http://127.0.0.1:{upstream_port}")
                    }
                ]
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/api/v1/users")).expect("GET /api/v1/users");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "/api/v1/users should be proxied"
    );

    // Path outside the glob should not match → fallback 404.
    let resp2 = reqwest::blocking::get(srv.url("/other/path")).expect("GET /other");
    assert_eq!(
        resp2.status().as_u16(),
        404,
        "/other should fall through to fallback"
    );
}

/// First matching route wins — ordering is respected.
#[test]
#[serial]
fn routes_first_match_wins() {
    let port_a = common::free_port();
    let port_b = common::free_port();

    // Upstream A: returns 200 with body "A"
    let listener_a = std::net::TcpListener::bind(format!("127.0.0.1:{port_a}")).expect("bind A");
    std::thread::spawn(move || {
        for stream in listener_a.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 512];
            std::io::Read::read(&mut s, &mut buf).ok();
            std::io::Write::write_all(&mut s, b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nA")
                .ok();
        }
    });

    // Upstream B: returns 200 with body "B"
    let listener_b = std::net::TcpListener::bind(format!("127.0.0.1:{port_b}")).expect("bind B");
    std::thread::spawn(move || {
        for stream in listener_b.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 512];
            std::io::Read::read(&mut s, &mut buf).ok();
            std::io::Write::write_all(&mut s, b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nB")
                .ok();
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [
                    // Route A matches first — B's more specific path never wins
                    // if A's glob covers it too.
                    {
                        "match": { "path": "/**" },
                        "proxy": format!("http://127.0.0.1:{port_a}")
                    },
                    {
                        "match": { "path": "/api/**" },
                        "proxy": format!("http://127.0.0.1:{port_b}")
                    }
                ]
            }]
        }),
    );

    let body = reqwest::blocking::get(srv.url("/api/data"))
        .expect("GET")
        .text()
        .expect("text");
    // The first route (/**) should win, routing to upstream A.
    assert_eq!(body.trim(), "A", "first route must win: got {body:?}");
}

// ── Method filter ─────────────────────────────────────────────────────────────

/// A route with `match.method` only handles matching HTTP methods.
#[test]
#[serial]
fn routes_method_filter() {
    let upstream_port = common::free_port();
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{upstream_port}")).expect("bind");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 512];
            std::io::Read::read(&mut s, &mut buf).ok();
            std::io::Write::write_all(&mut s, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .ok();
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [{
                    "match": {
                        "path": "/submit",
                        "method": ["POST", "PUT"]
                    },
                    "proxy": format!("http://127.0.0.1:{upstream_port}")
                }]
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();

    // POST and PUT should match.
    let r_post = client.post(srv.url("/submit")).send().expect("POST");
    assert_eq!(r_post.status().as_u16(), 200, "POST should match");

    let r_put = client.put(srv.url("/submit")).send().expect("PUT");
    assert_eq!(r_put.status().as_u16(), 200, "PUT should match");

    // GET should NOT match → fallback 404.
    let r_get = client.get(srv.url("/submit")).send().expect("GET");
    assert_eq!(r_get.status().as_u16(), 404, "GET should not match");
}

// ── Header predicate ──────────────────────────────────────────────────────────

/// A route with `match.headers` only applies when the header regex matches.
#[test]
#[serial]
fn routes_header_predicate() {
    let upstream_port = common::free_port();
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{upstream_port}")).expect("bind");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 512];
            std::io::Read::read(&mut s, &mut buf).ok();
            std::io::Write::write_all(
                &mut s,
                b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nmatched!",
            )
            .ok();
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [{
                    "match": {
                        "path": "/guarded",
                        "headers": { "x-api-version": "v2" }
                    },
                    "proxy": format!("http://127.0.0.1:{upstream_port}")
                }]
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();

    // Correct header value → proxied.
    let r_match = client
        .get(srv.url("/guarded"))
        .header("x-api-version", "v2")
        .send()
        .expect("with header");
    assert_eq!(
        r_match.status().as_u16(),
        200,
        "matching header should proxy"
    );

    // Wrong value → no match → fallback 404.
    let r_no_match = client
        .get(srv.url("/guarded"))
        .header("x-api-version", "v1")
        .send()
        .expect("wrong header");
    assert_eq!(
        r_no_match.status().as_u16(),
        404,
        "wrong header value should not match"
    );

    // Missing header → no match → fallback 404.
    let r_missing = client.get(srv.url("/guarded")).send().expect("no header");
    assert_eq!(
        r_missing.status().as_u16(),
        404,
        "missing header should not match"
    );
}

// ── Query predicate ───────────────────────────────────────────────────────────

/// A route with `match.query` only applies when the query param regex matches.
#[test]
#[serial]
fn routes_query_predicate() {
    let upstream_port = common::free_port();
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{upstream_port}")).expect("bind");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 512];
            std::io::Read::read(&mut s, &mut buf).ok();
            std::io::Write::write_all(&mut s, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .ok();
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [{
                    "match": {
                        "path": "/search",
                        "query": { "version": "2" }
                    },
                    "proxy": format!("http://127.0.0.1:{upstream_port}")
                }]
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();

    // Matching query → proxied.
    let r_ok = client
        .get(srv.url("/search?version=2&q=foo"))
        .send()
        .expect("with query");
    assert_eq!(r_ok.status().as_u16(), 200, "matching query should proxy");

    // Wrong value → fallback.
    let r_bad = client
        .get(srv.url("/search?version=1"))
        .send()
        .expect("wrong query");
    assert_eq!(
        r_bad.status().as_u16(),
        404,
        "wrong query value should not match"
    );

    // Missing param → fallback.
    let r_missing = client.get(srv.url("/search")).send().expect("no query");
    assert_eq!(
        r_missing.status().as_u16(),
        404,
        "missing query param should not match"
    );
}

// ── Static action in routes ───────────────────────────────────────────────────

/// A route can serve static files instead of proxying.
///
/// When a route's `static` action points at a root dir and the request path is
/// `/files/hello.txt`, the handler looks for `<root>/files/hello.txt`.
#[test]
#[serial]
fn routes_static_action() {
    let dir = tempfile::tempdir().expect("tempdir");
    // File must live at <root>/files/hello.txt to be found when requesting /files/hello.txt.
    std::fs::create_dir(dir.path().join("files")).expect("mkdir files");
    std::fs::write(
        dir.path().join("files").join("hello.txt"),
        b"hello from routes static",
    )
    .expect("write");

    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = dir.path().to_str().expect("utf8").to_owned();

    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [{
                    "match": { "path": "/files/**" },
                    "static": static_path
                }]
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/files/hello.txt")).expect("GET");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.text().expect("body"), "hello from routes static");
}

// ── No-match fallthrough ──────────────────────────────────────────────────────

/// When no route matches, the request falls through to the default fallback (404).
#[test]
#[serial]
fn routes_no_match_returns_404() {
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [{
                    "match": { "path": "/specific" },
                    "proxy": "http://127.0.0.1:19999"  // unreachable, but match is all we check
                }]
            }]
        }),
    );

    let resp = reqwest::blocking::get(srv.url("/other")).expect("GET /other");
    assert_eq!(
        resp.status().as_u16(),
        404,
        "/other should be 404 when no route matches"
    );
}

// ── No-match criterion falls through ─────────────────────────────────────────

/// A route with no match criteria matches every request.
#[test]
#[serial]
fn routes_empty_match_catches_all() {
    let upstream_port = common::free_port();
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{upstream_port}")).expect("bind");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 512];
            std::io::Read::read(&mut s, &mut buf).ok();
            std::io::Write::write_all(&mut s, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .ok();
        }
    });

    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "routes": [{
                    "match": {},
                    "proxy": format!("http://127.0.0.1:{upstream_port}")
                }]
            }]
        }),
    );

    // Any path should be routed.
    let r1 = reqwest::blocking::get(srv.url("/anything")).expect("GET");
    assert_eq!(r1.status().as_u16(), 200, "empty match should catch all");

    let r2 = reqwest::blocking::get(srv.url("/foo/bar/baz")).expect("GET deep");
    assert_eq!(
        r2.status().as_u16(),
        200,
        "empty match should catch deep paths"
    );
}
