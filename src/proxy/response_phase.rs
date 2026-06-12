//! Response-side orchestration for [`ConduitProxy`].
//!
//! This module hosts the response-processing pipeline that used to live
//! directly in `service.rs`:
//!
//! - the `upstream_response_filter`, `upstream_response_body_filter`,
//!   `response_filter`, and `response_cache_filter` trait-method bodies
//!   (called from thin delegators in the `impl ProxyHttp for ConduitProxy`
//!   block in `service.rs`)
//! - response-side helpers: RFC 7234 `Age` computation
//!   (`compute_response_age`) and the early cache-refresh background task
//!   (`fire_early_refresh`, spawned from the logging phase)
//!
//! Pure mechanical split from `service.rs` — no behavioral change.

use bytes::Bytes;
use pingora_cache::{NoCacheReason, RespCacheable};
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

use crate::proxy::cache as proxy_cache;
use crate::proxy::ctx::RequestCtx;
use crate::proxy::service::ConduitProxy;

// ── Trait-method bodies (called from thin delegators in `impl ProxyHttp`) ────

/// Body of [`pingora_proxy::ProxyHttp::upstream_response_filter`].
///
/// Phase-ordered response pipeline.
///
/// Builds a [`ResponseFilterChain`] for this request and runs it against
/// the upstream response headers.  The chain encapsulates all header-level
/// concerns in six discrete phases:
///
/// 1. CRLF injection protection
/// 2. Inject CORS + security + custom site headers
/// 3. Apply `responseTransform` (set / remove headers)
/// 4. Inject `X-Response-Time`
/// 5. Trigger Pingora retry on 5xx (when `retry.conditions` includes "5xx")
/// 6. Set `mask_upstream_body` flag on 5xx (when `maskErrors: true`)
///
/// Adding a new response-side behaviour: implement `ResponseFilter` +
/// push into `ResponseFilterChain::build()` — no other changes required.
///
/// [`ResponseFilterChain`]: crate::filter::response_chain::ResponseFilterChain
pub(super) async fn upstream_response_filter(
    proxy: &ConduitProxy,
    _session: &mut Session,
    upstream_response: &mut ResponseHeader,
    ctx: &mut Option<RequestCtx>,
) -> Result<()> {
    use crate::filter::response_chain::{ResponseFilterChain, ResponseFilterOutcome};

    // RFC 7234 §5.1 Age header (#49): compute age for cache hits.
    //
    // Must happen before the immutable `req_ctx` borrow below so we can
    // write to `ctx`.  The mutable borrow ends at the closing `}`.
    #[cfg(feature = "cache")]
    {
        use pingora_cache::CachePhase;
        if matches!(
            _session.cache.phase(),
            CachePhase::Hit | CachePhase::Stale | CachePhase::StaleUpdating
        ) {
            if let Some(req_ctx_mut) = ctx.as_mut() {
                req_ctx_mut.cache_age_secs = Some(compute_response_age(upstream_response));
            }
        }
    }

    let req_ctx = match ctx.as_ref() {
        Some(c) => c,
        None => return Ok(()),
    };

    // Ignore 1xx interim responses from upstream (#50) — except 101.
    //
    // Pingora calls `upstream_response_filter` once per 1xx informational
    // response AND once more for the final response.  Running the full
    // ResponseFilterChain on 103 Early Hints, 100 Continue, etc. would
    // wrongly trigger retry / error-mask / CRLF-strip logic on a non-final
    // status.  We return `Ok(())` to let Pingora forward the 1xx to the
    // client unchanged.
    //
    // Source: freenginx `ngx_http_proxy_module.c` commit `fd953ff4` —
    //   ignore unexpected 1xx and continue parsing the real response.
    let status_u16 = upstream_response.status.as_u16();
    if status_u16 != 101 && upstream_response.status.is_informational() {
        tracing::debug!(
            status = status_u16,
            upstream = ?req_ctx.proxy_upstream_url,
            "skipping ResponseFilterChain for interim 1xx response"
        );
        return Ok(());
    }

    // WebSocket upgrade security (#46): reject unexpected protocol upgrades.
    //
    // A `101 Switching Protocols` response from upstream is only permitted
    // when the route explicitly declares `websocket: true`.  Otherwise the
    // upstream is violating the HTTP protocol contract and we drop the
    // connection with 502 to prevent unexpected tunnelling.
    if status_u16 == 101 && !req_ctx.websocket_allowed {
        tracing::warn!(
            upstream = ?req_ctx.proxy_upstream_url,
            "upstream returned 101 Switching Protocols but websocket is not \
             enabled for this route — rejecting upgrade"
        );
        return Err(pingora_core::Error::explain(
            pingora_core::ErrorType::HTTPStatus(502),
            "upstream attempted unexpected protocol upgrade (set websocket:true to allow)",
        ));
    }

    let config = proxy.state.config.load();
    let chain = ResponseFilterChain::build(req_ctx, &config);

    // Clone sticky-cookie data before the chain runs so that subsequent
    // mutable borrows of `ctx` (in MaskBody / RetryUpstream arms) do not
    // conflict with the immutable `req_ctx` reference.
    let sticky_cookie: Option<(String, String)> = req_ctx.sticky_set_cookie.clone();

    // The response chain may execute WASM plugins whose .wasm file is
    // read from disk on first load.  Use block_in_place to signal Tokio
    // that this synchronous chain execution may block.
    let run_result = tokio::task::block_in_place(|| chain.run(upstream_response, req_ctx));

    match run_result? {
        ResponseFilterOutcome::Continue => {}

        ResponseFilterOutcome::RetryUpstream => {
            let status = upstream_response.status.as_u16();
            // Failure propagation fix (#47): record health / metrics for the
            // failed upstream BEFORE returning the retry error — see
            // `record_failed_upstream_for_retry` for the full rationale.
            proxy.record_failed_upstream_for_retry(ctx, &config, status);
            // Use new_up() so ErrorSource::Upstream is set — required for
            // should_serve_stale() to recognise this as an upstream error
            // and serve a stale cached response (#48).
            return Err(
                pingora_core::Error::new_up(pingora_core::ErrorType::Custom("5xx_retry"))
                    .more_context(format!("upstream returned HTTP {status}")),
            );
        }

        ResponseFilterOutcome::MaskBody => {
            if let Some(req_ctx_mut) = ctx.as_mut() {
                req_ctx_mut.mask_upstream_body = true;
                let body_len = b"{\"error\":\"Internal Server Error\",\"status\":500}".len();
                upstream_response.insert_header("content-type", "application/json")?;
                upstream_response.insert_header("content-length", body_len.to_string())?;
                upstream_response.set_status(500)?;
            }
        }
    }

    // Sticky-session Set-Cookie injection (#39): when `sticky.secret` is
    // configured the chosen upstream URL is HMAC-signed and returned to
    // the client so future requests pin to the same backend.
    //
    // This runs after the response chain so it is never clobbered by
    // response transforms.  The value is `HttpOnly; SameSite=Lax` so it is
    // not accessible from JavaScript and provides basic CSRF protection.
    if let Some((cookie_name, cookie_val)) = &sticky_cookie {
        let cookie_header = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Lax",
            cookie_name, cookie_val
        );
        // Use append_header so upstream Set-Cookie headers (session
        // tokens, auth cookies) are preserved alongside the sticky cookie.
        let _ = upstream_response.append_header("set-cookie", cookie_header);
    }

    Ok(())
}

