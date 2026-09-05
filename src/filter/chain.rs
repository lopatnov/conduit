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

use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;

#[cfg(feature = "consumers")]
use crate::config::schema::ConsumersConfig;
use crate::config::schema::{ApiKeyConfig, BasicAuthConfig, MiddlewareEntry, RateLimitConfig};
use crate::filter::rate_limit::RateLimiter;
#[cfg(feature = "redis")]
use crate::filter::rate_limit_redis::RedisRateLimiter;
#[cfg(feature = "rhai")]
use crate::filter::script;
use crate::filter::{auth, rate_limit};
use crate::handler::response;
use uuid::Uuid;

// ── Outcome + Context + Trait (Layer-0 vocabulary, #114/#126) ──────────────────

pub use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};

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

/// Extracted into `crates/conduit-ipfilter` (issue #114/#136) — this is a
/// facade re-export so `crate::filter::chain::IpGuard` keeps resolving to
/// the same type at the same location for every existing call site/test.
/// See that crate's `src/guard.rs` for the implementation: rejects requests
/// whose client IP is not in the allow-list / is in the deny-list (including
/// the runtime deny-list managed via Admin API `POST /ip-deny`).
pub use conduit_ipfilter::guard::IpGuard;

/// Extracted into `crates/conduit-cors` (issue #114/#136) — this is a facade
/// re-export so `crate::filter::chain::CorsPreflight` keeps resolving to the
/// same type at the same location for every existing call site/test. See
/// that crate's `src/guard.rs` for the implementation: handles CORS
/// preflight (`OPTIONS`) requests and echoes the appropriate headers,
/// returning `FilterOutcome::Handled` so downstream filters and the
/// upstream proxy are never reached.
pub use conduit_cors::guard::CorsPreflight;

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

/// Extracted into `crates/conduit-security-headers` (issue #114/#136) — this
/// is a facade re-export so `crate::filter::chain::AllowedHostsGuard` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. See that crate's `src/guard.rs` for the implementation:
/// validates the `Host` request header against a configured allowlist
/// (falling back to the site's own `host:` value when `securityHeaders.
/// allowedHosts` isn't explicitly set), returning `400 Bad Request` on a
/// mismatch. Runs immediately after `HealthBypass` so health/ACME/hot-reload
/// endpoints are always reachable regardless of the allowlist.
pub use conduit_security_headers::guard::AllowedHostsGuard;

/// Extracted into `crates/conduit-limits` (issue #114/#137) — this is a
/// facade re-export so `crate::filter::chain::{LimitsGuard, IpConnSlotGuard}`
/// keep resolving to the same types at the same location for every existing
/// call site/test. See that crate's `src/guard.rs` for the implementation:
/// Host-header validation, `maxRequestHeaders`, `maxInflightRequests`,
/// declared body/header size limits, and `maxConnectionsPerIp` via the RAII
/// `IpConnSlotGuard` (released when `RequestCtx`'s `limits.ip_conn_slot`
/// is dropped at the end of `logging()`).
pub use conduit_limits::guard::{IpConnSlotGuard, LimitsGuard};

/// Token-bucket rate limiter; falls back to in-memory when Redis is unavailable.
pub struct RateLimitGuard {
    pub cfg: RateLimitConfig,
    /// Label used for `conduit_rate_limit_rejected_total{site=…}`.
    /// Typically `"host:port"` or `"*"` for catch-all sites.
    pub site_label: String,
    /// In-memory rate-limit token buckets.
    pub rate_limiter: Arc<RateLimiter>,
    /// Optional Redis-backed rate limiter (may be `None` at startup).
    /// Only available when compiled with `--features redis`.
    #[cfg(feature = "redis")]
    pub redis_rate_limiter: Option<Arc<RedisRateLimiter>>,
}

