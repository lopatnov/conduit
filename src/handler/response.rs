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
pub async fn write_denied(
    session: &mut Session,
    www_authenticate: Option<&str>,
    extra: &[(String, String)],
) -> Result<()> {
    let mut resp = ResponseHeader::build(401, Some(2 + extra.len()))?;
    resp.insert_header("content-length", "0")?;
    if let Some(challenge) = www_authenticate {
        resp.insert_header("www-authenticate", challenge)?;
    }
    insert_extra(&mut resp, extra)?;
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(Bytes::new()), true).await
}

/// Write a redirect response (3xx + Location header, empty body).
pub async fn write_redirect(
    session: &mut Session,
    status: u16,
    location: &str,
    extra: &[(String, String)],
) -> Result<()> {
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
