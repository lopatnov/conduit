use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use prometheus::{Encoder, TextEncoder};
use subtle::ConstantTimeEq as _;

#[cfg(feature = "compression")]
use crate::filter::compression::CompressOptions;
use crate::handler::LocalHandlerImpl;
#[cfg(feature = "compression")]
use crate::proxy::ctx::AcceptEncoding;

/// Handler struct for Prometheus metrics responses.
pub struct MetricsHandler {
    pub token: Option<String>,
    pub extra_headers: Vec<(String, String)>,
    /// Resolved on-the-fly compression options for this site, if the
    /// `compression` feature is compiled in and enabled — see
    /// `crate::filter::compression`'s doc comment.
    #[cfg(feature = "compression")]
    pub compress_opts: Option<CompressOptions>,
    #[cfg(feature = "compression")]
    pub accept_enc: AcceptEncoding,
}

#[async_trait]
impl LocalHandlerImpl for MetricsHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        handle_metrics(
            session,
            self.token.as_deref(),
            &self.extra_headers,
            #[cfg(feature = "compression")]
            self.compress_opts.as_ref().map(|o| (o, &self.accept_enc)),
            #[cfg(not(feature = "compression"))]
            None,
        )
        .await
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
    #[cfg(feature = "compression")] compress: Option<(&CompressOptions, &AcceptEncoding)>,
    #[cfg(not(feature = "compression"))] _compress: Option<()>,
) -> Result<()> {
    if let Some(tok) = token {
        let provided = session
            .req_header()
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        // Constant-time comparison — same reasoning as admin API token.
        let ok = provided.len() == tok.len() && provided.as_bytes().ct_eq(tok.as_bytes()).into();
        if !ok {
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

    #[cfg(feature = "compression")]
    let (body, enc_header) = crate::filter::compression::compress_small_body(
        Bytes::from(output),
        encoder.format_type(),
        compress,
    )
    .await;
    #[cfg(not(feature = "compression"))]
    let (body, enc_header): (Bytes, Option<(String, String)>) = (Bytes::from(output), None);

    let mut all_extra = extra.to_vec();
    if let Some(pair) = enc_header {
        all_extra.push(pair);
        // Response representation depends on Accept-Encoding — same
        // convention as the static-file compression path.
        all_extra.push(("vary".to_owned(), "accept-encoding".to_owned()));
    }
    let extra = all_extra.as_slice();

    let mut resp = ResponseHeader::build(200, Some(2 + extra.len()))?;
    resp.insert_header("content-type", encoder.format_type())?;
    resp.insert_header("content-length", body.len().to_string())?;
    for (k, v) in extra {
        resp.insert_header(k.clone(), v.clone())?;
    }
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(body), true).await
}
