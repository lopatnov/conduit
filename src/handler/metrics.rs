use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use prometheus::{Encoder, TextEncoder};

use crate::handler::LocalHandlerImpl;

/// Handler struct for Prometheus metrics responses.
pub struct MetricsHandler {
    pub token: Option<String>,
    pub extra_headers: Vec<(String, String)>,
}

#[async_trait]
impl LocalHandlerImpl for MetricsHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        handle_metrics(session, self.token.as_deref(), &self.extra_headers).await
    }
}

/// Serve the Prometheus metrics endpoint.
///
/// If `token` is `Some(tok)`, the request must carry
/// `Authorization: Bearer <tok>`; otherwise 401 is returned.
pub async fn handle_metrics(
    session: &mut Session,
    token: Option<&str>,
    extra: &[(String, String)],
) -> Result<()> {
    if let Some(tok) = token {
        let provided = session
            .req_header()
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if provided != tok {
            let mut resp = ResponseHeader::build(401, Some(2 + extra.len()))?;
            resp.insert_header("content-length", "0")?;
            resp.insert_header("www-authenticate", "Bearer")?;
            for (k, v) in extra {
                resp.insert_header(k.clone(), v.clone())?;
            }
            session.write_response_header(Box::new(resp), false).await?;
            return session.write_response_body(Some(Bytes::new()), true).await;
        }
    }

    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut output = Vec::new();
    if let Err(e) = encoder.encode(&metric_families, &mut output) {
        let msg = format!("metrics encoding error: {e}");
        let body = bytes::Bytes::from(msg);
        let mut resp = ResponseHeader::build(500, Some(2 + extra.len()))?;
        resp.insert_header("content-type", "text/plain")?;
        resp.insert_header("content-length", body.len().to_string())?;
        for (k, v) in extra {
            resp.insert_header(k.clone(), v.clone())?;
        }
        session.write_response_header(Box::new(resp), false).await?;
        return session.write_response_body(Some(body), true).await;
    }

    let mut resp = ResponseHeader::build(200, Some(2 + extra.len()))?;
    resp.insert_header("content-type", encoder.format_type())?;
    resp.insert_header("content-length", output.len().to_string())?;
    for (k, v) in extra {
        resp.insert_header(k.clone(), v.clone())?;
    }
    session.write_response_header(Box::new(resp), false).await?;
    session
        .write_response_body(Some(Bytes::from(output)), true)
        .await
}