#[async_trait]
impl RequestFilter for RateLimitGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        let allowed = rate_limit_allowed(
            &self.cfg,
            ctx.session,
            &self.rate_limiter,
            &self.site_label,
            #[cfg(feature = "redis")]
            self.redis_rate_limiter.as_ref(),
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
/// (API key, Basic Auth, per-consumer JWT, or shared JWT) in declaration
/// order.  On success:
///   - Injects `X-Consumer-ID: <username>` (or the configured `idHeader`)
///   - Applies per-consumer rate limit using the shared limiter
///   - Injects any per-consumer custom headers
///
/// Returns 401 when no consumer matches.
#[cfg(feature = "consumers")]
pub struct ConsumersGuard {
    pub cfg: ConsumersConfig,
    pub path: String,
    /// Shared token-bucket rate limiter, used for the per-consumer rate limit.
    pub rate_limiter: Arc<RateLimiter>,
    /// Optional Redis-backed rate limiter (may be `None` at startup) — used
    /// when a consumer's `rateLimit.store` is `"redis://..."` (issue #322).
    /// Only available when compiled with `--features redis`.
    #[cfg(feature = "redis")]
    pub redis_rate_limiter: Option<Arc<RedisRateLimiter>>,
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
        //
        // Extracted into `crates/conduit-auth-consumers` (issue #114/#134):
        // `identify_consumer` moved there, but `ConsumersGuard` itself
        // stays here — see that crate's `src/lib.rs` doc comment for why
        // (this guard also needs `self.rate_limiter` below, which hasn't
        // been extracted yet).
        let consumer = conduit_auth_consumers::identify_consumer(&self.cfg, ctx.session);
        let Some(consumer) = consumer else {
            response::write_denied(ctx.session, None, ctx.extra_headers).await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        };

        // Per-consumer rate limit — global per consumer, not site-scoped
        // (see rate_limit::consumer_key's doc for why). `keyBy` stays a
        // no-op here by design regardless of backend (schema docs: the
        // consumer bucket key is always the username, never derived from
        // the request). `store: "redis://..."` is now honored too (issue
        // #322) — the Redis scope label is the fixed literal `"consumer"`
        // (not the username), with the username carried in `client_key`
        // instead, since `RedisRateLimiter::check`'s Redis key already
        // folds `client_key` in as its own segment.
        if let Some(rl_cfg) = &consumer.rate_limit {
            let skip = rl_cfg
                .skip_paths
                .as_deref()
                .is_some_and(|sp| auth::is_path_skipped(Some(sp), &self.path));
            if !skip {
                #[cfg(feature = "redis")]
                let allowed = if rl_cfg
                    .store
                    .as_deref()
                    .is_some_and(|s| s.starts_with("redis://") || s.starts_with("rediss://"))
                {
                    if let Some(rrl) = &self.redis_rate_limiter {
                        rrl.check(
                            "consumer",
                            &consumer.username,
                            rl_cfg.limit,
                            rl_cfg.burst.unwrap_or(0),
                            rl_cfg.window_secs,
                        )
                        .await
                    } else {
                        let key = rate_limit::consumer_key(&consumer.username);
                        conduit_ratelimit::check_key_for(&self.rate_limiter, &key, rl_cfg)
                    }
                } else {
                    let key = rate_limit::consumer_key(&consumer.username);
                    // Routed through the shared MAX_BUCKETS-capped admission
                    // point (issue #305) instead of a hand-rolled, uncapped
                    // entry()/or_insert_with() — this map has no cap check
                    // of its own.
                    conduit_ratelimit::check_key_for(&self.rate_limiter, &key, rl_cfg)
                };
                #[cfg(not(feature = "redis"))]
                let allowed = {
                    let key = rate_limit::consumer_key(&consumer.username);
                    conduit_ratelimit::check_key_for(&self.rate_limiter, &key, rl_cfg)
                };
                if !allowed {
                    if rl_cfg.dry_run.unwrap_or(false) {
                        tracing::warn!(
                            consumer = %consumer.username,
                            "[dry-run] per-consumer rate limit exceeded — request allowed through (dryRun: true)"
                        );
                    } else {
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

/// Extracted into `crates/conduit-auth-jwt` (issue #114/#133) — this is a
/// facade re-export so `crate::filter::chain::JwtGuard` keeps resolving to
/// the same type at the same location for every existing call site/test.
/// See that crate's `src/guard.rs` for the implementation: validates the
/// `Authorization: Bearer <token>` header using either an HMAC secret
/// (`jwtAuth.secret`) or a remote JWKS endpoint (`jwtAuth.jwksUrl`).
/// Returns `401 Unauthorized` when the token is absent or invalid.
#[cfg(feature = "jwt")]
pub use conduit_auth_jwt::guard::JwtGuard;

/// Extracted into `crates/conduit-auth-forward` (issue #114/#134) — this is
/// a facade re-export so `crate::filter::chain::ForwardAuthGuard` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. See that crate's `src/guard.rs` for the implementation:
/// delegates authentication/authorization to an external service, sending
/// the incoming request (filtered headers) to the configured auth URL.
/// - **2xx** → auth passed; headers listed in `responseHeaders` are injected
///   into the upstream request so the upstream receives user identity/role info.
/// - **4xx / 5xx** → auth denied; the auth service status is returned to the
///   client immediately.
#[cfg(feature = "forward-auth")]
pub use conduit_auth_forward::guard::ForwardAuthGuard;

/// Extracted into `crates/conduit-faults` (issue #114/#132) — this is a
/// facade re-export so `crate::filter::chain::FaultInjectionGuard` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. See that crate's `src/guard.rs` for the implementation:
/// injects artificial faults (aborts or delays) for chaos-engineering and
/// testing retry/circuit-breaker behaviour. **Should not be used in
/// production.**
#[cfg(feature = "fault-injection")]
pub use conduit_faults::guard::FaultInjectionGuard;

/// Extracted into `crates/conduit-redirects` (issue #114/#140) — this is a
/// facade re-export so `crate::filter::chain::RedirectGuard` keeps
/// resolving to the same type at the same location for every existing call
/// site/test. See that crate's `src/guard.rs` for the implementation:
/// handles configured URL redirects (301/302/307/308), answering the
/// request directly when a rule matched.
pub use conduit_redirects::guard::RedirectGuard;

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
    site_label: &str,
    #[cfg(feature = "redis")] redis: Option<&Arc<RedisRateLimiter>>,
    #[cfg(not(feature = "redis"))] _redis: Option<()>,
) -> bool {
    // Checked once, up front, so it applies uniformly to both the Redis and
    // in-memory paths below. Previously only `rate_limit::check` (the
    // in-memory path) checked this — a site-level `store: redis` config
    // silently ignored `skipPaths` entirely (found while fixing #306/#307;
    // not a previously-filed issue, just the same code this touches).
    let path = session.req_header().uri.path();
    if auth::is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return true;
    }

    #[cfg(feature = "redis")]
    if cfg
        .store
        .as_deref()
        .is_some_and(|s| s.starts_with("redis://") || s.starts_with("rediss://"))
    {
        if let Some(rrl) = redis {
            let key = rate_limit::extract_client_key(cfg, session);
            return rrl
                .check(
                    site_label,
                    &key,
                    cfg.limit,
                    cfg.burst.unwrap_or(0),
                    cfg.window_secs,
                )
                .await;
        }
    }
    #[cfg(not(feature = "redis"))]
    let _ = cfg.store.as_deref(); // suppress unused warning when redis disabled
    rate_limit::check(cfg, session, rate_limiter, site_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── LimitsGuard / IpConnSlotGuard / is_host_header_invalid /
    //    try_acquire_ip_slot / limits_rejection tests — moved to
    //    crates/conduit-limits/src/guard.rs (issue #114/#137).

    // ── IpGuard dynamic deny list ────────────────────────────────────────────
    //
    // `is_dynamic_denied` takes a `&pingora_proxy::Session`, which this crate
    // has no unit-test helper to construct without real I/O — so real
    // end-to-end coverage (a request actually blocked/allowed by a dynamic
    // deny entry added via `POST /ip-deny`) lives in `tests/ip_filter.rs`
    // instead. `client_ip_for_check` and `is_in_deny_list`, the two pieces
    // `is_dynamic_denied` composes, each have direct unit coverage in
    // `ip_filter.rs`.

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

    // ── forward_auth_client — moved to crates/conduit-auth-forward (#114/#134) ─
}
