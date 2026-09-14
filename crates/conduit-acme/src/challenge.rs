//! ACME HTTP-01 challenge-response handler.
//!
//! Compiled only with this crate's own `acme` Cargo feature — mirrors the
//! pre-extraction `#![cfg(feature = "acme")]` file-level gate on
//! `src/handler/acme_challenge.rs` (issue #114/#130). The root crate's
//! `src/handler/acme_challenge.rs` is now a thin facade re-exporting this
//! module's public items.
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use conduit_core::handler::response;
use conduit_core::handler::LocalHandlerImpl;
use dashmap::DashMap;
use pingora_core::Result;
use pingora_proxy::Session;

/// Handler struct for ACME HTTP-01 challenge responses.
pub struct AcmeChallengeHandler {
    pub token: String,
    pub challenges: Arc<DashMap<String, String>>,
    pub extra_headers: Vec<(String, String)>,
}

#[async_trait]
impl LocalHandlerImpl for AcmeChallengeHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        handle_acme_challenge(session, &self.token, &self.challenges).await
    }
}

/// Serve an ACME HTTP-01 challenge token response.
///
/// Looks up `token` in the shared challenge store; returns the key-authorization
/// string with `Content-Type: text/plain` when found, or 404 when not found.
pub async fn handle_acme_challenge(
    session: &mut Session,
    token: &str,
    challenges: &Arc<DashMap<String, String>>,
) -> Result<()> {
    match challenges.get(token) {
        Some(key_auth) => {
            let body = Bytes::copy_from_slice(key_auth.as_bytes());
            response::write_response(session, 200, "text/plain; charset=utf-8", body, &[]).await
        }
        None => {
            response::write_response(
                session,
                404,
                "text/plain",
                Bytes::from_static(b"challenge not found"),
                &[],
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Build a real `pingora_proxy::Session` with a parsed GET request
    /// already read off the wire — see `.claude/skills/testing/SKILL.md`'s
    /// coverage-driven exception (added alongside #248) for why this is a
    /// unit test rather than an integration test.
    async fn session_with_request() -> (Session, tokio::io::DuplexStream) {
        let (server_side, mut client_side) = tokio::io::duplex(4096);
        client_side
            .write_all(b"GET /.well-known/acme-challenge/tok123 HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();

        let stream: pingora_core::protocols::Stream = Box::new(server_side);
        let mut session = Session::new_h1(stream);
        session
            .as_downstream_mut()
            .read_request()
            .await
            .expect("read_request");

        (session, client_side)
    }

    async fn read_response(client_side: &mut tokio::io::DuplexStream) -> String {
        let mut buf = vec![0u8; 512];
        let n = client_side.read(&mut buf).await.expect("read response");
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    #[tokio::test]
    async fn known_token_returns_200_with_key_authorization_body() {
        let (mut session, mut client_side) = session_with_request().await;
        let challenges = Arc::new(DashMap::new());
        challenges.insert("tok123".to_owned(), "tok123.thumbprint-abc".to_owned());

        handle_acme_challenge(&mut session, "tok123", &challenges)
            .await
            .expect("handle_acme_challenge");

        let response = read_response(&mut client_side).await;
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert!(
            response.contains("tok123.thumbprint-abc"),
            "got: {response}"
        );
        assert!(
            response.to_lowercase().contains("text/plain"),
            "got: {response}"
        );
    }

    #[tokio::test]
    async fn unknown_token_returns_404() {
        let (mut session, mut client_side) = session_with_request().await;
        let challenges: Arc<DashMap<String, String>> = Arc::new(DashMap::new());

        handle_acme_challenge(&mut session, "nonexistent", &challenges)
            .await
            .expect("handle_acme_challenge");

        let response = read_response(&mut client_side).await;
        assert!(response.starts_with("HTTP/1.1 404"), "got: {response}");
        assert!(response.contains("challenge not found"), "got: {response}");
    }

    #[tokio::test]
    async fn handler_delegates_to_handle_acme_challenge() {
        let (mut session, mut client_side) = session_with_request().await;
        let challenges = Arc::new(DashMap::new());
        challenges.insert("tok123".to_owned(), "key-auth-value".to_owned());
        let mut handler = AcmeChallengeHandler {
            token: "tok123".to_owned(),
            challenges,
            extra_headers: vec![],
        };

        handler.handle(&mut session).await.expect("handle");

        let response = read_response(&mut client_side).await;
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert!(response.contains("key-auth-value"), "got: {response}");
    }
}
