mod common;

use base64::Engine as _;
use reqwest::blocking::Client;

// ── helpers ──────────────────────────────────────────────────────────────────

fn plain_client() -> Client {
    Client::new()
}

/// Build the value of an `Authorization: Basic …` header.
fn basic_header(user: &str, pass: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
    format!("Basic {encoded}")
}

// ── Basic Auth tests ──────────────────────────────────────────────────────────

fn server_with_basic_auth() -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "basicAuth": {
                    "users": { "alice": "secret", "bob": "hunter2" },
                    "challenge": true,
                    "realm": "Test Realm",
                    "skipPaths": ["/__health__"]
                }
            }]
        }),
    )
}

#[test]
fn basic_auth_no_credentials_returns_401() {
    let srv = server_with_basic_auth();
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_eq!(resp.status().as_u16(), 401);
}

#[test]
fn basic_auth_www_authenticate_header_present() {
    let srv = server_with_basic_auth();
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_eq!(resp.status().as_u16(), 401);
    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .expect("missing WWW-Authenticate")
        .to_str()
        .expect("non-utf8 header");
    assert!(
        www_auth.contains("Basic"),
        "expected Basic scheme, got: {www_auth}"
    );
    assert!(
        www_auth.contains("Test Realm"),
        "expected realm, got: {www_auth}"
    );
}

#[test]
fn basic_auth_wrong_password_returns_401() {
    let srv = server_with_basic_auth();
    let resp = plain_client()
        .get(srv.url("/"))
        .header("Authorization", basic_header("alice", "wrong"))
        .send()
        .expect("GET /");
    assert_eq!(resp.status().as_u16(), 401);
}

#[test]
fn basic_auth_unknown_user_returns_401() {
    let srv = server_with_basic_auth();
    let resp = plain_client()
        .get(srv.url("/"))
        .header("Authorization", basic_header("nobody", "secret"))
        .send()
        .expect("GET /");
    assert_eq!(resp.status().as_u16(), 401);
}

#[test]
fn basic_auth_correct_credentials_alice() {
    let srv = server_with_basic_auth();
    // No static/proxy configured → 404 fallback, but auth passed (not 401).
    let resp = plain_client()
        .get(srv.url("/"))
        .header("Authorization", basic_header("alice", "secret"))
        .send()
        .expect("GET /");
    assert_ne!(
        resp.status().as_u16(),
        401,
        "alice's credentials should be accepted"
    );
}

#[test]
fn basic_auth_correct_credentials_bob() {
    let srv = server_with_basic_auth();
    let resp = plain_client()
        .get(srv.url("/"))
        .header("Authorization", basic_header("bob", "hunter2"))
        .send()
        .expect("GET /");
    assert_ne!(
        resp.status().as_u16(),
        401,
        "bob's credentials should be accepted"
    );
}

#[test]
fn basic_auth_skip_path_bypasses_auth() {
    let srv = server_with_basic_auth();
    // /__health__ is in skipPaths — health handler also bypasses auth at service level.
    let resp = plain_client()
        .get(srv.url("/__health__"))
        .send()
        .expect("GET /__health__");
    assert_eq!(resp.status().as_u16(), 200);
}

#[test]
fn basic_auth_no_challenge_flag() {
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "basicAuth": {
                    "users": { "u": "p" },
                    "challenge": false
                }
            }]
        }),
    );
    let resp = plain_client().get(srv.url("/")).send().expect("GET /");
    assert_eq!(resp.status().as_u16(), 401);
    // challenge: false → no WWW-Authenticate header.
    assert!(
        resp.headers().get("www-authenticate").is_none(),
        "challenge:false must omit WWW-Authenticate"
    );
}

// ── API key tests ─────────────────────────────────────────────────────────────

fn server_with_api_key() -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "apiKey": {
                    "keys": ["tok-abc123", "tok-xyz789"],
                    "header": "x-api-key",
                    "skipPaths": ["/__health__"]
                }
            }]
        }),
    )
}

#[test]
fn api_key_missing_returns_401() {
    let srv = server_with_api_key();
    let resp = plain_client()
        .get(srv.url("/data"))
        .send()
        .expect("GET /data");
    assert_eq!(resp.status().as_u16(), 401);
}

#[test]
fn api_key_wrong_value_returns_401() {
    let srv = server_with_api_key();
    let resp = plain_client()
        .get(srv.url("/data"))
        .header("x-api-key", "bad-key")
        .send()
        .expect("GET /data");
    assert_eq!(resp.status().as_u16(), 401);
}

#[test]
fn api_key_first_key_accepted() {
    let srv = server_with_api_key();
    let resp = plain_client()
        .get(srv.url("/data"))
        .header("x-api-key", "tok-abc123")
        .send()
        .expect("GET /data");
    assert_ne!(resp.status().as_u16(), 401, "first key should be accepted");
}

#[test]
fn api_key_second_key_accepted() {
    let srv = server_with_api_key();
    let resp = plain_client()
        .get(srv.url("/data"))
        .header("x-api-key", "tok-xyz789")
        .send()
        .expect("GET /data");
    assert_ne!(resp.status().as_u16(), 401, "second key should be accepted");
}

#[test]
fn api_key_skip_path_bypasses_auth() {
    let srv = server_with_api_key();
    let resp = plain_client()
        .get(srv.url("/__health__"))
        .send()
        .expect("GET /__health__");
    assert_eq!(resp.status().as_u16(), 200);
}

// ── Rate limiting tests ───────────────────────────────────────────────────────

fn server_with_rate_limit(limit: u64, window_secs: u64) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "rateLimit": {
                    "windowSecs": window_secs,
                    "limit": limit,
                    "keyBy": "ip",
                    "skipPaths": ["/__health__"]
                }
            }]
        }),
    )
}

