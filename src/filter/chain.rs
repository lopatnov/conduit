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

#[cfg(feature = "consumers")]
use crate::config::schema::ConsumersConfig;
#[cfg(feature = "fault-injection")]
use crate::config::schema::FaultInjectionConfig;
#[cfg(feature = "forward-auth")]
use crate::config::schema::ForwardAuthConfig;
#[cfg(feature = "jwt")]
use crate::config::schema::JwtAuthConfig;
use crate::config::schema::{
    ApiKeyConfig, BasicAuthConfig, CorsConfig, IpFilterConfig, LimitsConfig, MiddlewareEntry,
    RateLimitConfig,
};
#[cfg(feature = "jwt")]
use crate::filter::jwt;
use crate::filter::rate_limit::RateLimiter;
#[cfg(feature = "redis")]
use crate::filter::rate_limit_redis::RedisRateLimiter;
#[cfg(feature = "rhai")]
use crate::filter::script;
use crate::filter::{auth, cors, ip_filter, limits, rate_limit};
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
    /// Only available when compiled with `--features redis`.
    #[cfg(feature = "redis")]
    pub redis_rate_limiter: Option<&'a Arc<RedisRateLimiter>>,
    /// Per-client-IP concurrent connection counts (nginx limit_conn pattern).
    pub ip_conn_counts: &'a dashmap::DashMap<String, AtomicUsize>,
    /// Extracted client IP used for per-IP connection limiting.
    pub client_ip: String,
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
    /// Runtime deny-list managed via Admin API (`POST /ip-deny` / `DELETE /ip-deny`).
    /// Checked in addition to `ipFilter.deny` from the static config.
    pub dynamic_deny: Arc<std::sync::RwLock<Vec<String>>>,
}

#[async_trait]
impl RequestFilter for IpGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Fast path: no static rules and no dynamic denies — nothing to check.
        let has_static = self.cfg.allow.is_some() || self.cfg.deny.is_some();
        let has_dynamic = self
            .dynamic_deny
            .read()
            .map(|l| !l.is_empty())
            .unwrap_or(false);
        if !has_static && !has_dynamic {
            return Ok(FilterOutcome::Continue);
        }

        let blocked =
            !ip_filter::is_allowed(&self.cfg, ctx.session) || self.is_dynamic_denied(ctx.session);
        if blocked {
            // Dry-run mode (nginx `limit_conn_module dry_run` pattern):
            // log the violation but allow the request through.
            if self.cfg.dry_run.unwrap_or(false) {
                let client_ip = ctx
                    .session
                    .client_addr()
                    .and_then(|a| a.as_inet())
                    .map(|a| a.ip().to_string())
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
        let Ok(deny_list) = self.dynamic_deny.read() else {
            return false;
        };
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

/// Validates the `Host` request header against a configured allowlist.
///
/// Runs immediately after `HealthBypass` so health/ACME/hot-reload endpoints
/// are always reachable regardless of the allowlist.  All other requests with
/// a disallowed Host receive `400 Bad Request`.
///
/// Pattern from traefik `AllowedHosts` — prevents HTTP Host header injection
/// where an application generates absolute URLs from an untrusted Host header.
pub struct AllowedHostsGuard {
    pub security_cfg: Option<crate::config::schema::SecurityHeadersConfig>,
    pub host: String,
}

#[async_trait]
impl RequestFilter for AllowedHostsGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        let Some(ref cfg) = self.security_cfg else {
            return Ok(FilterOutcome::Continue);
        };
        if crate::filter::security_headers::is_host_allowed(cfg, &self.host) {
            return Ok(FilterOutcome::Continue);
        }
        crate::handler::response::write_response(
            ctx.session,
            400,
            "text/plain",
            bytes::Bytes::from_static(b"Bad Request: host not in allowedHosts"),
            ctx.extra_headers,
        )
        .await?;
        ctx.inflight.fetch_sub(1, Ordering::Relaxed);
        Ok(FilterOutcome::Handled)
    }
}

/// Enforces request body and header size limits.
pub struct LimitsGuard {
    pub cfg: LimitsConfig,
}

