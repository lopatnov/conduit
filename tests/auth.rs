mod common;

use base64::Engine as _;
#[cfg(feature = "jwt")]
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::blocking::Client;
#[cfg(feature = "jwt")]
use serde_json::json;

// ── helpers ────────────────────────────────────────────────────────────

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

// ── API key tests ──────────────────────────────────────────────────────────────

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

// ── Rate limiting tests ─────────────────────────────────────────────────────────

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

// ── Combined: Basic Auth + Rate Limit ──────────────────────────────────

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

// ── Rate limit keyBy: "header:..." ──────────────────────────────

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
        assert_ne!(
            r.status().as_u16(),
            429,
            "client-a request {i} within limit"
        );
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
fn rate_limit_key_by_header_missing_header_falls_back_to_shared_bucket() {
    // When keyBy names a header that is absent, the rate limiter must not crash
    // and must still enforce the limit (requests without the header share a
    // fallback bucket, so they are subject to the same window limit).
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
                    "limit": 3,
                    "keyBy": "header:X-Missing-Header"
                }
            }]
        }),
    );

    // First 3 requests: must not 500 (server is alive) and not 429 yet.
    for i in 0..3 {
        let resp = plain_client()
            .get(srv.url("/"))
            .send()
            .unwrap_or_else(|_| panic!("request {i} failed to send"));
        let status = resp.status().as_u16();
        assert_ne!(
            status, 500,
            "request {i}: missing key header must not cause 500"
        );
        assert_ne!(status, 429, "request {i}: should be within the limit");
    }

    // 4th request must be rate-limited — the fallback bucket is exhausted.
    let resp = plain_client()
        .get(srv.url("/"))
        .send()
        .expect("4th request");
    assert_eq!(
        resp.status().as_u16(),
        429,
        "4th request must be rate-limited when missing-header bucket is exhausted"
    );
}

// ── JWT helpers (used by JWT and Consumer JWT tests) ───────────────────────
// These live outside any mod so both jwt and consumers_tests can access them.
#[cfg(feature = "jwt")]
fn jwt_secret() -> &'static str {
    "test-jwt-secret-for-conduit"
}

#[cfg(feature = "jwt")]
fn make_jwt(secret: &str, exp_offset_secs: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let exp = (now + exp_offset_secs) as u64;
    let claims = json!({ "sub": "testuser", "exp": exp });
    let key = EncodingKey::from_secret(secret.as_bytes());
    encode(&Header::new(Algorithm::HS256), &claims, &key).unwrap()
}

// ── JWT Auth tests (require --features jwt) ──────────────────────────────
#[cfg(feature = "jwt")]
mod jwt {
    use super::*;

