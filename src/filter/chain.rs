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
    ApiKeyConfig, BasicAuthConfig, CorsConfig, FaultInjectionConfig, IpFilterConfig, JwtAuthConfig,
    LimitsConfig, MiddlewareEntry, RateLimitConfig,
};
use crate::filter::rate_limit::RateLimiter;
use crate::filter::rate_limit_redis::RedisRateLimiter;
use crate::filter::{auth, cors, ip_filter, jwt, limits, rate_limit, script};
use crate::handler::response;
use uuid::Uuid;

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

/// Ensures every request carries an `X-Request-ID` header.
///
/// If the client already sends the header, its value is forwarded unchanged so
/// that distributed tracing IDs flow through the proxy.  When the header is
/// absent a new UUID v4 is generated and injected before the request reaches
/// upstream.  The ID is also stored as a request attribute so access-log
/// formatters and downstream handlers can include it.
pub struct XRequestIdGuard;

#[async_trait]
impl RequestFilter for XRequestIdGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        let has = ctx
            .session
            .req_header()
            .headers
            .contains_key("x-request-id");
        if !has {
            let id = Uuid::new_v4().to_string();
            let _ = ctx
                .session
                .req_header_mut()
                .insert_header("x-request-id", &id);
        }
        Ok(FilterOutcome::Continue)
    }
}

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
        // Inflight request cap — checked before body/header limits so the
        // rejection cost is minimal when the server is under heavy load.
        if let Some(max) = self.cfg.max_inflight_requests {
            // The inflight counter was already incremented at the start of
            // request_filter, so the current value includes this request.
            let current = ctx.inflight.load(Ordering::Relaxed) as u64;
            if current > max {
                response::write_response(
                    ctx.session,
                    503,
                    "text/plain",
                    Bytes::from_static(b"Service Unavailable (too many concurrent requests)"),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

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

/// JWT bearer-token authentication guard.
///
/// Validates the `Authorization: Bearer <token>` header using either an HMAC
/// secret (`jwtAuth.secret`) or a remote JWKS endpoint (`jwtAuth.jwksUrl`).
/// Returns `401 Unauthorized` when the token is absent or invalid.
pub struct JwtGuard {
    pub cfg: JwtAuthConfig,
    pub path: String,
}

#[async_trait]
impl RequestFilter for JwtGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        let auth_header = ctx
            .session
            .req_header()
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok());

        match jwt::check_jwt(&self.cfg, &self.path, auth_header) {
            jwt::JwtCheckResult::Allowed => Ok(FilterOutcome::Continue),
            jwt::JwtCheckResult::Denied { reason } => {
                tracing::debug!(reason, "JWT validation denied");
                response::write_denied(ctx.session, Some("Bearer"), ctx.extra_headers).await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(FilterOutcome::Handled)
            }
        }
    }
}

/// Injects artificial faults (aborts or delays) for chaos-engineering and
/// testing retry/circuit-breaker behaviour.
///
/// **Should not be used in production.**  Use it in staging or test
/// environments to validate that your clients handle upstream failures
/// gracefully.
pub struct FaultInjectionGuard {
    pub cfg: FaultInjectionConfig,
}

