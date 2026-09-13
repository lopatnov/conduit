use std::sync::atomic::Ordering;

use async_trait::async_trait;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use conduit_core::filter::path::is_path_skipped;
use conduit_core::handler::response;
use pingora_core::Result;

use crate::config::JwtAuthConfig;
use crate::jwt;

/// JWT bearer-token authentication guard.
///
/// Validates the `Authorization: Bearer <token>` header using either an HMAC
/// secret (`jwtAuth.secret`) or a remote JWKS endpoint (`jwtAuth.jwksUrl`).
/// Returns `401 Unauthorized` when the token is absent or invalid.
pub struct JwtGuard {
    pub cfg: JwtAuthConfig,
    pub path: String,
}

#[async_trait]
impl RequestFilter for JwtGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        let auth_header = ctx
            .session
            .req_header()
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok());

        match jwt::check_jwt(&self.cfg, &self.path, auth_header) {
            jwt::JwtCheckResult::Allowed => Ok(FilterOutcome::Continue),
            jwt::JwtCheckResult::Denied { reason } => {
                tracing::debug!(reason, "JWT validation denied");
                response::write_denied(ctx.session, Some("Bearer"), ctx.extra_headers).await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(FilterOutcome::Handled)
            }
        }
    }
}

/// Per-request JWT claim state.
///
/// Stored on the root crate's `RequestCtx.jwt` field
/// (`#[cfg(feature = "jwt")]`-gated, `src/proxy/ctx.rs`) — see `CLAUDE.md`
/// decision #30. `claims` is populated by [`extract_claims_from_session`]
/// after the guard chain runs, and read back by the root crate's
/// always-compiled `{{ jwt.<claim> }}` header-template expansion
/// (`template::expand_jwt_templates`) via a small `#[cfg]`-branching
/// accessor that lives on `RequestCtx` itself, not here.
#[derive(Debug, Default)]
pub struct JwtReqState {
    pub claims: Option<std::collections::HashMap<String, serde_json::Value>>,
}