    fn server_with_jwt_auth() -> common::TestServer {
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
                    "jwtAuth": {
                        "secret": jwt_secret(),
                        "skipPaths": ["/__health__"]
                    }
                }]
            }),
        )
    }

    #[test]
    fn jwt_no_token_returns_401() {
        let srv = server_with_jwt_auth();
        let resp = plain_client().get(srv.url("/")).send().expect("GET /");
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[test]
    fn jwt_invalid_token_returns_401() {
        let srv = server_with_jwt_auth();
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", "Bearer this.is.not.valid")
            .send()
            .expect("GET /");
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[test]
    fn jwt_wrong_secret_returns_401() {
        let srv = server_with_jwt_auth();
        let token = make_jwt("wrong-secret", 3600);
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[test]
    fn jwt_valid_token_passes() {
        let srv = server_with_jwt_auth();
        let token = make_jwt(jwt_secret(), 3600);
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        // No upstream configured — fallback handler returns 404 (not 401).
        assert_ne!(
            resp.status().as_u16(),
            401,
            "valid JWT should not be rejected"
        );
    }

    #[test]
    fn jwt_skip_path_bypasses_auth() {
        let srv = server_with_jwt_auth();
        let resp = plain_client()
            .get(srv.url("/__health__"))
            .send()
            .expect("GET /__health__");
        assert_eq!(resp.status().as_u16(), 200);
    }

    /// Regression test: on a `skipPaths` route, `JwtGuard` never checks the
    /// token's signature at all (see `jwt::jwt_prelude`) — so
    /// `{{ jwt.<claim> }}` header-template substitution must not trust an
    /// unverified token's claims either, or an attacker could forge
    /// arbitrary claim values (e.g. spoof `{{ jwt.sub }}` into an
    /// upstream-trusted identity header) simply by hitting a path the
    /// operator intentionally exempted from JWT auth.
    #[test]
    fn jwt_skip_path_does_not_trust_forged_claims_in_templates() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(String::new()));
        let cap_clone = captured.clone();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let cap = cap_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                });
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
                    "proxy": format!("http://{upstream_addr}"),
                    "jwtAuth": {
                        "secret": jwt_secret(),
                        "skipPaths": ["/public/**"]
                    },
                    "requestTransform": {
                        "setHeaders": { "X-User": "{{ jwt.sub }}" }
                    }
                }]
            }),
        );

        // Token signed with a completely different secret than jwtAuth's own —
        // insecure_decode doesn't check the signature at all, so this "attacker"
        // token decodes cleanly if (and only if) the vulnerable path is hit.
        let forged_token = make_jwt("attacker-controlled-secret-not-jwtauths", 3600);

        plain_client()
            .get(srv.url("/public/anything"))
            .header("authorization", format!("Bearer {forged_token}"))
            .send()
            .expect("GET /public/anything");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let req_text = captured.lock().unwrap().clone();
        assert!(
            !req_text.is_empty(),
            "upstream never received the request; test would pass vacuously"
        );
        assert!(
            !req_text.to_ascii_lowercase().contains("testuser"),
            "forged/unverified JWT claims on a skipPaths route must NOT reach \
             {{{{ jwt.<claim> }}}} template substitution; got upstream request:\n{req_text}"
        );
    }

    #[test]
    fn jwt_www_authenticate_header_on_401() {
        let srv = server_with_jwt_auth();
        let resp = plain_client().get(srv.url("/")).send().expect("GET /");
        assert_eq!(resp.status().as_u16(), 401);
        let www_auth = resp.headers().get("www-authenticate");
        assert!(
            www_auth.is_some(),
            "WWW-Authenticate header should be present"
        );
        assert!(
            www_auth.unwrap().to_str().unwrap().contains("Bearer"),
            "WWW-Authenticate should contain 'Bearer'"
        );
    }

    // ── Per-route rate limit tests ─────────────────────────────────────

    fn server_with_per_route_rate_limit() -> common::TestServer {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
            }
        });

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
                        "/api": {
                            "targets": [ format!("http://{upstream_addr}") ],
                            "rateLimit": { "windowSecs": 3600, "limit": 2, "keyBy": "ip" }
                        }
                    }
                }]
            }),
        )
    }

    #[test]
    fn per_route_rate_limit_within_limit_passes() {
        let srv = server_with_per_route_rate_limit();
        for _ in 0..2 {
            let resp = plain_client()
                .get(srv.url("/api/data"))
                .send()
                .expect("GET /api/data");
            assert_eq!(resp.status().as_u16(), 200, "first 2 requests should pass");
        }
    }

    #[test]
    fn per_route_rate_limit_exceeded_returns_429() {
        let srv = server_with_per_route_rate_limit();
        // Exhaust the limit (2 requests).
        plain_client().get(srv.url("/api/x")).send().ok();
        plain_client().get(srv.url("/api/x")).send().ok();
        // Third request must be rate-limited.
        let resp = plain_client()
            .get(srv.url("/api/x"))
            .send()
            .expect("GET /api/x");
        assert_eq!(
            resp.status().as_u16(),
            429,
            "3rd request must be rate-limited (per-route limit=2)"
        );
    }

    #[test]
    fn per_route_rate_limit_skip_path_not_counted() {
        // #307: rateLimit.skipPaths was silently ignored at the route level.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
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
                    "proxy": {
                        "/api": {
                            "targets": [ format!("http://{upstream_addr}") ],
                            "rateLimit": {
                                "windowSecs": 3600, "limit": 1, "keyBy": "ip",
                                "skipPaths": ["/api/exempt"]
                            }
                        }
                    }
                }]
            }),
        );

        // Exhaust the limit=1 budget on a non-exempt path.
        plain_client().get(srv.url("/api/data")).send().ok();
        let resp = plain_client()
            .get(srv.url("/api/data"))
            .send()
            .expect("GET /api/data");
        assert_eq!(resp.status().as_u16(), 429, "budget must be exhausted");

        // The exempt path must still pass, even with the budget exhausted.
        let resp = plain_client()
            .get(srv.url("/api/exempt"))
            .send()
            .expect("GET /api/exempt");
        assert_ne!(
            resp.status().as_u16(),
            429,
            "skipPaths-matched route path must not be rate-limited"
        );
    }

    // ── JWKS / RS256 / ES256 end-to-end (issue #164) ────────────────────────
    //
    // RSA-2048 / P-256 test key material is generated fresh at test-run
    // time (not embedded as static PEM literals) — matches the `rcgen`
    // "no checked-in cert fixtures" idiom already used for TLS test certs
    // (`.claude/skills/testing/SKILL.md`). Same approach as
    // `src/filter/jwt.rs`'s unit tests; duplicated here since integration
    // tests are a separate compilation unit.

    struct JwksTestRsaKey {
        pem: String,
        n: String,
        e: String,
    }

    fn gen_jwks_test_rsa_key() -> JwksTestRsaKey {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::traits::PublicKeyParts;
        let private_key =
            rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).expect("RSA-2048 keygen");
        let pem = private_key
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("RSA PKCS#1 PEM encode")
            .to_string();
        let public_key = private_key.to_public_key();
        JwksTestRsaKey {
            pem,
            n: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(public_key.n().to_bytes_be()),
            e: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(public_key.e().to_bytes_be()),
        }
    }

    struct JwksTestEcKey {
        pem: String,
        x: String,
        y: String,
    }

    fn gen_jwks_test_ec_key() -> JwksTestEcKey {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        use p256::pkcs8::EncodePrivateKey;
        let secret_key = p256::SecretKey::random(&mut rand_core::OsRng);
        let pem = secret_key
            .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
            .expect("EC PKCS#8 PEM encode")
            .to_string();
        let point = secret_key.public_key().to_encoded_point(false);
        JwksTestEcKey {
            pem,
            x: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(point.x().expect("uncompressed point has x")),
            y: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(point.y().expect("uncompressed point has y")),
        }
    }

    /// RSA-2048 keygen is comparatively slow — generate once per test
    /// binary and share read-only across tests (the key material itself
    /// doesn't need to vary between them).
    fn rsa_key() -> &'static JwksTestRsaKey {
        static KEY: std::sync::OnceLock<JwksTestRsaKey> = std::sync::OnceLock::new();
        KEY.get_or_init(gen_jwks_test_rsa_key)
    }

    fn ec_key() -> &'static JwksTestEcKey {
        static KEY: std::sync::OnceLock<JwksTestEcKey> = std::sync::OnceLock::new();
        KEY.get_or_init(gen_jwks_test_ec_key)
    }

    /// Raw-TCP mock JWKS endpoint (see `.claude/skills/testing/SKILL.md`
    /// "Mock upstream = raw TcpListener, not Axum") — returns `body` as a
    /// `200 application/json` response for every connection it accepts.
    fn spawn_mock_jwks_server(body: String) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let body = body.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes());
                });
            }
        });
        addr
    }

    fn server_with_jwks_auth(jwks_url: &str) -> common::TestServer {
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
                    "jwtAuth": {
                        "jwksUrl": jwks_url,
                        "skipPaths": ["/__health__"]
                    }
                }]
            }),
        )
    }

    fn jwks_exp_future() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600
    }

    #[test]
    fn jwt_jwks_rs256_valid_token_passes() {
        let rsa = rsa_key();
        let jwks_body = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"rsa-1","n":"{}","e":"{}"}}]}}"#,
            rsa.n, rsa.e
        );
        let jwks_addr = spawn_mock_jwks_server(jwks_body);
        let srv = server_with_jwks_auth(&format!("http://{jwks_addr}/jwks"));

        let key = EncodingKey::from_rsa_pem(rsa.pem.as_bytes()).unwrap();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("rsa-1".into());
        let token = encode(
            &header,
            &json!({ "sub": "u", "exp": jwks_exp_future() }),
            &key,
        )
        .unwrap();

        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_ne!(
            resp.status().as_u16(),
            401,
            "valid RS256/JWKS token should not be rejected"
        );
    }

    #[test]
    fn jwt_jwks_es256_valid_token_passes() {
        let ec = ec_key();
        let jwks_body = format!(
            r#"{{"keys":[{{"kty":"EC","kid":"ec-1","crv":"P-256","x":"{}","y":"{}"}}]}}"#,
            ec.x, ec.y
        );
        let jwks_addr = spawn_mock_jwks_server(jwks_body);
        let srv = server_with_jwks_auth(&format!("http://{jwks_addr}/jwks"));

        let key = EncodingKey::from_ec_pem(ec.pem.as_bytes()).unwrap();
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some("ec-1".into());
        let token = encode(
            &header,
            &json!({ "sub": "u", "exp": jwks_exp_future() }),
            &key,
        )
        .unwrap();

        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_ne!(
            resp.status().as_u16(),
            401,
            "valid ES256/JWKS token should not be rejected"
        );
    }

    #[test]
    fn jwt_jwks_wrong_kid_returns_401() {
        // JWKS only advertises "rsa-other" — a token signed with kid
        // "rsa-1" has no matching key to verify against.
        let rsa = rsa_key();
        let jwks_body = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"rsa-other","n":"{}","e":"{}"}}]}}"#,
            rsa.n, rsa.e
        );
        let jwks_addr = spawn_mock_jwks_server(jwks_body);
        let srv = server_with_jwks_auth(&format!("http://{jwks_addr}/jwks"));

        let key = EncodingKey::from_rsa_pem(rsa.pem.as_bytes()).unwrap();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("rsa-1".into());
        let token = encode(
            &header,
            &json!({ "sub": "u", "exp": jwks_exp_future() }),
            &key,
        )
        .unwrap();

        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "kid with no matching JWKS key must be rejected"
        );
    }
} // mod jwt