#[async_trait]
impl RequestFilter for FaultInjectionGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Use a simple pseudo-random roll based on the current time nanoseconds.
        // Good enough for percentage-based fault injection; not cryptographically
        // random, but that's not required here.
        let roll: f64 = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as f64;
            (ns % 10_000.0) / 100.0 // 0.0 – 99.99
        };

        // Abort injection — checked first.
        if let Some(ref abort) = self.cfg.abort {
            if roll < abort.percent {
                let status = abort.status.unwrap_or(503).clamp(100, 999);
                let body = abort
                    .body
                    .clone()
                    .unwrap_or_else(|| "Fault injected".to_owned());
                response::write_response(
                    ctx.session,
                    status,
                    "text/plain",
                    Bytes::from(body),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

        // Delay injection.
        if let Some(ref delay) = self.cfg.delay {
            if roll < delay.percent {
                tokio::time::sleep(std::time::Duration::from_millis(delay.ms)).await;
            }
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

/// Executes all middleware entries in the order they appear in `site.middleware`.
///
/// Dispatches by `entry.r#type`:
/// - `"script"` → Rhai scripting (always available)
/// - `"wasm"`   → WASM plugin (requires `--features wasm`; skipped with a warning if disabled)
/// - other      → skipped (unknown types are rejected at config validation time)
///
/// This struct replaces the former `ScriptGuard` so that Rhai and WASM entries
/// interleave freely in the declared order.
pub struct MiddlewareGuard {
    pub middleware: Vec<MiddlewareEntry>,
    /// Request path forwarded to scripts/plugins.
    pub req_path: String,
    pub method: String,
    pub query: String,
    /// Lower-cased header map forwarded to scripts/plugins.
    pub headers: std::collections::HashMap<String, String>,
    /// Remote client IP (used by WASM plugins).
    pub client_ip: String,
}

/// Backward-compatible type alias — existing code that names `ScriptGuard`
/// still compiles.  New code should use `MiddlewareGuard` directly.
pub type ScriptGuard = MiddlewareGuard;

#[async_trait]
impl RequestFilter for MiddlewareGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        for entry in &self.middleware {
            match entry.r#type.as_str() {
                // ── Rhai scripting ────────────────────────────────────────────
                "script" => {
                    let Some(ref path) = entry.path else { continue };
                    match script::run_script(
                        path,
                        &self.req_path,
                        &self.method,
                        &self.query,
                        self.headers.clone(),
                    ) {
                        script::ScriptOutcome::Continue => {}
                        script::ScriptOutcome::Abort {
                            status,
                            body,
                            extra_headers,
                        } => {
                            let mut all = ctx.extra_headers.to_vec();
                            all.extend(extra_headers);
                            response::write_response(
                                ctx.session,
                                status,
                                "text/plain",
                                Bytes::from(body),
                                &all,
                            )
                            .await?;
                            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                            return Ok(FilterOutcome::Handled);
                        }
                    }
                }

                // ── WASM plugins ──────────────────────────────────────────────
                #[cfg(feature = "wasm")]
                "wasm" => {
                    let Some(ref path) = entry.path else { continue };
                    let plugin_config = entry
                        .config
                        .as_ref()
                        .and_then(|v| serde_json::to_vec(v).ok())
                        .unwrap_or_default();

                    let request = crate::filter::wasm::WasmRequest {
                        method: self.method.clone(),
                        path: self.req_path.clone(),
                        query: self.query.clone(),
                        client_ip: self.client_ip.clone(),
                        headers: self.headers.clone(),
                        plugin_config,
                    };

                    match crate::filter::wasm::run_wasm(request, path) {
                        crate::filter::wasm::WasmOutcome::Continue {
                            added_headers,
                            removed_headers,
                        } => {
                            // Apply requested header mutations to the session.
                            for (name, val) in added_headers {
                                let _ = ctx.session.req_header_mut().insert_header(name, val);
                            }
                            for name in removed_headers {
                                ctx.session.req_header_mut().remove_header(&name);
                            }
                        }
                        crate::filter::wasm::WasmOutcome::Abort {
                            status,
                            body,
                            headers,
                        } => {
                            let mut all = ctx.extra_headers.to_vec();
                            all.extend(headers);
                            response::write_response(
                                ctx.session,
                                status,
                                "application/octet-stream",
                                body,
                                &all,
                            )
                            .await?;
                            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                            return Ok(FilterOutcome::Handled);
                        }
                    }
                }

                // ── Feature disabled: warn and skip ───────────────────────────
                #[cfg(not(feature = "wasm"))]
                "wasm" => {
                    tracing::warn!(
                        path = entry.path.as_deref().unwrap_or("<none>"),
                        "WASM middleware entry ignored — rebuild with --features wasm"
                    );
                }

                // ── Unknown types (rejected at validation time) ────────────────
                _ => {}
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
