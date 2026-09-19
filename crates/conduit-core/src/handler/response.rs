use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

/// Apply extra headers (CORS, security, …) to a response header under construction.
fn insert_extra(resp: &mut ResponseHeader, extra: &[(String, String)]) -> Result<()> {
    for (name, value) in extra {
        resp.insert_header(name.clone(), value.clone())?;
    }
    Ok(())
}

/// Write a 401 Unauthorized response with an optional `WWW-Authenticate` challenge.
///
/// The response body is content-type aware: JSON clients (those sending
/// `Accept: application/json`) receive a JSON body; all others receive an
/// empty body.  This matches the behaviour of Oathkeeper's conditional error
/// handlers.
pub async fn write_denied(
    session: &mut Session,
    www_authenticate: Option<&str>,
    extra: &[(String, String)],
) -> Result<()> {
    let wants_json = session
        .req_header()
        .headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/json"));

    if wants_json {
        let body = Bytes::from_static(b"{\"error\":\"Unauthorized\",\"status\":401}");
        let mut resp = ResponseHeader::build(401, Some(3 + extra.len()))?;
        resp.insert_header("content-type", "application/json")?;
        resp.insert_header("content-length", body.len().to_string())?;
        if let Some(challenge) = www_authenticate {
            resp.insert_header("www-authenticate", challenge)?;
        }
        insert_extra(&mut resp, extra)?;
        session.write_response_header(Box::new(resp), false).await?;
        session.write_response_body(Some(body), true).await
    } else {
        let mut resp = ResponseHeader::build(401, Some(2 + extra.len()))?;
        resp.insert_header("content-length", "0")?;
        if let Some(challenge) = www_authenticate {
            resp.insert_header("www-authenticate", challenge)?;
        }
        insert_extra(&mut resp, extra)?;
        session.write_response_header(Box::new(resp), false).await?;
        session.write_response_body(Some(Bytes::new()), true).await
    }
}

/// Write a redirect response (3xx + Location header, empty body).
///
/// Returns an error if `status` is not in the 3xx range.
pub async fn write_redirect(
    session: &mut Session,
    status: u16,
    location: &str,
    extra: &[(String, String)],
) -> Result<()> {
    if !(300..400).contains(&status) {
        return Err(pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            format!("write_redirect called with non-3xx status {status}"),
        ));
    }
    let mut resp = ResponseHeader::build(status, Some(2 + extra.len()))?;
    resp.insert_header("location", location)?;
    resp.insert_header("content-length", "0")?;
    insert_extra(&mut resp, extra)?;
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(Bytes::new()), true).await
}

/// Write a complete HTTP response (headers + body) to the downstream session.
pub async fn write_response(
    session: &mut Session,
    status: u16,
    content_type: &str,
    body: Bytes,
    extra: &[(String, String)],
) -> Result<()> {
    let mut resp = ResponseHeader::build(status, Some(2 + extra.len()))?;
    resp.insert_header("content-type", content_type)?;
    resp.insert_header("content-length", body.len().to_string())?;
    insert_extra(&mut resp, extra)?;
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(body), true).await
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Build a real `pingora_proxy::Session` with the given raw request line
    /// (headers included) already read off the wire — see
    /// `.claude/skills/testing/SKILL.md`'s coverage-driven exception (added
    /// alongside #248) for why this is a unit test, not an integration test.
    async fn session_with_request(raw_request: &[u8]) -> (Session, tokio::io::DuplexStream) {
        let (server_side, mut client_side) = tokio::io::duplex(4096);
        client_side.write_all(raw_request).await.unwrap();

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

    // ── write_denied ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn write_denied_json_client_gets_json_body() {
        let (mut session, mut client_side) =
            session_with_request(b"GET / HTTP/1.1\r\nHost: t\r\nAccept: application/json\r\n\r\n")
                .await;

        write_denied(&mut session, None, &[])
            .await
            .expect("write_denied");

        let response = read_response(&mut client_side).await;
        assert!(response.starts_with("HTTP/1.1 401"), "got: {response}");
        assert!(response.contains("application/json"), "got: {response}");
        assert!(
            response.contains(r#"{"error":"Unauthorized","status":401}"#),
            "got: {response}"
        );
    }

    #[tokio::test]
    async fn write_denied_non_json_client_gets_empty_body() {
        let (mut session, mut client_side) =
            session_with_request(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;

        write_denied(&mut session, None, &[])
            .await
            .expect("write_denied");

        let response = read_response(&mut client_side).await;
        assert!(response.starts_with("HTTP/1.1 401"), "got: {response}");
        assert!(response.contains("content-length: 0"), "got: {response}");
        assert!(!response.contains("application/json"), "got: {response}");
    }

    #[tokio::test]
    async fn write_denied_includes_www_authenticate_challenge() {
        let (mut session, mut client_side) =
            session_with_request(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;

        write_denied(&mut session, Some("Bearer realm=\"conduit\""), &[])
            .await
            .expect("write_denied");

        let response = read_response(&mut client_side).await;
        assert!(
            response.contains("www-authenticate: Bearer realm=\"conduit\""),
            "got: {response}"
        );
    }

    #[tokio::test]
    async fn write_denied_applies_extra_headers() {
        let (mut session, mut client_side) =
            session_with_request(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;

        write_denied(
            &mut session,
            None,
            &[("x-custom".to_owned(), "value1".to_owned())],
        )
        .await
        .expect("write_denied");

        let response = read_response(&mut client_side).await;
        assert!(response.contains("x-custom: value1"), "got: {response}");
    }

    // ── write_redirect ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn write_redirect_valid_3xx_writes_location() {
        let (mut session, mut client_side) =
            session_with_request(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;

        write_redirect(&mut session, 302, "https://example.com/new", &[])
            .await
            .expect("write_redirect");

        let response = read_response(&mut client_side).await;
        assert!(response.starts_with("HTTP/1.1 302"), "got: {response}");
        assert!(
            response.contains("location: https://example.com/new"),
            "got: {response}"
        );
        assert!(response.contains("content-length: 0"), "got: {response}");
    }

    #[tokio::test]
    async fn write_redirect_rejects_non_3xx_status() {
        let (mut session, _client_side) =
            session_with_request(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;

        let err = write_redirect(&mut session, 200, "https://example.com", &[])
            .await
            .expect_err("200 must be rejected");
        assert!(err.to_string().contains("non-3xx"), "got: {err}");
    }

    // ── write_response ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn write_response_writes_status_content_type_and_body() {
        let (mut session, mut client_side) =
            session_with_request(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;

        write_response(
            &mut session,
            201,
            "application/json",
            Bytes::from_static(b"{\"ok\":true}"),
            &[],
        )
        .await
        .expect("write_response");

        let response = read_response(&mut client_side).await;
        assert!(response.starts_with("HTTP/1.1 201"), "got: {response}");
        assert!(response.contains("application/json"), "got: {response}");
        assert!(response.contains(r#"{"ok":true}"#), "got: {response}");
    }
}