/// Body of [`pingora_proxy::ProxyHttp::upstream_response_body_filter`].
///
/// When `maskErrors` is enabled for the site and the upstream returned a 5xx
/// status, replace every body chunk with a generic JSON error so that
/// internal details never reach the client.
pub(super) fn upstream_response_body_filter(
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut Option<RequestCtx>,
) -> Result<Option<std::time::Duration>> {
    if let Some(req_ctx) = ctx.as_ref() {
        if req_ctx.mask_upstream_body {
            if end_of_stream {
                // Replace the entire (or last) body chunk with a generic error.
                *body = Some(Bytes::from_static(
                    b"{\"error\":\"Internal Server Error\",\"status\":500}",
                ));
            } else {
                // Discard intermediate chunks — only send the replacement on eos.
                *body = None;
            }
        }
    }
    Ok(None)
}

/// Body of [`pingora_proxy::ProxyHttp::response_filter`].
///
/// Detect cache hits that are within the early-refresh window (#31).
///
/// Pingora calls `response_filter` for **all** responses — including those
/// served directly from the cache.  We check whether:
///
/// 1. The route has `cache.earlyRefreshSecs` configured.
/// 2. The current response is a cache hit (`CachePhase::Hit`).
/// 3. The remaining TTL (`fresh_until − now`) is within the refresh window.
///
/// When all three are true, we store the upstream URL in `RequestCtx`.
/// `logging()` will then spawn a fire-and-forget reqwest GET, causing Pingora
/// to fetch a fresh copy and store it.  The **current** client request still
/// receives the cached (still valid) response without any latency penalty.
///
/// Source: h2o `lib/common/cache.c` — `H2O_CACHE_FLAG_EARLY_UPDATE`.
pub(super) async fn response_filter(
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
) -> Result<()> {
    #[cfg(feature = "cache")]
    {
        use pingora_cache::CachePhase;
        use std::time::SystemTime;

        // Only trigger early refresh for cache hits.
        if !matches!(session.cache.phase(), CachePhase::Hit) {
            return Ok(());
        }

        let Some(req_ctx) = ctx.as_mut() else {
            return Ok(());
        };

        // Only when the route has earlyRefreshSecs configured.
        let early_window_secs = req_ctx
            .proxy_cache_cfg
            .as_ref()
            .and_then(|c| c.early_refresh_secs)
            .unwrap_or(0);
        if early_window_secs == 0 {
            return Ok(());
        }

        // Get the upstream URL for the background refresh task.
        let upstream_url = match &req_ctx.proxy_upstream_url {
            Some(url) => url.clone(),
            None => return Ok(()),
        };

        // Check how much TTL remains.
        let remaining_secs = session
            .cache
            .maybe_cache_meta()
            .and_then(|meta| meta.fresh_until().duration_since(SystemTime::now()).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if proxy_cache::should_early_refresh(remaining_secs, early_window_secs) {
            tracing::debug!(
                upstream = %upstream_url,
                remaining_secs,
                early_window_secs,
                "cache TTL within early-refresh window — scheduling background refresh"
            );
            req_ctx.early_refresh_upstream_url = Some(upstream_url);
        }
    }
    #[cfg(not(feature = "cache"))]
    let _ = (session, ctx);
    Ok(())
}

/// Body of [`pingora_proxy::ProxyHttp::response_cache_filter`].
///
/// Decide whether an upstream response is cacheable.
///
/// Returns [`RespCacheable::Cacheable`] for `200 OK` responses when the
/// route has a non-zero `ttl_secs`.  Everything else is uncacheable.
pub(super) fn response_cache_filter(
    resp: &ResponseHeader,
    ctx: &mut Option<RequestCtx>,
) -> Result<RespCacheable> {
    let cacheable = ctx
        .as_ref()
        .and_then(|c| c.proxy_cache_cfg.as_ref())
        .map(|cfg| proxy_cache::response_cacheable(cfg, resp))
        .unwrap_or(RespCacheable::Uncacheable(NoCacheReason::Custom(
            "no-cache-cfg",
        )));
    Ok(cacheable)
}

// ── response-side helpers ─────────────────────────────────────────────────────

/// Compute the RFC 7234 §5.1 `Age` value for a cached response.
///
/// Uses the `Date` header of the stored response to determine how old it is:
/// `age = now − date_header_value`.  This is the "apparent age" formula from
/// RFC 7234 §5.1, which is sufficient for most proxy deployments.
///
/// Returns `0` when the `Date` header is absent or cannot be parsed — the
/// response will still carry an `Age: 0` header, signalling a fresh hit.
#[cfg(feature = "cache")]
fn compute_response_age(resp: &pingora_http::ResponseHeader) -> u64 {
    use std::time::SystemTime;
    resp.headers
        .get("date")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok())
        .and_then(|date| SystemTime::now().duration_since(date).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fire-and-forget an early cache refresh GET to an upstream URL (#31).
///
/// Called from `logging()` when the cached entry's remaining TTL is within
/// `earlyRefreshSecs`.  The response is discarded; the purpose is purely to
/// cause Pingora's cache to be refreshed for the next real client request.
///
/// The request is sent directly to the upstream (not via Pingora), so it
/// bypasses the local request pipeline.  A short 10-second timeout prevents
/// slow upstreams from holding the background task open.
///
/// Source: h2o `lib/common/cache.c` — `H2O_CACHE_FLAG_EARLY_UPDATE`.
#[cfg(feature = "cache")]
pub(super) async fn fire_early_refresh(upstream_url: &str, path_and_query: &str) {
    let base = upstream_url.trim_end_matches('/');
    let target = format!("{base}{path_and_query}");

    // Reuse a single process-wide client to benefit from connection pooling.
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    });

    match client
        .get(&target)
        .header("X-Early-Refresh", "1")
        .send()
        .await
    {
        Ok(resp) => {
            tracing::debug!(
                url = %target,
                status = resp.status().as_u16(),
                "early refresh: upstream response received"
            );
        }
        Err(e) => {
            tracing::debug!(url = %target, error = %e, "early refresh: request failed");
        }
    }
}

#[cfg(test)]
mod tests {
    // `compute_response_age` (the only item exercised from this module's scope)
    // is compiled only with --features cache; the 1xx tests below use full paths.
    #[cfg(feature = "cache")]
    use super::*;

    // ── compute_response_age ─────────────────────────────────────────────────

    #[cfg(feature = "cache")]
    #[test]
    fn compute_response_age_from_date_header() {
        use http::StatusCode;
        // Build a response with a Date header set 60 seconds in the past.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let date_str = httpdate::fmt_http_date(past);
        let mut resp = pingora_http::ResponseHeader::build(StatusCode::OK, None).unwrap();
        resp.insert_header("date", date_str).unwrap();
        let age = compute_response_age(&resp);
        // Allow ±2s for test execution time.
        assert!(age >= 58 && age <= 62, "age should be ~60s, got {age}");
    }

    #[cfg(feature = "cache")]
    #[test]
    fn compute_response_age_missing_date_returns_zero() {
        use http::StatusCode;
        let resp = pingora_http::ResponseHeader::build(StatusCode::OK, None).unwrap();
        assert_eq!(compute_response_age(&resp), 0);
    }

    #[cfg(feature = "cache")]
    #[test]
    fn compute_response_age_unparseable_date_returns_zero() {
        use http::StatusCode;
        let mut resp = pingora_http::ResponseHeader::build(StatusCode::OK, None).unwrap();
        resp.insert_header("date", "not-a-date").unwrap();
        assert_eq!(compute_response_age(&resp), 0);
    }

    // ── 1xx interim response guard (#50) ─────────────────────────────────────
    //
    // The logic in `upstream_response_filter` that skips the ResponseFilterChain
    // for 1xx (except 101) uses `StatusCode::is_informational()`.  These tests
    // verify the classification of status codes we care about so that a refactor
    // of the guard condition does not silently regress.

    #[test]
    fn status_100_is_informational_and_not_101() {
        let s = http::StatusCode::CONTINUE;
        assert!(s.is_informational());
        assert_ne!(s.as_u16(), 101);
    }

    #[test]
    fn status_103_early_hints_is_informational_and_not_101() {
        // 103 Early Hints — common source of unexpected 1xx from backends
        // (Spring Boot, Caddy, CDNs).  Must be skipped by the guard.
        let s = http::StatusCode::from_u16(103).unwrap();
        assert!(s.is_informational());
        assert_ne!(s.as_u16(), 101);
    }

    #[test]
    fn status_101_is_informational_but_handled_separately() {
        // 101 is informational but should NOT be skipped — the WebSocket
        // upgrade guard runs for 101 specifically.
        let s = http::StatusCode::SWITCHING_PROTOCOLS;
        assert!(s.is_informational());
        assert_eq!(s.as_u16(), 101);
        // Guard condition: status != 101 AND is_informational() → skip.
        // For 101 this is FALSE — so the WebSocket check runs instead.
        assert!(!(s.as_u16() != 101 && s.is_informational()));
    }

    #[test]
    fn status_200_is_not_informational() {
        let s = http::StatusCode::OK;
        assert!(!s.is_informational());
    }

    #[test]
    fn status_500_is_not_informational() {
        let s = http::StatusCode::INTERNAL_SERVER_ERROR;
        assert!(!s.is_informational());
    }
}
