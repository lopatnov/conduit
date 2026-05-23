use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

/// Write a redirect response (3xx + Location header, empty body).
pub async fn write_redirect(session: &mut Session, status: u16, location: &str) -> Result<()> {
    let mut resp = ResponseHeader::build(status, Some(3))?;
    resp.insert_header("location", location)?;
    resp.insert_header("content-length", "0")?;
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(Bytes::new()), true).await
}

/// Write a complete HTTP response (headers + body) to the downstream session.
pub async fn write_response(
    session: &mut Session,
    status: u16,
    content_type: &str,
    body: Bytes,
) -> Result<()> {
    let mut resp = ResponseHeader::build(status, Some(3))?;
    resp.insert_header("content-type", content_type)?;
    resp.insert_header("content-length", body.len().to_string())?;
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(body), true).await
}
