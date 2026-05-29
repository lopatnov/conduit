mod common;

use common::{free_port, TestServer};
use serial_test::serial;

fn cors_server(cors: serde_json::Value) -> TestServer {
    let port = free_port();
    let admin_port = free_port();
    TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "cors": cors,
                "proxy": "http://127.0.0.1:1"   // won't be reached for preflights
            }]
        }),
    )
}

// ── Preflight tests ────────────────────────────────────────────────────────

#[test]
#[serial]
fn preflight_cors_true_returns_204() {
    let srv = cors_server(serde_json::json!(true));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/api/data"))
        .header("Origin", "https://app.example.com")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 204);
}

#[test]
#[serial]
fn preflight_sets_allow_origin_wildcard() {
    let srv = cors_server(serde_json::json!(true));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://any.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .expect("send");
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(acao, "*", "wildcard expected for cors: true");
}

#[test]
#[serial]
fn preflight_with_specific_origin_echoes_origin() {
    let srv = cors_server(serde_json::json!({
        "origins": ["https://allowed.com"],
        "methods": ["GET", "POST"]
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://allowed.com")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 204);
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(acao, "https://allowed.com");
    // Must also set Vary: Origin
    let vary = resp
        .headers()
        .get("vary")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        vary.contains("Origin"),
        "Vary: Origin expected, got '{vary}'"
    );
}

#[test]
#[serial]
fn preflight_disallowed_origin_returns_204_no_acao() {
    // When the origin is not in the list, the server returns 204 but without
    // the Access-Control-Allow-Origin header — the browser will block it.
    let srv = cors_server(serde_json::json!({
        "origins": ["https://allowed.com"]
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://evil.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 204);
    // No ACAO header should be present
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "disallowed origin must not receive ACAO header"
    );
}

#[test]
#[serial]
fn preflight_max_age_header_present() {
    let srv = cors_server(serde_json::json!(true));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://a.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .expect("send");
    assert!(
        resp.headers().contains_key("access-control-max-age"),
        "Access-Control-Max-Age header must be present in preflight response"
    );
}

// ── Regular response tests ─────────────────────────────────────────────────

#[test]
#[serial]
fn regular_get_has_acao_header_when_cors_enabled() {
    let port = free_port();
    let admin_port = free_port();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<html/>").expect("write");

    let srv = TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "cors": true,
                "static": dir.path().to_str().unwrap()
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(srv.url("/index.html"))
        .header("Origin", "https://app.example.com")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 200);
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        acao, "*",
        "GET to CORS-enabled static site should carry ACAO: *"
    );
}

#[test]
#[serial]
fn no_cors_header_when_cors_disabled() {
    let port = free_port();
    let admin_port = free_port();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<html/>").expect("write");

    let srv = TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "cors": false,
                "static": dir.path().to_str().unwrap()
            }]
        }),
    );

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(srv.url("/index.html"))
        .header("Origin", "https://app.example.com")
        .send()
        .expect("send");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "cors: false must not emit ACAO header"
    );
}