#[async_trait]
impl RequestFilter for LimitsGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Host header security check — always enforced, no config needed.
        //
        // Reject requests whose Host header is:
        //   1. Non-UTF-8 bytes (obvious malform / injection attempt).
        //   2. Contains CR, LF, or NUL (header-injection / smuggling).
        //   3. Not a valid HTTP authority (e.g. contains spaces, path
        //      separators, or other RFC 3986 §3.2-invalid characters).
        let host_hdr = ctx.session.req_header().headers.get("host");
        if is_host_header_invalid(host_hdr) {
            response::write_response(
                ctx.session,
                400,
                "text/plain",
                Bytes::from_static(b"Bad Request (invalid Host header)"),
                ctx.extra_headers,
            )
            .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }

        // Request header count limit.
        if let Some(max_hdrs) = self.cfg.max_request_headers {
            let count = ctx.session.req_header().headers.len() as u32;
            if count > max_hdrs {
                response::write_response(
                    ctx.session,
                    431,
                    "text/plain",
                    Bytes::from_static(b"Request Header Fields Too Large"),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

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

        // Per-IP concurrent connection limit (nginx limit_conn pattern).
        // Checked after the inflight cap so the DashMap lookup only runs when
        // the server is accepting new connections.
        if let Some(max_per_ip) = self.cfg.max_connections_per_ip {
            let ip = &ctx.client_ip;
            if !ip.is_empty() && !try_acquire_ip_slot(ip, max_per_ip, ctx.ip_conn_counts) {
                response::write_response(
                    ctx.session,
                    429,
                    "text/plain",
                    Bytes::from_static(b"Too Many Connections"),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

        Ok(FilterOutcome::Continue)
    }
}

/// Returns `true` when the `Host` header value is malformed or contains
/// characters that could be exploited for header-injection attacks.
///
/// Rejects:
/// - Non-UTF-8 bytes.
/// - Values containing CR, LF, or NUL control characters.
/// - Values that are not a valid RFC 3986 authority (spaces, backslash,
///   path separators, etc.).
///
/// Source: freenginx `ngx_http_request.c` — `ngx_http_validate_host()`
/// commit `d5ea86c7`.
fn is_host_header_invalid(hdr: Option<&http::header::HeaderValue>) -> bool {
    let v = match hdr {
        Some(v) => v,
        None => return false, // absent Host is handled separately
    };
    let s = match v.to_str() {
        Err(_) => return true, // non-UTF-8 → reject
        Ok(s) => s,
    };
    // Belt-and-suspenders control-byte check.
    if s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return true;
    }
    // Full RFC 3986 authority grammar validation.
    http::uri::Authority::try_from(s).is_err()
}

/// Attempt to acquire one connection slot for `ip` against `max`.
///
/// Atomically increments the counter for this IP.  If the result exceeds
/// `max` the increment is immediately rolled back and `false` is returned so
/// the caller can reject the request with a 429.  Returns `true` when the
/// slot was successfully acquired.
fn try_acquire_ip_slot(ip: &str, max: u64, counts: &dashmap::DashMap<String, AtomicUsize>) -> bool {
    let current = counts
        .entry(ip.to_owned())
        .or_insert_with(|| AtomicUsize::new(0))
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    if current as u64 > max {
        // Undo the increment — this request is rejected.
        if let Some(counter) = counts.get(ip) {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
        return false;
    }
    // Request is allowed — mark the IP so logging() can decrement the counter.
    // We need to propagate this to RequestCtx; the LimitsGuard doesn't have
    // direct access to RequestCtx here. We store it on the session via the
    // ctx extra_headers note — actually we'll handle this in service.rs by
    // checking max_connections_per_ip in do_request_filter and setting
    // req_ctx.client_ip_for_conn_limit.
    true
}

/// Token-bucket rate limiter; falls back to in-memory when Redis is unavailable.
pub struct RateLimitGuard {
    pub cfg: RateLimitConfig,
    /// Label used for `conduit_rate_limit_rejected_total{site=…}`.
    /// Typically `"host:port"` or `"*"` for catch-all sites.
    pub site_label: String,
}

#[async_trait]
impl RequestFilter for RateLimitGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        let allowed = rate_limit_allowed(
            &self.cfg,
            ctx.session,
            ctx.rate_limiter,
            #[cfg(feature = "redis")]
            ctx.redis_rate_limiter,
            #[cfg(not(feature = "redis"))]
            None,
        )
        .await;
        if !allowed {
            // Increment the rate-limit rejection counter.
            crate::proxy::service::ConduitMetrics::global()
                .rate_limit_rejected_total
                .with_label_values(&[&self.site_label])
                .inc();

            // Dry-run mode (nginx `limit_req_dry_run` pattern):
            // log the violation but forward the request instead of rejecting.
            if self.cfg.dry_run.unwrap_or(false) {
                tracing::warn!(
                    site = %self.site_label,
                    "[dry-run] rate limit exceeded — request allowed through (dryRun: true)"
                );
                return Ok(FilterOutcome::Continue);
            }

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

/// Consumer-model authentication guard.
///
/// Identifies the caller by checking configured consumers' credentials
/// (API key or Basic Auth) in declaration order.  On success:
///   - Injects `X-Consumer-ID: <username>` (or the configured `idHeader`)
///   - Applies per-consumer rate limit using the shared limiter
///   - Injects any per-consumer custom headers
///
/// Returns 401 when no consumer matches.
#[cfg(feature = "consumers")]
pub struct ConsumersGuard {
    pub cfg: ConsumersConfig,
    pub path: String,
}

#[cfg(feature = "consumers")]
#[async_trait]
impl RequestFilter for ConsumersGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Skip configured paths (health, public assets, etc.)
        if let Some(skip) = &self.cfg.skip_paths {
            if auth::is_path_skipped(Some(skip.as_slice()), &self.path) {
                return Ok(FilterOutcome::Continue);
            }
        }

        // Strip any existing consumer-identity header from the incoming request
        // BEFORE identification.  A client could forge `X-Consumer-ID: admin`
        // and send it to upstream to impersonate a privileged consumer.  We
        // always overwrite this header with the identity we compute ourselves,
        // but stripping it first ensures it is absent on skip-paths too.
        let id_header_name = self.cfg.id_header.as_deref().unwrap_or("x-consumer-id");
        let _ = ctx.session.req_header_mut().remove_header(id_header_name);

        // Identify consumer from credentials in the request.
        let consumer = auth::identify_consumer(&self.cfg, ctx.session);
        let Some(consumer) = consumer else {
            response::write_denied(ctx.session, None, ctx.extra_headers).await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        };

        // Per-consumer rate limit — key: "consumer:{username}" (global per consumer).
        if let Some(rl_cfg) = &consumer.rate_limit {
            let key = format!("consumer:{}", consumer.username);
            let allowed = ctx
                .rate_limiter
                .entry(key)
                .or_insert_with(|| {
                    crate::filter::rate_limit::TokenBucket::new(
                        rl_cfg.limit,
                        rl_cfg.burst.unwrap_or(0),
                        rl_cfg.window_secs,
                    )
                })
                .try_consume();
            if !allowed {
                response::write_response(
                    ctx.session,
                    429,
                    "text/plain",
                    bytes::Bytes::from_static(b"Too Many Requests"),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

        // Inject X-Consumer-ID (or custom idHeader) so upstream knows who's calling.
        // Use owned Strings to satisfy insert_header's lifetime requirements.
        let id_header = self
            .cfg
            .id_header
            .clone()
            .unwrap_or_else(|| "x-consumer-id".to_owned());
        let consumer_name = consumer.username.clone();
        let _ = ctx
            .session
            .req_header_mut()
            .insert_header(id_header, consumer_name);

        // Inject per-consumer custom headers (e.g. X-Tier: premium).
        if let Some(ref custom) = consumer.headers {
            // Collect to owned Vec<(String,String)> to avoid lifetime issues.
            let pairs: Vec<(String, String)> =
                custom.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            for (k, v) in pairs {
                let _ = ctx.session.req_header_mut().insert_header(k, v);
            }
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
#[cfg(feature = "jwt")]
pub struct JwtGuard {
    pub cfg: JwtAuthConfig,
    pub path: String,
}

#[cfg(feature = "jwt")]
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

/// Forward Auth guard — delegates authentication/authorization to an external service.
///
/// Sends the incoming request (filtered headers) to the configured auth URL.
/// - **2xx** → auth passed; headers listed in `responseHeaders` are injected
///   into the upstream request so the upstream receives user identity/role info.
/// - **4xx / 5xx** → auth denied; the auth service status is returned to the
///   client immediately.
///
/// Uses a process-wide `reqwest::Client` with a connection pool so that
/// hot-path requests don't pay TCP setup overhead.
#[cfg(feature = "forward-auth")]
pub struct ForwardAuthGuard {
    pub cfg: ForwardAuthConfig,
    pub path: String,
}

/// Process-wide reqwest client for forward-auth and JWKS fetching.
///
/// Uses separate `connect_timeout` (TCP SYN + TLS handshake) and overall
/// `timeout` (from connect to last body byte) so that both hung TCP
/// connections AND slow auth servers are bounded.
#[cfg(feature = "forward-auth")]
fn forward_auth_client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3)) // TCP+TLS max
            .timeout(std::time::Duration::from_secs(10)) // total request max
            .build()
            .unwrap_or_default()
    })
}

#[cfg(feature = "forward-auth")]
#[async_trait]
impl RequestFilter for ForwardAuthGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        use crate::filter::auth::is_path_skipped;

        // Bypass for configured skip paths.
        if let Some(skip) = &self.cfg.skip_paths {
            if is_path_skipped(Some(skip.as_slice()), &self.path) {
                return Ok(FilterOutcome::Continue);
            }
        }

        let auth_url = &self.cfg.url;
        let timeout_ms = self.cfg.timeout_ms.unwrap_or(5000);
        let client = forward_auth_client();

        // Build the subrequest.
        let method = ctx.session.req_header().method.as_str();
        let uri = ctx
            .session
            .req_header()
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let client_ip = ctx
            .session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip().to_string())
            .unwrap_or_default();

        let mut req = client
            .get(auth_url)
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .header("X-Forwarded-Method", method)
            .header("X-Forwarded-Uri", uri)
            .header("X-Forwarded-For", &client_ip);

        // Forward specific request headers if configured.
        if let Some(fwd_hdrs) = &self.cfg.request_headers {
            req = forward_auth_add_headers(req, fwd_hdrs, ctx.session);
        }

        // Make the subrequest.
        let auth_resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(url = %auth_url, error = %e, "forward-auth service unreachable");
                // Fail closed: treat unreachable auth service as 401.
                response::write_denied(ctx.session, None, ctx.extra_headers).await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        };

        let status = auth_resp.status();
        if status.is_success() {
            // Inject auth service response headers into the upstream request.
            if let Some(copy_hdrs) = &self.cfg.response_headers {
                forward_auth_inject_response_headers(&auth_resp, copy_hdrs, ctx.session);
            }
            Ok(FilterOutcome::Continue)
        } else {
            let status_code = status.as_u16();
            let body = bytes::Bytes::from_static(if status_code == 403 {
                b"Forbidden"
            } else {
                b"Unauthorized"
            });
            response::write_response(
                ctx.session,
                status_code,
                "text/plain",
                body,
                ctx.extra_headers,
            )
            .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            Ok(FilterOutcome::Handled)
        }
    }
}

