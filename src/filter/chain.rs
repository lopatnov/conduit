//! Chain of Responsibility pattern for request guard filters.
//!
//! Each guard filter checks one concern (IP allow/deny, rate limiting, auth,
//! etc.) and returns a [`FilterOutcome`] that tells the chain whether to
//! continue, stop with a handled response, or bypass all remaining guards.
//!
//! ## Adding a new filter
//!
//! 1. Create a struct that holds the filter's configuration.
//! 2. `impl RequestFilter for YourFilter { async fn apply(...) ... }`.
//! 3. Push it into the `FilterChain` at the right position in
//!    `FilterChain::from_guard_ctx` (or whatever builder is appropriate).
//!
//! No changes to `service.rs` are required.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::config::schema::{
    ApiKeyConfig, BasicAuthConfig, CorsConfig, IpFilterConfig, LimitsConfig, MiddlewareEntry,
    RateLimitConfig,
};
use crate::filter::rate_limit::RateLimiter;
use crate::filter::rate_limit_redis::RedisRateLimiter;
use crate::filter::{auth, cors, ip_filter, limits, rate_limit, script};
use crate::handler::response;

// ── Outcome ───────────────────────────────────────────────────────────────────

/// What a filter returns after inspecting a request.
pub enum FilterOutcome {
    /// Pass the request to the next filter in the chain.
    Continue,
    /// The filter wrote a rejection response and decremented the inflight
    /// counter; the chain should stop and return `Ok(true)` to Pingora.
    Handled,
    /// Skip all remaining guard filters and let the normal dispatch proceed.
    /// Used by the health / ACME / hot-reload bypass guard.
    Bypass,
}

// ── Context ───────────────────────────────────────────────────────────────────

/// Data shared by every filter in a single request's guard chain.
pub struct FilterContext<'a> {
    /// The live HTTP session — filters may read headers and write responses.
    pub session: &'a mut Session,
    /// Headers to attach to any rejection response (CORS, security, custom).
    pub extra_headers: &'a [(String, String)],
    /// Inflight counter; each filter that writes a rejection response
    /// decrements it.
    pub inflight: &'a AtomicUsize,
    /// In-memory rate-limit token buckets.
    pub rate_limiter: &'a RateLimiter,
    /// Optional Redis-backed rate limiter (may be `None` at startup).
    pub redis_rate_limiter: Option<&'a Arc<RedisRateLimiter>>,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A single guard filter in the request pipeline.
#[async_trait]
pub trait RequestFilter: Send + Sync {
    /// Inspect the request and decide whether to continue, reject, or bypass.
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome>;
}

// ── Chain ─────────────────────────────────────────────────────────────────────

/// An ordered list of request guard filters.
///
/// Filters are evaluated in insertion order; the first [`FilterOutcome::Handled`]
/// or [`FilterOutcome::Bypass`] stops the chain.
#[derive(Default)]
pub struct FilterChain {
    filters: Vec<Box<dyn RequestFilter>>,
}

impl FilterChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a filter to the end of the chain.
    pub fn push(mut self, f: impl RequestFilter + 'static) -> Self {
        self.filters.push(Box::new(f));
        self
    }

    /// Run every filter in order, stopping on the first non-Continue outcome.
    ///
    /// Returns `Ok(true)` when a filter handled the request (response written),
    /// `Ok(false)` when all filters passed (or the chain was bypassed).
    pub async fn run<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<bool> {
        for filter in &self.filters {
            match filter.apply(ctx).await? {
                FilterOutcome::Continue => continue,
                FilterOutcome::Handled => return Ok(true),
                FilterOutcome::Bypass => return Ok(false),
            }
        }
        Ok(false)
    }
}

// ── Concrete guards ───────────────────────────────────────────────────────────

/// Rejects requests whose client IP is not in the allow-list / is in the deny-list.
pub struct IpGuard {
    pub cfg: IpFilterConfig,
}

#[async_trait]
impl RequestFilter for IpGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if !ip_filter::is_allowed(&self.cfg, ctx.session) {
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
        cors::handle_preflight(ctx.session, &self.cfg, origin, &self.sec_headers).await?;
        ctx.inflight.fetch_sub(1, Ordering::Relaxed);
        Ok(FilterOutcome::Handled)
    }
}

/// Bypasses all remaining guard filters for health, ACME challenge, and
/// hot-reload endpoints — they must always be reachable.
pub struct HealthBypass {
    pub bypass: bool,
}

#[async_trait]
impl RequestFilter for HealthBypass {
    async fn apply<'a>(&self, _ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if self.bypass {
            Ok(FilterOutcome::Bypass)
        } else {
            Ok(FilterOutcome::Continue)
        }
    }
}

/// Enforces request body and header size limits.
pub struct LimitsGuard {
    pub cfg: LimitsConfig,
}