// ── Consumer model tests (require --features consumers) ──────────────────────────
#[cfg(feature = "consumers")]
mod consumers_tests {
    use super::*;

    fn server_with_consumers() -> common::TestServer {
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
                    "consumers": {
                        "consumers": [
                            {
                                "username": "alice",
                                "apiKey": "key-alice-secret",
                                "headers": { "X-Tier": "free" }
                            },
                            {
                                "username": "bob",
                                "apiKey": "key-bob-secret",
                                "rateLimit": { "windowSecs": 3600, "limit": 2 }
                            },
                            {
                                "username": "carol",
                                "basicAuth": { "password": "carol-pass" }
                            }
                        ],
                        "skipPaths": ["/__health__"]
                    }
                }]
            }),
        )
    }

    #[test]
    fn consumers_no_credentials_returns_401() {
        let srv = server_with_consumers();
        let resp = plain_client().get(srv.url("/")).send().expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "request without credentials must be rejected"
        );
    }

    #[test]
    fn consumers_wrong_api_key_returns_401() {
        let srv = server_with_consumers();
        let resp = plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "wrong-key")
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "wrong API key must be rejected"
        );
    }

    #[test]
    fn consumers_correct_api_key_passes() {
        let srv = server_with_consumers();
        let resp = plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "key-alice-secret")
            .send()
            .expect("GET /");
        // No upstream → 404, but NOT 401
        assert_ne!(
            resp.status().as_u16(),
            401,
            "valid API key must be accepted"
        );
    }

    #[test]
    fn consumers_x_consumer_id_injected() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(String::new()));
        let cap_clone = captured.clone();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let cap = cap_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                });
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
                    "proxy": format!("http://{upstream_addr}"),
                    "consumers": {
                        "consumers": [
                            { "username": "alice", "apiKey": "key-alice" }
                        ]
                    }
                }]
            }),
        );

        plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "key-alice")
            .send()
            .expect("GET /");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let req_text = captured.lock().unwrap().clone();
        assert!(
            req_text
                .to_ascii_lowercase()
                .contains("x-consumer-id: alice"),
            "X-Consumer-ID header must be injected into upstream request; got:\n{req_text}"
        );
    }

    #[test]
    fn consumers_basic_auth_passes() {
        let srv = server_with_consumers();
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", basic_header("carol", "carol-pass"))
            .send()
            .expect("GET /");
        assert_ne!(
            resp.status().as_u16(),
            401,
            "valid Basic Auth consumer must be accepted"
        );
    }

    /// The `Basic` scheme token is case-insensitive per RFC 7235 — a client
    /// sending `authorization: basic ...` (lowercase) must be accepted, same
    /// as the top-level `basicAuth` guard already handles.
    #[test]
    fn consumers_basic_auth_lowercase_scheme_passes() {
        let srv = server_with_consumers();
        let header_val = basic_header("carol", "carol-pass").replacen("Basic ", "basic ", 1);
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", header_val)
            .send()
            .expect("GET /");
        assert_ne!(
            resp.status().as_u16(),
            401,
            "lowercase 'basic' scheme must be accepted"
        );
    }

    #[test]
    fn consumers_wrong_basic_password_returns_401() {
        let srv = server_with_consumers();
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", basic_header("carol", "wrong-password"))
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "wrong Basic Auth password must be rejected"
        );
    }

    #[test]
    fn consumers_basic_auth_x_consumer_id_injected() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(String::new()));
        let cap_clone = captured.clone();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let cap = cap_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                });
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
                    "proxy": format!("http://{upstream_addr}"),
                    "consumers": {
                        "consumers": [
                            { "username": "carol", "basicAuth": { "password": "carol-pass" } }
                        ]
                    }
                }]
            }),
        );

        plain_client()
            .get(srv.url("/"))
            .header("authorization", basic_header("carol", "carol-pass"))
            .send()
            .expect("GET /");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let req_text = captured.lock().unwrap().clone();
        assert!(
            req_text
                .to_ascii_lowercase()
                .contains("x-consumer-id: carol"),
            "X-Consumer-ID header must be injected for Basic Auth consumer; got:\n{req_text}"
        );
    }

    #[test]
    fn consumers_custom_headers_injected() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(String::new()));
        let cap_clone = captured.clone();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let cap = cap_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                });
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
                    "proxy": format!("http://{upstream_addr}"),
                    "consumers": {
                        "consumers": [
                            {
                                "username": "alice",
                                "apiKey": "key-alice-secret",
                                "headers": { "X-Tier": "free" }
                            }
                        ]
                    }
                }]
            }),
        );

        plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "key-alice-secret")
            .send()
            .expect("GET /");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let req_text = captured.lock().unwrap().clone();
        assert!(
            req_text.to_ascii_lowercase().contains("x-tier: free"),
            "consumer.headers custom header must be injected into upstream request; got:\n{req_text}"
        );
    }

    #[test]
    fn consumers_skip_path_bypasses_auth() {
        let srv = server_with_consumers();
        let resp = plain_client()
            .get(srv.url("/__health__"))
            .send()
            .expect("health");
        assert_eq!(
            resp.status().as_u16(),
            200,
            "skip path must bypass consumers auth"
        );
    }

    #[test]
    fn consumers_per_consumer_rate_limit() {
        let port = common::free_port();
        let admin_port = common::free_port();
        let srv = common::TestServer::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [{
                    "port": port,
                    "consumers": {
                        "consumers": [{
                            "username": "limited",
                            "apiKey": "limited-key",
                            "rateLimit": { "windowSecs": 3600, "limit": 2 }
                        }]
                    }
                }]
            }),
        );

        // Two requests within limit — should pass (404 = no upstream, not 429 or 401)
        for _ in 0..2 {
            let resp = plain_client()
                .get(srv.url("/"))
                .header("x-api-key", "limited-key")
                .send()
                .expect("GET /");
            assert_ne!(
                resp.status().as_u16(),
                429,
                "within-limit requests must not be rate-limited"
            );
        }

        // Third request — must be rate-limited
        let resp = plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "limited-key")
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            429,
            "3rd request must hit per-consumer rate limit"
        );
    }

    #[test]
    fn consumers_per_consumer_rate_limit_skip_path_not_counted() {
        // #307: rateLimit.skipPaths was silently ignored at the consumer level.
        let port = common::free_port();
        let admin_port = common::free_port();
        let srv = common::TestServer::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [{
                    "port": port,
                    "consumers": {
                        "consumers": [{
                            "username": "limited",
                            "apiKey": "limited-key",
                            "rateLimit": {
                                "windowSecs": 3600, "limit": 1,
                                "skipPaths": ["/exempt"]
                            }
                        }]
                    }
                }]
            }),
        );

        // Exhaust the limit=1 budget on a non-exempt path.
        plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "limited-key")
            .send()
            .ok();
        let resp = plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "limited-key")
            .send()
            .expect("GET /");
        assert_eq!(resp.status().as_u16(), 429, "budget must be exhausted");

        // The exempt path must still pass, even with the budget exhausted.
        let resp = plain_client()
            .get(srv.url("/exempt"))
            .header("x-api-key", "limited-key")
            .send()
            .expect("GET /exempt");
        assert_ne!(
            resp.status().as_u16(),
            429,
            "skipPaths-matched consumer rate limit must not reject"
        );
    }

    #[test]
    fn consumers_per_consumer_rate_limit_dry_run_logs_but_allows() {
        // #307: rateLimit.dryRun was silently ignored at the consumer level.
        let port = common::free_port();
        let admin_port = common::free_port();
        let srv = common::TestServer::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [{
                    "port": port,
                    "consumers": {
                        "consumers": [{
                            "username": "limited",
                            "apiKey": "limited-key",
                            "rateLimit": { "windowSecs": 3600, "limit": 1, "dryRun": true }
                        }]
                    }
                }]
            }),
        );

        // Exhaust the limit=1 budget.
        plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "limited-key")
            .send()
            .ok();
        // With dryRun, the 2nd request must still pass through (not 429).
        let resp = plain_client()
            .get(srv.url("/"))
            .header("x-api-key", "limited-key")
            .send()
            .expect("GET /");
        assert_ne!(
            resp.status().as_u16(),
            429,
            "dryRun must allow the request through instead of rejecting"
        );
    }

    // ── Consumer JWT V2 tests (also require --features jwt) ──────────────────────────
    // These tests use JWT tokens and require both consumers AND jwt features.

    #[cfg(feature = "jwt")]
    fn server_with_jwt_consumer(secret: &str) -> common::TestServer {
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
                    "consumers": {
                        "consumers": [
                            {
                                "username": "jwt-service",
                                "jwt": { "secret": secret }
                            }
                        ],
                        "skipPaths": ["/__health__"]
                    }
                }]
            }),
        )
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_jwt_hs256_correct_passes() {
        let secret = "consumers-jwt-test-secret";
        let srv = server_with_jwt_consumer(secret);
        let token = make_jwt(secret, 3600);
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        // No upstream → 404, but NOT 401
        assert_ne!(
            resp.status().as_u16(),
            401,
            "valid JWT consumer token must be accepted"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_jwt_hs256_wrong_secret_returns_401() {
        let srv = server_with_jwt_consumer("correct-secret");
        let token = make_jwt("wrong-secret", 3600);
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "JWT signed with wrong secret must be rejected"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_jwt_expired_returns_401() {
        let secret = "expired-test-secret";
        let srv = server_with_jwt_consumer(secret);
        // Expire 120 s in the past — beyond jsonwebtoken's default 60 s leeway
        let token = make_jwt(secret, -120);
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "expired JWT consumer token must be rejected"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_jwt_x_consumer_id_injected() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(String::new()));
        let cap_clone = captured.clone();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let cap = cap_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                });
            }
        });

        let secret = "jwt-id-inject-secret";
        let port = common::free_port();
        let admin_port = common::free_port();
        let srv = common::TestServer::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [{
                    "port": port,
                    "proxy": format!("http://{upstream_addr}"),
                    "consumers": {
                        "consumers": [
                            { "username": "jwt-client", "jwt": { "secret": secret } }
                        ]
                    }
                }]
            }),
        );

        let token = make_jwt(secret, 3600);
        plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let req_text = captured.lock().unwrap().clone();
        assert!(
            req_text
                .to_ascii_lowercase()
                .contains("x-consumer-id: jwt-client"),
            "X-Consumer-ID header must be injected for JWT consumer; got:\n{req_text}"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_jwt_no_bearer_returns_401() {
        let srv = server_with_jwt_consumer("some-secret");
        // No Authorization header at all
        let resp = plain_client().get(srv.url("/")).send().expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "request without Bearer token must be rejected by JWT consumer"
        );
    }

    // ── Consumer sharedJwt V3 tests (also require --features jwt) ────────────────

    #[cfg(feature = "jwt")]
    fn server_with_shared_jwt(secret: &str) -> common::TestServer {
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
                    "consumers": {
                        // Shared JWKS: one secret for all consumers (V3)
                        "sharedJwt": {
                            "secret": secret
                            // usernameClaim defaults to "sub"
                        },
                        "skipPaths": ["/__health__"],
                        // Consumers are identified by jwt.sub == username
                        "consumers": [
                            {
                                "username": "alice-sub",
                                "rateLimit": { "windowSecs": 3600, "limit": 5 }
                            },
                            {
                                "username": "bob-sub",
                                "headers": { "X-Tier": "premium" }
                            }
                        ]
                    }
                }]
            }),
        )
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_shared_jwt_identifies_by_sub() {
        let secret = "shared-jwt-test-secret";
        let srv = server_with_shared_jwt(secret);
        // Token with sub = "alice-sub" — should match alice-sub consumer
        let token = make_jwt(secret, 3600); // sub = "testuser" in make_jwt default
                                            // We need a token with sub = "alice-sub" — use custom claims
        use serde_json::json;
        use std::time::{SystemTime, UNIX_EPOCH};
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let claims = json!({ "sub": "alice-sub", "exp": exp });
        let key = jsonwebtoken::EncodingKey::from_secret(secret.as_bytes());
        let token_alice = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &key,
        )
        .unwrap();

        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token_alice}"))
            .send()
            .expect("GET /");
        // No upstream → 404, but NOT 401
        assert_ne!(
            resp.status().as_u16(),
            401,
            "token with sub=alice-sub should be accepted by sharedJwt"
        );
        // Also verify: unrelated token with correct secret should not match if sub unknown
        let _ = token; // suppress unused warning
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_shared_jwt_unknown_sub_returns_401() {
        let secret = "shared-jwt-test-secret";
        let srv = server_with_shared_jwt(secret);
        // Token is valid (correct signature) but sub doesn't match any consumer username
        use serde_json::json;
        use std::time::{SystemTime, UNIX_EPOCH};
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let claims = json!({ "sub": "nobody-knows-me", "exp": exp });
        let key = jsonwebtoken::EncodingKey::from_secret(secret.as_bytes());
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &key,
        )
        .unwrap();

        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "valid JWT but unknown sub must return 401 (no matching consumer)"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_shared_jwt_wrong_secret_returns_401() {
        let srv = server_with_shared_jwt("correct-shared-secret");
        // Token signed with the WRONG secret
        let bad_token = make_jwt("wrong-secret", 3600);
        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {bad_token}"))
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            401,
            "JWT signed with wrong shared secret must be rejected"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_shared_jwt_x_consumer_id_injected() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(String::new()));
        let cap_clone = captured.clone();
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in upstream.incoming() {
                let Ok(mut s) = stream else { break };
                let cap = cap_clone.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
                });
            }
        });

        let secret = "shared-jwt-id-inject-secret";
        let port = common::free_port();
        let admin_port = common::free_port();
        let srv = common::TestServer::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [{
                    "port": port,
                    "proxy": format!("http://{upstream_addr}"),
                    "consumers": {
                        "sharedJwt": { "secret": secret },
                        "consumers": [
                            { "username": "alice-sub" }
                        ]
                    }
                }]
            }),
        );

        use serde_json::json;
        use std::time::{SystemTime, UNIX_EPOCH};
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let claims = json!({ "sub": "alice-sub", "exp": exp });
        let key = jsonwebtoken::EncodingKey::from_secret(secret.as_bytes());
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &key,
        )
        .unwrap();

        plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let req_text = captured.lock().unwrap().clone();
        assert!(
            req_text
                .to_ascii_lowercase()
                .contains("x-consumer-id: alice-sub"),
            "X-Consumer-ID header must be injected for sharedJwt consumer; got:\n{req_text}"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_shared_jwt_per_consumer_rate_limit() {
        let secret = "shared-jwt-rate-limit-secret";
        let srv = server_with_shared_jwt(secret);
        // alice-sub has rateLimit: { windowSecs: 3600, limit: 5 } in the fixture.
        use serde_json::json;
        use std::time::{SystemTime, UNIX_EPOCH};
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let claims = json!({ "sub": "alice-sub", "exp": exp });
        let key = jsonwebtoken::EncodingKey::from_secret(secret.as_bytes());
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &key,
        )
        .unwrap();

        for _ in 0..5 {
            let resp = plain_client()
                .get(srv.url("/"))
                .header("authorization", format!("Bearer {token}"))
                .send()
                .expect("GET /");
            assert_ne!(
                resp.status().as_u16(),
                429,
                "within-limit requests must not be rate-limited"
            );
        }

        let resp = plain_client()
            .get(srv.url("/"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .expect("GET /");
        assert_eq!(
            resp.status().as_u16(),
            429,
            "6th request must hit per-consumer rate limit (limit=5) via the sharedJwt path"
        );
    }

    #[cfg(feature = "jwt")]
    #[test]
    fn consumers_shared_jwt_skip_path_bypasses() {
        let srv = server_with_shared_jwt("any-secret");
        let resp = plain_client()
            .get(srv.url("/__health__"))
            .send()
            .expect("health");
        assert_eq!(
            resp.status().as_u16(),
            200,
            "skipPaths must bypass sharedJwt auth"
        );
    }
} // mod consumers_tests