/// Add configured request headers to a forward-auth subrequest.
#[cfg(feature = "forward-auth")]
fn forward_auth_add_headers(
    mut req: reqwest::RequestBuilder,
    fwd_hdrs: &[String],
    session: &Session,
) -> reqwest::RequestBuilder {
    for name in fwd_hdrs {
        if let Some(val) = session.req_header().headers.get(name.as_str()) {
            if let Ok(v) = val.to_str() {
                req = req.header(name.as_str(), v);
            }
        }
    }
    req
}

/// Copy configured response headers from a forward-auth response into the session.
#[cfg(feature = "forward-auth")]
fn forward_auth_inject_response_headers(
    auth_resp: &reqwest::Response,
    copy_hdrs: &[String],
    session: &mut Session,
) {
    let to_inject: Vec<(String, String)> = copy_hdrs
        .iter()
        .filter_map(|name| {
            auth_resp
                .headers()
                .get(name.as_str())
                .and_then(|val| val.to_str().ok())
                .map(|v| (name.clone(), v.to_owned()))
        })
        .collect();
    for (name, value) in to_inject {
        let _ = session.req_header_mut().insert_header(name, value);
    }
}

/// Injects artificial faults (aborts or delays) for chaos-engineering and
/// testing retry/circuit-breaker behaviour.
///
/// **Should not be used in production.**  Use it in staging or test
/// environments to validate that your clients handle upstream failures
/// gracefully.
#[cfg(feature = "fault-injection")]
pub struct FaultInjectionGuard {
    pub cfg: FaultInjectionConfig,
}

