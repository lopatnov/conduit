use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use conduit_core::handler::response;
use pingora_core::Result;

use crate::config::IpFilterConfig;
use crate::ip_filter;

/// Rejects requests whose client IP is not in the allow-list / is in the deny-list.
pub struct IpGuard {
    pub cfg: IpFilterConfig,
    /// Runtime deny-list managed via Admin API (`POST /ip-deny` / `DELETE /ip-deny`).
    /// Checked in addition to `ipFilter.deny` from the static config.
    pub dynamic_deny: Arc<std::sync::RwLock<Vec<String>>>,
}

#[async_trait]
impl RequestFilter for IpGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Fast path: no static rules and no dynamic denies — nothing to check.
        // Recover from lock poisoning here too (not just in is_dynamic_denied
        // below) — otherwise a poisoned lock with no static rules configured
        // short-circuits to Continue before is_dynamic_denied's own recovery
        // logic is ever reached, silently disabling the whole dynamic deny
        // list for exactly the sites relying on it most (dynamic-only setups).
        let has_static = self.cfg.allow.is_some() || self.cfg.deny.is_some();
        let has_dynamic = self
            .dynamic_deny
            .read()
            .map(|l| !l.is_empty())
            .unwrap_or_else(|e| !e.into_inner().is_empty());
        if !has_static && !has_dynamic {
            return Ok(FilterOutcome::Continue);
        }

        let blocked =
            !ip_filter::is_allowed(&self.cfg, ctx.session) || self.is_dynamic_denied(ctx.session);
        if blocked {
            // Dry-run mode (nginx `limit_conn_module dry_run` pattern):
            // log the violation but allow the request through.
            if self.cfg.dry_run.unwrap_or(false) {
                // Use the same trust_proxy-aware resolution as the actual
                // filtering decision above — otherwise the logged IP can
                // diverge from the IP that was actually evaluated (e.g. the
                // direct TCP peer instead of the trusted XFF entry).
                let trust_proxy = self.cfg.trust_proxy.unwrap_or(false);
                let client_ip = ip_filter::client_ip_for_check(ctx.session, trust_proxy)
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                tracing::warn!(
                    ip = %client_ip,
                    "[dry-run] IP filter blocked — request allowed through (dryRun: true)"
                );
                return Ok(FilterOutcome::Continue);
            }
            response::write_response(
                ctx.session,
                403,
                "text/plain",
                Bytes::from_static(b"Forbidden"),
                ctx.extra_headers,
            )
            .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }
        Ok(FilterOutcome::Continue)
    }
}

impl IpGuard {
    /// Returns `true` when the client IP matches any entry in `dynamic_deny`.
    ///
    /// Holds the read lock only for the duration of the check — avoids the
    /// previous `deny_list.clone()` that allocated a full Vec per request.
    fn is_dynamic_denied(&self, session: &pingora_proxy::Session) -> bool {
        // Recover from lock poisoning instead of fail-open — a panic while
        // another request held the write lock (e.g. inside the Admin API's
        // POST/DELETE /ip-deny handler) must not silently disable the whole
        // dynamic deny list for every subsequent request. Matches the
        // recovery pattern already used on the admin write-side.
        let deny_list = self.dynamic_deny.read().unwrap_or_else(|e| e.into_inner());
        if deny_list.is_empty() {
            return false;
        }
        // Use apply_ip_filter directly while holding the read lock so we avoid
        // cloning the deny list into a new IpFilterConfig on every request.
        let trust_proxy = self.cfg.trust_proxy.unwrap_or(false);
        let client_ip = ip_filter::client_ip_for_check(session, trust_proxy);
        ip_filter::is_in_deny_list(client_ip, &deny_list)
    }
}
