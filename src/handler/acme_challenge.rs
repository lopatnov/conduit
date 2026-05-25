use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::handler::response;

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
