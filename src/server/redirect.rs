use async_trait::async_trait;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::{ProxyHttp, Session};

/// A minimal Pingora proxy that redirects every HTTP request to HTTPS.
///
/// Configured with the HTTPS port so that it can build the redirect URL.
/// Returns `308 Permanent Redirect` to preserve the request method.
pub struct RedirectProxy {
    https_port: u16,
}

impl RedirectProxy {
    pub fn new(https_port: u16) -> Self {
        Self { https_port }
    }
}

#[async_trait]
impl ProxyHttp for RedirectProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn request_filter(&self, session: &mut Session, _ctx: &mut ()) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        // Extract the host (without port), handling IPv6 bracketed addresses.
        let host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .map(|h| {
                if h.starts_with('[') {
                    // IPv6: "[::1]:8080" or "[::1]" → keep "[::1]"
                    h.split(']')
                        .next()
                        .map(|s| format!("{s}]"))
                        .unwrap_or_else(|| h.to_owned())
                } else {
                    h.split(':').next().unwrap_or(h).to_owned()
                }
            })
            .unwrap_or_default();

        // Preserve path and query string.
        let path_and_query = session
            .req_header()
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let location = if self.https_port == 443 {
            format!("https://{host}{path_and_query}")
        } else {
            format!("https://{host}:{}{path_and_query}", self.https_port)
        };

        let mut resp = ResponseHeader::build(308, Some(2))?;
        resp.insert_header("Location", location)?;
        resp.insert_header("Content-Length", "0")?;
        session.write_response_header(Box::new(resp), true).await?;
        Ok(true)
    }

    /// Never called — all requests are handled in `request_filter`.
    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut ()) -> Result<Box<HttpPeer>>
    where
        Self::CTX: Send + Sync,
    {
        unreachable!("RedirectProxy always handles requests in request_filter")
    }
}