/// Extract JWT claims from the `Authorization: Bearer` header for header
/// template substitution (`{{ jwt.<claim> }}`).
///
/// Returns `None` when JWT auth is not configured, the current path is in
/// `jwtAuth.skipPaths`, the header is missing or not a Bearer token, or the
/// token cannot be decoded.
///
/// The `skipPaths` check is required here, not just in [`JwtGuard`]: on a
/// skipped path `JwtGuard` lets the request through *without* verifying the
/// token's signature at all (see `jwt::jwt_prelude`), so if this function
/// didn't apply the same check it would decode and trust an attacker-forged,
/// unsigned token's claims for header-template substitution — effectively
/// spoofing `{{ jwt.<claim> }}` values (e.g. `{{ jwt.sub }}`) into whatever
/// upstream header a route's `requestTransform` injects them into, on any
/// path the operator intentionally exempted from auth.
///
/// Called from the root crate's `do_request_filter`
/// (`src/proxy/request_phase.rs`), after the guard chain runs.
pub fn extract_claims_from_session(
    session: &pingora_proxy::Session,
    jwt_cfg: Option<&JwtAuthConfig>,
) -> Option<JwtReqState> {
    let jwt_cfg = jwt_cfg?;
    let path = session.req_header().uri.path();
    if let Some(skip) = &jwt_cfg.skip_paths {
        if is_path_skipped(Some(skip.as_slice()), path) {
            return None;
        }
    }
    let auth_hdr = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())?;
    let token = jwt::extract_bearer(Some(auth_hdr))?;
    let claims = jwt::extract_claims_unchecked(token)?;
    Some(JwtReqState {
        claims: Some(claims),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Build a real [`pingora_proxy::Session`] with a parsed GET request
    /// already read off the wire, so guards that touch `ctx.session`
    /// (`req_header()`, `write_response`, …) can be exercised as real unit
    /// tests instead of only via `tests/*.rs` integration tests (which
    /// `cargo llvm-cov --lib` never instruments — see #248). Mirrors
    /// `conduit-faults`'s own `guard.rs` test helper (#132).
    async fn session_with_request(raw: &[u8]) -> (pingora_proxy::Session, tokio::io::DuplexStream) {
        let (server_side, mut client_side) = tokio::io::duplex(4096);
        client_side.write_all(raw).await.unwrap();

        let stream: pingora_core::protocols::Stream = Box::new(server_side);
        let mut session = pingora_proxy::Session::new_h1(stream);
        session
            .as_downstream_mut()
            .read_request()
            .await
            .expect("read_request");

        (session, client_side)
    }

    fn hs256_token(secret: &str, claims: serde_json::Value) -> String {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let key = EncodingKey::from_secret(secret.as_bytes());
        encode(&Header::new(Algorithm::HS256), &claims, &key).unwrap()
    }

    fn exp_future() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600
    }

    #[tokio::test]
    async fn apply_denies_missing_token_with_401() {
        let (mut session, mut client_sock) =
            session_with_request(b"GET /api HTTP/1.1\r\nHost: test\r\n\r\n").await;
        let guard = JwtGuard {
            cfg: JwtAuthConfig {
                secret: Some("s3cr3t".into()),
                ..Default::default()
            },
            path: "/api".to_owned(),
        };
        let inflight = AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Handled));
        assert_eq!(inflight.load(Ordering::Relaxed), 0);

        let mut buf = vec![0u8; 512];
        let n = client_sock.read(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 401"), "got: {response}");
    }

    #[tokio::test]
    async fn apply_allows_valid_token() {
        let secret = "test-secret";
        let token = hs256_token(
            secret,
            serde_json::json!({ "sub": "u", "exp": exp_future() }),
        );
        let raw =
            format!("GET /api HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer {token}\r\n\r\n");
        let (mut session, _client_sock) = session_with_request(raw.as_bytes()).await;
        let guard = JwtGuard {
            cfg: JwtAuthConfig {
                secret: Some(secret.into()),
                ..Default::default()
            },
            path: "/api".to_owned(),
        };
        let inflight = AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Continue));
        // Allowed path never touches inflight.
        assert_eq!(inflight.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn extract_claims_from_session_none_when_not_configured() {
        let (session, _client_sock) =
            session_with_request(b"GET /api HTTP/1.1\r\nHost: test\r\n\r\n").await;
        assert!(extract_claims_from_session(&session, None).is_none());
    }

    #[tokio::test]
    async fn extract_claims_from_session_none_on_skipped_path() {
        let (session, _client_sock) =
            session_with_request(b"GET /public/asset HTTP/1.1\r\nHost: test\r\n\r\n").await;
        let cfg = JwtAuthConfig {
            secret: Some("secret".into()),
            skip_paths: Some(vec!["/public/**".into()]),
            ..Default::default()
        };
        assert!(extract_claims_from_session(&session, Some(&cfg)).is_none());
    }

    #[tokio::test]
    async fn extract_claims_from_session_returns_claims_for_valid_token() {
        let secret = "claim-secret";
        let token = hs256_token(
            secret,
            serde_json::json!({ "sub": "user42", "exp": exp_future() }),
        );
        let raw =
            format!("GET /api HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer {token}\r\n\r\n");
        let (session, _client_sock) = session_with_request(raw.as_bytes()).await;
        let cfg = JwtAuthConfig {
            secret: Some(secret.into()),
            ..Default::default()
        };
        let state = extract_claims_from_session(&session, Some(&cfg))
            .expect("claims must be extracted for a well-formed bearer token");
        let claims = state.claims.expect("claims field must be populated");
        assert_eq!(claims.get("sub").and_then(|v| v.as_str()), Some("user42"));
    }

    #[tokio::test]
    async fn extract_claims_from_session_none_without_bearer_header() {
        let (session, _client_sock) =
            session_with_request(b"GET /api HTTP/1.1\r\nHost: test\r\n\r\n").await;
        let cfg = JwtAuthConfig {
            secret: Some("secret".into()),
            ..Default::default()
        };
        assert!(extract_claims_from_session(&session, Some(&cfg)).is_none());
    }
}