#[async_trait]
impl RequestFilter for LimitsGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if let Some((status, body)) = limits_rejection(limits::check(&self.cfg, ctx.session)) {
            response::write_response(ctx.session, status, "text/plain", body, ctx.extra_headers)
                .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }
        Ok(FilterOutcome::Continue)
    }
}

/// Token-bucket rate limiter; falls back to in-memory when Redis is unavailable.
pub struct RateLimitGuard {
    pub cfg: RateLimitConfig,
}

#[async_trait]
impl RequestFilter for RateLimitGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        let allowed = rate_limit_allowed(
            &self.cfg,
            ctx.session,
            ctx.rate_limiter,
            ctx.redis_rate_limiter,
        )
        .await;
        if !allowed {
            response::write_response(
                ctx.session,
                429,
                "text/plain",
                Bytes::from_static(b"Too Many Requests"),
                ctx.extra_headers,
            )
            .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }
        Ok(FilterOutcome::Continue)
    }
}

/// HTTP Basic Authentication guard.
pub struct BasicAuthGuard {
    pub cfg: BasicAuthConfig,
}

#[async_trait]
impl RequestFilter for BasicAuthGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if let auth::BasicAuthResult::Denied { challenge, realm } =
            auth::check_basic_auth(&self.cfg, ctx.session)
        {
            let www_auth = challenge.then(|| format!("Basic realm=\"{realm}\""));
            response::write_denied(ctx.session, www_auth.as_deref(), ctx.extra_headers).await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }
        Ok(FilterOutcome::Continue)
    }
}

/// API-key authentication guard.
pub struct ApiKeyGuard {
    pub cfg: ApiKeyConfig,
}

#[async_trait]
impl RequestFilter for ApiKeyGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if !auth::check_api_key(&self.cfg, ctx.session) {
            response::write_denied(ctx.session, None, ctx.extra_headers).await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }
        Ok(FilterOutcome::Continue)
    }
}

/// Handles configured URL redirects (301/302/307/308).
pub struct RedirectGuard {
    /// Pre-computed redirect target from the redirect rules, if any matched.
    pub result: Option<(String, u16)>,
}

#[async_trait]
impl RequestFilter for RedirectGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        if let Some((ref location, status)) = self.result {
            response::write_redirect(ctx.session, status, location, ctx.extra_headers).await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }
        Ok(FilterOutcome::Continue)
    }
}

/// Runs Rhai script middleware entries (`type: "script"` in `site.middleware`).
pub struct ScriptGuard {
    pub middleware: Vec<MiddlewareEntry>,
    pub script_path: String,
    pub script_method: String,
    pub script_query: String,
    pub script_headers: std::collections::HashMap<String, String>,
}

#[async_trait]
impl RequestFilter for ScriptGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        for entry in &self.middleware {
            if entry.r#type != "script" {
                continue;
            }
            let Some(ref script_path) = entry.path else {
                continue;
            };
            match script::run_script(
                script_path,
                &self.script_path,
                &self.script_method,
                &self.script_query,
                self.script_headers.clone(),
            ) {
                script::ScriptOutcome::Continue => {}
                script::ScriptOutcome::Abort {
                    status,
                    body,
                    extra_headers,
                } => {
                    let mut all_headers = ctx.extra_headers.to_vec();
                    all_headers.extend(extra_headers);
                    response::write_response(
                        ctx.session,
                        status,
                        "text/plain",
                        Bytes::from(body),
                        &all_headers,
                    )
                    .await?;
                    ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                    return Ok(FilterOutcome::Handled);
                }
            }
        }
        Ok(FilterOutcome::Continue)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Check the rate limiter: Redis if configured and available, otherwise memory.
async fn rate_limit_allowed(
    cfg: &RateLimitConfig,
    session: &mut Session,
    rate_limiter: &RateLimiter,
    redis: Option<&Arc<RedisRateLimiter>>,
) -> bool {
    if cfg
        .store
        .as_deref()
        .is_some_and(|s| s.starts_with("redis://") || s.starts_with("rediss://"))
    {
        if let Some(rrl) = redis {
            let key = rate_limit::extract_client_key(cfg, session);
            return rrl.check(&key, cfg.limit, cfg.window_secs).await;
        }
    }
    rate_limit::check(cfg, session, rate_limiter)
}

/// Map a `limits::CheckResult` to the HTTP rejection status + body, or `None`
/// when the request is within the configured limits.
fn limits_rejection(result: limits::CheckResult) -> Option<(u16, Bytes)> {
    match result {
        limits::CheckResult::BodyTooLarge => {
            Some((413, Bytes::from_static(b"Request Entity Too Large")))
        }
        limits::CheckResult::HeaderTooLarge => {
            Some((431, Bytes::from_static(b"Request Header Fields Too Large")))
        }
        limits::CheckResult::Ok => None,
    }
}