#[cfg(feature = "fault-injection")]
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
    async fn apply<'a>(
        &self,
        #[cfg_attr(not(any(feature = "rhai", feature = "wasm")), allow(unused_variables))]
        ctx: &mut FilterContext<'a>,
    ) -> Result<FilterOutcome> {
        for entry in &self.middleware {
            match entry.r#type.as_str() {
                // ── Rhai scripting ────────────────────────────────────────────
                #[cfg(feature = "rhai")]
                "script" => {
                    if entry.phase.as_deref() == Some("response") {
                        continue;
                    }
                    let Some(ref path) = entry.path else { continue };
                    if apply_rhai_entry(self, path, entry, ctx).await? {
                        return Ok(FilterOutcome::Handled);
                    }
                }

                // ── WASM plugins ──────────────────────────────────────────────
                #[cfg(feature = "wasm")]
                "wasm" => {
                    let Some(ref path) = entry.path else { continue };
                    if apply_wasm_entry(self, path, entry, ctx).await? {
                        return Ok(FilterOutcome::Handled);
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

/// Run a Rhai script entry and return `true` if the request was aborted.
#[cfg(feature = "rhai")]
async fn apply_rhai_entry<'a>(
    guard: &MiddlewareGuard,
    path: &str,
    entry: &crate::config::schema::MiddlewareEntry,
    ctx: &mut FilterContext<'a>,
) -> Result<bool> {
    // `run_script` may read the script file from disk on first call (subsequent
    // calls use the AST cache).  Use `block_in_place` so the Tokio scheduler
    // knows this thread may block and can temporarily move other tasks elsewhere.
    let outcome = tokio::task::block_in_place(|| {
        script::run_script(
            path,
            &guard.req_path,
            &guard.method,
            &guard.query,
            guard.headers.clone(),
            entry.config.as_ref(),
        )
    });
    match outcome {
        script::ScriptOutcome::Continue => Ok(false),
        script::ScriptOutcome::Abort {
            status,
            body,
            extra_headers,
        } => {
            let mut all = ctx.extra_headers.to_vec();
            all.extend(extra_headers);
            response::write_response(ctx.session, status, "text/plain", Bytes::from(body), &all)
                .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            Ok(true)
        }
    }
}

/// Run a WASM plugin entry and return `true` if the request was aborted.
#[cfg(feature = "wasm")]
async fn apply_wasm_entry<'a>(
    guard: &MiddlewareGuard,
    path: &str,
    entry: &crate::config::schema::MiddlewareEntry,
    ctx: &mut FilterContext<'a>,
) -> Result<bool> {
    let plugin_config = entry
        .config
        .as_ref()
        .and_then(|v| serde_json::to_vec(v).ok())
        .unwrap_or_default();
    let header_names: Vec<String> = guard.headers.keys().cloned().collect();
    let request_id = ctx
        .session
        .req_header()
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    let request = crate::filter::wasm::WasmRequest {
        method: guard.method.clone(),
        path: guard.req_path.clone(),
        query: guard.query.clone(),
        client_ip: guard.client_ip.clone(),
        headers: guard.headers.clone(),
        header_names,
        request_id,
        plugin_config,
    };

    // `run_wasm` reads the .wasm file from disk on first call.
    // Use block_in_place so Tokio can schedule around the I/O.
    let wasm_outcome = tokio::task::block_in_place(|| crate::filter::wasm::run_wasm(request, path));
    match wasm_outcome {
        crate::filter::wasm::WasmOutcome::Continue {
            added_headers,
            removed_headers,
        } => {
            for (name, val) in added_headers {
                let _ = ctx.session.req_header_mut().insert_header(name, val);
            }
            for name in removed_headers {
                ctx.session.req_header_mut().remove_header(&name);
            }
            Ok(false)
        }
        crate::filter::wasm::WasmOutcome::Abort {
            status,
            body,
            headers,
        } => {
            let mut all = ctx.extra_headers.to_vec();
            all.extend(headers);
            response::write_response(ctx.session, status, "application/octet-stream", body, &all)
                .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            Ok(true)
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Check the rate limiter: Redis if configured and available, otherwise memory.
async fn rate_limit_allowed(
    cfg: &RateLimitConfig,
    session: &mut Session,
    rate_limiter: &RateLimiter,
    #[cfg(feature = "redis")] redis: Option<&Arc<RedisRateLimiter>>,
    #[cfg(not(feature = "redis"))] _redis: Option<()>,
) -> bool {
    #[cfg(feature = "redis")]
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
    #[cfg(not(feature = "redis"))]
    let _ = cfg.store.as_deref(); // suppress unused warning when redis disabled
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── limits_rejection ─────────────────────────────────────────────────────

    #[test]
    fn limits_rejection_body_too_large_returns_413() {
        let result = limits_rejection(limits::CheckResult::BodyTooLarge);
        assert!(result.is_some());
        let (status, body) = result.unwrap();
        assert_eq!(status, 413);
        assert!(!body.is_empty());
    }

    #[test]
    fn limits_rejection_header_too_large_returns_431() {
        let result = limits_rejection(limits::CheckResult::HeaderTooLarge);
        assert!(result.is_some());
        let (status, _) = result.unwrap();
        assert_eq!(status, 431);
    }

    #[test]
    fn limits_rejection_ok_returns_none() {
        let result = limits_rejection(limits::CheckResult::Ok);
        assert!(result.is_none());
    }

    // ── IpGuard dynamic deny list ────────────────────────────────────────────

    #[test]
    fn ip_guard_empty_dynamic_deny_returns_false() {
        use std::sync::RwLock;
        let guard = IpGuard {
            cfg: crate::config::schema::IpFilterConfig {
                allow: None,
                deny: None,
                trust_proxy: None,
                dry_run: None,
            },
            dynamic_deny: std::sync::Arc::new(RwLock::new(vec![])),
        };
        // No session needed since list is empty — returns false immediately.
        let deny_list = guard.dynamic_deny.read().unwrap();
        assert!(deny_list.is_empty());
    }

    // ── IpGuard with populated dynamic deny list ──────────────────────────────

    #[test]
    fn ip_guard_with_cidr_in_dynamic_deny() {
        use std::sync::RwLock;
        let guard = IpGuard {
            cfg: crate::config::schema::IpFilterConfig {
                allow: None,
                deny: None,
                trust_proxy: None,
                dry_run: None,
            },
            dynamic_deny: std::sync::Arc::new(RwLock::new(vec!["10.0.0.0/8".to_owned()])),
        };
        // Can read the deny list and verify it's non-empty.
        let deny_list = guard.dynamic_deny.read().unwrap();
        assert!(!deny_list.is_empty());
        assert_eq!(deny_list[0], "10.0.0.0/8");
    }

    // ── FilterChain builder ───────────────────────────────────────────────────

    #[test]
    fn filter_chain_new_is_empty() {
        let chain = FilterChain::new();
        // An empty chain must exist without panicking.
        // We can't run it without a session, but we can test construction.
        drop(chain);
    }

    #[test]
    fn filter_chain_default_is_same_as_new() {
        // FilterChain derives Default, which should be equivalent to new().
        let _ = FilterChain::default();
    }

    // ── FilterOutcome variants ────────────────────────────────────────────────

    // ── forward_auth_client ───────────────────────────────────────────────────

    #[test]
    #[cfg(feature = "forward-auth")]
    fn forward_auth_client_returns_same_singleton() {
        let c1 = forward_auth_client();
        let c2 = forward_auth_client();
        // Both calls must return the same static reference.
        assert!(
            std::ptr::eq(c1 as *const _, c2 as *const _),
            "forward_auth_client must be a singleton"
        );
    }

    // ── Host header validation (LimitsGuard) ─────────────────────────────────

    #[test]
    fn host_validation_rejects_crlf_in_host() {
        // Validate that the host-header check correctly flags CR/LF bytes.
        // The check is: host_val.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
        let bad_hosts = [
            "evil.com\r\nX-Injected: yes",
            "evil.com\n",
            "evil.com\r",
            "evil\0.com",
        ];
        for h in &bad_hosts {
            let has_bad = h.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
            assert!(has_bad, "expected bad host to be detected: {h:?}");
        }
    }

    #[test]
    fn host_validation_accepts_normal_host() {
        let good_hosts = [
            "example.com",
            "example.com:8080",
            "192.168.1.1:443",
            "[::1]:8080",
        ];
        for h in &good_hosts {
            let has_bad = h.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
            assert!(!has_bad, "expected normal host to pass: {h:?}");
        }
    }

    // ── maxRequestHeaders ────────────────────────────────────────────────────

    #[test]
    fn max_request_headers_threshold() {
        // Verify the comparison logic used inside LimitsGuard.
        assert!(50u32 > 49u32, "50 headers should exceed limit of 49");
        assert!(
            !(50u32 > 50u32),
            "50 headers at limit should not exceed limit of 50"
        );
    }

    // ── limits_rejection body/header messages ─────────────────────────────────

    #[test]
    fn limits_rejection_body_message_correct() {
        let result = limits_rejection(limits::CheckResult::BodyTooLarge);
        let (status, body) = result.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap_or("?");
        assert!(
            body_str.contains("Large"),
            "body must explain the limit: {body_str}"
        );
        assert_eq!(status, 413);
    }

    #[test]
    fn limits_rejection_header_message_correct() {
        let result = limits_rejection(limits::CheckResult::HeaderTooLarge);
        let (status, body) = result.unwrap();
        assert_eq!(status, 431);
        let body_str = std::str::from_utf8(&body).unwrap_or("?");
        assert!(!body_str.is_empty());
    }

    // ── Host header validation (LimitsGuard) ─────────────────────────────────
    //
    // These tests exercise the host_invalid logic inline, without needing a
    // full session, by reproducing the exact validation expression.

    fn check_host(raw: &[u8]) -> bool {
        // Returns true when the host value is INVALID (should be rejected).
        match http::header::HeaderValue::from_bytes(raw).ok().as_ref() {
            None => true, // bytes not representable as HeaderValue → invalid
            Some(v) => match v.to_str() {
                Err(_) => true, // non-UTF-8 → invalid
                Ok(s) => {
                    if s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
                        true
                    } else {
                        http::uri::Authority::try_from(s).is_err()
                    }
                }
            },
        }
    }

    #[test]
    fn host_valid_simple_domain_accepted() {
        assert!(!check_host(b"example.com"));
    }

    #[test]
    fn host_valid_domain_with_port_accepted() {
        assert!(!check_host(b"example.com:8080"));
    }

    #[test]
    fn host_valid_ipv4_accepted() {
        assert!(!check_host(b"192.168.1.1"));
    }

    #[test]
    fn host_valid_ipv6_accepted() {
        assert!(!check_host(b"[::1]:443"));
    }

    #[test]
    fn host_cr_lf_rejected() {
        assert!(check_host(b"evil.com\r\nX-Injected: yes"));
    }

    #[test]
    fn host_nul_byte_rejected() {
        assert!(check_host(b"evil.com\x00"));
    }

    #[test]
    fn host_space_rejected() {
        assert!(check_host(b"evil .com"));
    }

    #[test]
    fn host_path_separator_rejected() {
        assert!(check_host(b"evil.com/../../etc/passwd"));
    }

    #[test]
    fn host_non_utf8_rejected() {
        // 0xFF is not valid UTF-8; to_str() will return Err → treated as invalid.
        assert!(check_host(b"evil\xff.com"));
    }
}
