use std::sync::atomic::Ordering;

use async_trait::async_trait;
use bytes::Bytes;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use conduit_core::handler::response;
use pingora_core::Result;

use crate::config::SecurityHeadersConfig;
use crate::security_headers;

/// Validates the `Host` request header against a configured allowlist.
///
/// Runs immediately after `HealthBypass` so health/ACME/hot-reload endpoints
/// are always reachable regardless of the allowlist. All other requests with
/// a disallowed Host receive `400 Bad Request`.
///
/// Pattern from traefik `AllowedHosts` — prevents HTTP Host header injection
/// where an application generates absolute URLs from an untrusted Host header.
///
/// When `securityHeaders.allowedHosts` is not explicitly configured, falls
/// back to the site's own `host:` value (`site_host`) so this protection
/// applies by default, not just when opted into.
pub struct AllowedHostsGuard {
    pub security_cfg: Option<SecurityHeadersConfig>,
    pub site_host: Option<String>,
    pub host: String,
}

#[async_trait]
impl RequestFilter for AllowedHostsGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if security_headers::is_host_allowed(
            self.security_cfg.as_ref(),
            self.site_host.as_deref(),
            &self.host,
        ) {
            return Ok(FilterOutcome::Continue);
        }
        response::write_response(
            ctx.session,
            400,
            "text/plain",
            Bytes::from_static(b"Bad Request: host not allowed"),
            ctx.extra_headers,
        )
        .await?;
        ctx.inflight.fetch_sub(1, Ordering::Relaxed);
        Ok(FilterOutcome::Handled)
    }
}
