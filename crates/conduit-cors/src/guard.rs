use std::sync::atomic::Ordering;

use async_trait::async_trait;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use pingora_core::Result;

use crate::config::CorsConfig;
use crate::cors;

/// Handles CORS preflight (`OPTIONS`) requests and echoes the appropriate headers.
///
/// Returns [`FilterOutcome::Handled`] for preflight so downstream filters and
/// the upstream proxy are never reached (browsers send OPTIONS without credentials).
pub struct CorsPreflight {
    pub cfg: CorsConfig,
    pub is_preflight: bool,
    pub origin: Option<String>,
    /// Security-headers-only set — used for preflight instead of the full
    /// extra-headers set which may include CORS headers already.
    pub sec_headers: Vec<(String, String)>,
}

#[async_trait]
impl RequestFilter for CorsPreflight {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if !self.is_preflight {
            return Ok(FilterOutcome::Continue);
        }
        let origin = self.origin.as_deref().unwrap_or("");
        let allow_pna = cors::requests_private_network_access(ctx.session);
        cors::handle_preflight(ctx.session, &self.cfg, origin, &self.sec_headers, allow_pna)
            .await?;
        ctx.inflight.fetch_sub(1, Ordering::Relaxed);
        Ok(FilterOutcome::Handled)
    }
}