#[test]
fn rate_limit_allows_requests_within_limit() {
    // 5 requests per hour — all 5 must succeed.
    let srv = server_with_rate_limit(5, 3600);
    for i in 0..5 {
        let resp = plain_client()
            .get(srv.url("/"))
            .send()
            .unwrap_or_else(|_| panic!("request {i} failed to send"));
        assert_ne!(
            resp.status().as_u16(),
            429,
            "request {i} should be within limit"
        );
    }
}

#[test]
fn rate_limit_blocks_after_limit_exceeded() {
    // 3 requests per hour — the 4th must be rejected.
    let srv = server_with_rate_limit(3, 3600);
    for _ in 0..3 {
        plain_client().get(srv.url("/")).send().ok();
    }
    let resp = plain_client()
        .get(srv.url("/"))
        .send()
        .expect("4th request");
    assert_eq!(
        resp.status().as_u16(),
        429,
        "4th request should be rate-limited"
    );
}

#[test]
fn rate_limit_skip_path_not_counted() {
    // Only 1 token available per hour — exhaust it, then verify health still works.
    let srv = server_with_rate_limit(1, 3600);
    plain_client().get(srv.url("/")).send().ok(); // exhaust the token

    let resp = plain_client()
        .get(srv.url("/__health__"))
        .send()
        .expect("GET /__health__");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "health must not be rate-limited"
    );
}

#[test]
fn rate_limit_health_always_passes_at_handler_level() {
    // Even with limit=1 exhausted, health bypasses rate-limit at the handler level
    // (HandlerKind::Health skips the entire filter chain).
    let srv = server_with_rate_limit(1, 3600);
    plain_client().get(srv.url("/anything")).send().ok(); // exhaust

    for _ in 0..5 {
        let resp = plain_client()
            .get(srv.url("/__health__"))
            .send()
            .expect("health check");
        assert_eq!(resp.status().as_u16(), 200);
    }
}

// ── Combined: Basic Auth + Rate Limit ────────────────────────────────────────

#[test]
fn basic_auth_and_rate_limit_combined() {
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true,
                "rateLimit": {
                    "windowSecs": 3600,
                    "limit": 3,
                    "keyBy": "ip"
                },
                "basicAuth": {
                    "users": { "user": "pass" },
                    "challenge": false
                }
            }]
        }),
    );

    let authed = basic_header("user", "pass");

    // Rate-limit runs BEFORE auth: a failed auth request still consumes a token.
    // Token 1 consumed by this unauthenticated request → 401 from auth.
    let r1 = plain_client().get(srv.url("/")).send().expect("r1");
    assert_eq!(r1.status().as_u16(), 401);

    // Tokens 2 and 3 consumed by authed requests → pass.
    for i in 2..=3 {
        let r = plain_client()
            .get(srv.url("/"))
            .header("Authorization", &authed)
            .send()
            .unwrap_or_else(|_| panic!("request {i} failed"));
        assert_ne!(r.status().as_u16(), 401, "request {i}: auth should pass");
        assert_ne!(
            r.status().as_u16(),
            429,
            "request {i}: should not be rate-limited"
        );
    }

    // All 3 tokens exhausted — next request (even with valid auth) gets 429.
    let r_limited = plain_client()
        .get(srv.url("/"))
        .header("Authorization", &authed)
        .send()
        .expect("rate-limited request");
    assert_eq!(r_limited.status().as_u16(), 429);
}

// ── Rate limit keyBy: "header:..." ───────────────────────────────────────

#[test]
fn rate_limit_key_by_header_separate_clients_have_independent_buckets() {
    // Use keyBy: "header:X-Client-Id" so each unique header value gets its own
    // token bucket, regardless of IP.
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "rateLimit": {
                    "windowSecs": 3600,
                    "limit": 2,
                    "keyBy": "header:X-Client-Id"
                }
            }]
        }),
    );

    // Client A: exhaust its 2-request bucket.
    for i in 0..2 {
        let r = plain_client()
            .get(srv.url("/"))
            .header("X-Client-Id", "client-a")
            .send()
            .unwrap_or_else(|_| panic!("client-a request {i}"));
        assert_ne!(r.status().as_u16(), 429, "client-a request {i} within limit");
    }
    // Client A is now rate-limited.
    let r_a = plain_client()
        .get(srv.url("/"))
        .header("X-Client-Id", "client-a")
        .send()
        .expect("client-a over limit");
    assert_eq!(r_a.status().as_u16(), 429, "client-a must be rate-limited");

    // Client B has its OWN bucket — must still be allowed.
    let r_b = plain_client()
        .get(srv.url("/"))
        .header("X-Client-Id", "client-b")
        .send()
        .expect("client-b first request");
    assert_ne!(
        r_b.status().as_u16(),
        429,
        "client-b should not be affected by client-a's limit"
    );
}

#[test]
fn rate_limit_key_by_header_missing_header_uses_ip_fallback() {
    // When keyBy is a header and the header is absent, the rate limiter should
    // still handle the request (fall back to IP key or treat as single bucket).
    let port = common::free_port();
    let admin_port = common::free_port();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "rateLimit": {
                    "windowSecs": 3600,
                    "limit": 5,
                    "keyBy": "header:X-Missing-Header"
                }
            }]
        }),
    );

    // Request without the header should not crash — either allowed or limited,
    // but must return a well-formed HTTP response.
    let resp = plain_client()
        .get(srv.url("/"))
        .send()
        .expect("request without key header");
    let status = resp.status().as_u16();
    assert!(
        status != 500,
        "missing key header must not cause 500, got: {status}"
    );
}
