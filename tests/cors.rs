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

// ── credentials: true tests ───────────────────────────────────────────────

#[test]
#[serial]
fn credentials_true_sets_acac_header_on_preflight() {
    let srv = cors_server(serde_json::json!({
        "origins": ["https://app.example.com"],
        "credentials": true
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/api"))
        .header("Origin", "https://app.example.com")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .expect("preflight");
    assert_eq!(resp.status(), 204);
    let acac = resp
        .headers()
        .get("access-control-allow-credentials")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        acac, "true",
        "credentials:true must emit Access-Control-Allow-Credentials: true on preflight"
    );
}

#[test]
#[serial]
fn credentials_true_echoes_origin_not_wildcard_on_preflight() {
    // When credentials:true, the server MUST echo the specific origin back,
    // not the wildcard "*" — browsers reject credentialed requests with wildcard.
    let srv = cors_server(serde_json::json!({
        "origins": ["https://trusted.com"],
        "credentials": true
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://trusted.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .expect("preflight");
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        acao, "https://trusted.com",
        "credentials:true must echo the specific origin, not wildcard. Got: '{acao}'"
    );
}

#[test]
#[serial]
fn credentials_true_sets_acac_on_regular_response() {
    // For a regular (non-preflight) request, ACAC must also be present when
    // credentials:true and the origin is in the allow-list.
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
                "cors": {
                    "origins": ["https://app.example.com"],
                    "credentials": true
                },
                "static": dir.path().to_str().unwrap()
            }]
        }),
    );
    Box::leak(Box::new(dir));

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(srv.url("/index.html"))
        .header("Origin", "https://app.example.com")
        .send()
        .expect("GET");
    assert_eq!(resp.status(), 200);
    let acac = resp
        .headers()
        .get("access-control-allow-credentials")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        acac, "true",
        "credentials:true must emit ACAC: true on regular responses too"
    );
    // Origin must be echoed, not wildcard
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        acao, "https://app.example.com",
        "credentials:true must echo origin, got: '{acao}'"
    );
}

/// credentials:true *without* an explicit origins list means "allow any origin,
/// but still echo the actual origin back" — the server must NOT return wildcard
/// because browsers reject credentials with ACAO: *.
#[test]
#[serial]
fn credentials_true_without_origins_list_echoes_origin() {
    // cors: { credentials: true } — no origins field
    let srv = cors_server(serde_json::json!({ "credentials": true }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://any-origin.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .expect("preflight");
    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_ne!(
        acao, "*",
        "credentials:true must never return ACAO: * (browsers reject this)"
    );
    assert_eq!(
        acao, "https://any-origin.com",
        "credentials:true without origins list must echo the request origin, got: '{acao}'"
    );
    let acac = resp
        .headers()
        .get("access-control-allow-credentials")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(acac, "true", "ACAC header must be 'true', got: '{acac}'");
}

/// When credentials:true but the request origin is NOT in the allow-list,
/// neither ACAO nor ACAC must be present — browsers will block the request.
#[test]
#[serial]
fn credentials_true_disallowed_origin_gets_no_acac() {
    let srv = cors_server(serde_json::json!({
        "origins": ["https://allowed.com"],
        "credentials": true
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://evil.com")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .expect("preflight");
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "disallowed origin must not receive ACAO header"
    );
    assert!(
        resp.headers()
            .get("access-control-allow-credentials")
            .is_none(),
        "disallowed origin must not receive ACAC header"
    );
}

/// credentials:false (explicit) must not emit ACAC even when an origins list
/// is configured — the browser must not send cookies for this endpoint.
#[test]
#[serial]
fn credentials_false_does_not_emit_acac() {
    let srv = cors_server(serde_json::json!({
        "origins": ["https://app.example.com"],
        "credentials": false
    }));
    let client = reqwest::blocking::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, srv.url("/"))
        .header("Origin", "https://app.example.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .expect("preflight");
    assert_eq!(resp.status(), 204);
    assert!(
        resp.headers()
            .get("access-control-allow-credentials")
            .is_none(),
        "credentials:false must not emit Access-Control-Allow-Credentials"
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
