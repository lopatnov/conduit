use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::config::schema::{FallbackConfig, SiteConfig};
use crate::handler::response::write_response;

pub async fn handle_fallback(session: &mut Session, site: Option<&SiteConfig>) -> Result<()> {
    if let Some(site) = site {
        if let Some(fb) = &site.fallback {
            return handle_configured(session, fb).await;
        }
    }
    write_response(session, 404, "text/plain", Bytes::from_static(b"Not Found")).await
}

async fn handle_configured(session: &mut Session, fb: &FallbackConfig) -> Result<()> {
    let status = fb.status.unwrap_or(404);
    let body = if let Some(b) = &fb.body {
        Bytes::from(b.to_string())
    } else {
        Bytes::from_static(b"Not Found")
    };
    let ct = if fb.body.is_some() {
        "application/json"
    } else {
        "text/plain"
    };
    write_response(session, status, ct, body).await
}
