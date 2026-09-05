//! Phase-ordered response pipeline (symmetric to the request-side FilterChain).
//!
//! Each [`ResponseFilter`] handles one concern in the response path.
//! Filters are evaluated in insertion order; the first
//! [`ResponseFilterOutcome::RetryUpstream`] or
//! [`ResponseFilterOutcome::MaskBody`] terminates the chain.
//!
//! ## Phases (in default order)
//!
//! 1. `CrlfProtectionFilter`  — strip CRLF-injected response headers
//! 2. `InjectExtraHeadersFilter` — CORS + security + custom site headers
//! 3. `ResponseTransformFilter` — static set/remove from `responseTransform`
//! 4. `ResponseTimeFilter`    — inject `X-Response-Time`
//! 5. `RetryOnErrorFilter`    — trigger Pingora retry on 5xx (if configured)
//! 6. `ErrorMaskFilter`       — flag body replacement on 5xx (maskErrors)
//!
//! ## Adding a new response phase
//!
//! 1. Create a struct that holds its config.
//! 2. `impl ResponseFilter for YourFilter`.
//! 3. Push it into `ResponseFilterChain::build()` at the correct position.
//!
//! No other files need to change.

use pingora_core::Result;
use pingora_http::ResponseHeader;

use crate::config::schema::{AppConfig, HeaderTransformConfig, ResponseTimeConfig};
use crate::filter::response_time;
use crate::proxy::ctx::RequestCtx;

// ── Outcome + Trait (Layer-0 vocabulary, #114/#120/#126) ────────────────────────

pub use conduit_core::filter::response_chain::{
    ResponseCtx, ResponseFilter, ResponseFilterOutcome,
};

impl ResponseCtx for RequestCtx {
    fn cache_age_secs(&self) -> Option<u64> {
        // Calls the inherent `RequestCtx::cache_age_secs` accessor (defined
        // in `proxy/ctx.rs`) — Rust resolves inherent methods before trait
        // methods on `self.method()` calls, so this is not recursive.
        self.cache_age_secs()
    }
}

// ── Chain ─────────────────────────────────────────────────────────────────────

/// An ordered list of response filters.
///
/// Run by [`ResponseFilterChain::run`]; terminates early on the first
/// non-Continue outcome.
#[derive(Default)]
pub struct ResponseFilterChain {
    filters: Vec<Box<dyn ResponseFilter>>,
}

impl ResponseFilterChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(mut self, f: impl ResponseFilter + 'static) -> Self {
        self.filters.push(Box::new(f));
        self
    }

    /// Run every filter in order.
    ///
    /// Returns the first non-Continue outcome, or `Continue` when all filters
    /// pass without raising a terminal outcome.
    pub fn run(
        &self,
        resp: &mut ResponseHeader,
        req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        for filter in &self.filters {
            match filter.apply(resp, req_ctx)? {
                ResponseFilterOutcome::Continue => continue,
                other => return Ok(other),
            }
        }
        Ok(ResponseFilterOutcome::Continue)
    }

    /// Build the default response filter chain from request context and config.
    ///
    /// Called once per request inside `upstream_response_filter`.
    pub fn build(req_ctx: &RequestCtx, config: &AppConfig) -> Self {
        let site = config.sites.get(req_ctx.site_idx);

        let mut chain = Self::new();

        // Phase 1 — Security: strip header-injection characters; deduplicate chunked.
        let allow_duplicate_chunked = site
            .and_then(|s| s.allow_duplicate_chunked)
            .unwrap_or(false);
        chain = chain.push(CrlfProtectionFilter {
            allow_duplicate_chunked,
        });

        // Phase 2 — CORS + security + custom site headers.
        chain = chain.push(InjectExtraHeadersFilter {
            headers: req_ctx.extra_headers.clone(),
        });

        // Phase 3 — Static response-header transform (set/remove).
        if let Some(transform) = req_ctx.response_transform.clone() {
            chain = chain.push(ResponseTransformFilter { transform });
        }

        // Phase 4 — X-Response-Time header.
        let rt_cfg = site.and_then(|s| s.response_time.clone());
        let start_time = req_ctx.start_time;
        chain = chain.push(ResponseTimeFilter { rt_cfg, start_time });

        // Phase 4.5 — W3C Server-Timing header.
        if site.and_then(|s| s.server_timing).unwrap_or(false) {
            chain = chain.push(ServerTimingFilter {
                start_time: req_ctx.start_time,
                upstream_start: req_ctx.upstream_start,
            });
        }

        // Phase 5 — 5xx retry / stale-if-error fallback (terminates chain if fired).
        //
        // `stale_on_error` enables the error path even when retry is not configured
        // (or retry budget is exhausted).  This allows Pingora to call
        // `should_serve_stale()` and serve a cached stale response on 5xx (#48).
        let stale_on_error = req_ctx
            .proxy_cache_cfg
            .as_ref()
            .and_then(|c| c.stale_if_error_secs)
            .unwrap_or(0)
            > 0;
        chain = chain.push(RetryOnErrorFilter {
            retry: req_ctx.retry.as_ref().map(|r| RetrySpec {
                has_attempts_left: r.has_attempts_left(),
                has_5xx_condition: r.has_condition("5xx"),
            }),
            stale_on_error,
        });

        // Phase 6 — Error masking (terminates chain if fired).
        let mask_enabled = site.and_then(|s| s.mask_errors).unwrap_or(false);
        chain = chain.push(ErrorMaskFilter { mask_enabled });

        // Phase 7 — Rhai / WASM on_response middleware.
        let middleware = site
            .and_then(|s| s.middleware.as_ref())
            .cloned()
            .unwrap_or_default();
        if !middleware.is_empty() {
            chain = chain.push(MiddlewareResponseFilter { middleware });
        }

        chain
    }
}

// ── Concrete filters ──────────────────────────────────────────────────────────

/// Phase 1 — Strip response headers whose values contain CR or LF characters,
/// and deduplicate `Transfer-Encoding: chunked` headers from upstream.
///
/// Prevents header-injection (CRLF injection) attacks where an upstream
/// embeds newlines in header values to splice additional HTTP headers into
/// the client response.
///
/// Duplicate `chunked` directives (e.g. `Transfer-Encoding: chunked, chunked`
/// or two separate `Transfer-Encoding: chunked` headers) are removed unless
/// `allowDuplicateChunked: true` is set in the site config.
pub struct CrlfProtectionFilter {
    /// When `true`, pass duplicate `Transfer-Encoding: chunked` headers through
    /// unmodified.  Defaults to `false` (deduplicate).
    pub allow_duplicate_chunked: bool,
}

impl ResponseFilter for CrlfProtectionFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        // Remove headers containing CR or LF (header-injection protection).
        let bad: Vec<http::header::HeaderName> = resp
            .headers
            .iter()
            .filter_map(|(name, value)| {
                if value.as_bytes().iter().any(|&b| b == b'\r' || b == b'\n') {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        for name in bad {
            resp.headers.remove(&name);
        }

        if !self.allow_duplicate_chunked {
            dedup_chunked_transfer_encoding(resp);
        }

        Ok(ResponseFilterOutcome::Continue)
    }
}

/// Remove duplicate `Transfer-Encoding: chunked` directives from upstream
/// responses.
///
/// Some Java application servers (Spring Cloud Gateway, Zuul, Tomcat) emit
/// two `Transfer-Encoding: chunked` headers or a single header with a
/// comma-separated `chunked, chunked` value.  RFC 7230 §3.3.1 requires that
/// `chunked` appears exactly once as the outermost encoding, so we normalise
/// the header by collapsing multiple occurrences into one.
///
/// Source: freenginx `ngx_http_proxy_module.c` commit `56d8eaa6`
/// (`proxy_allow_duplicate_chunked`).
fn dedup_chunked_transfer_encoding(resp: &mut ResponseHeader) {
    // Fast path: single iterator pass with no allocation.
    // Peek at the first two TE headers; if there is at most one and it
    // contains no comma, there can be no duplicate "chunked" tokens.
    let mut te_iter = resp.headers.get_all("transfer-encoding").iter();
    let first = te_iter.next();
    let second = te_iter.next();
    let needs_dedup = match (first, second) {
        (None, _) => false,
        (Some(_), Some(_)) => true, // two or more separate TE headers
        (Some(v), None) => v.as_bytes().contains(&b','), // comma → possible duplicates
    };
    if !needs_dedup {
        return;
    }

    let te_values: Vec<String> = resp
        .headers
        .get_all("transfer-encoding")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .collect();

    // Count occurrences of "chunked" across all TE header values.
    let chunked_count = te_values
        .iter()
        .flat_map(|v| v.split(','))
        .filter(|s| s.trim().eq_ignore_ascii_case("chunked"))
        .count();

    if chunked_count <= 1 {
        return;
    }

    // Remove all Transfer-Encoding headers and re-insert a single
    // deduplicated value, preserving any other directives.
    let other_directives: Vec<&str> = te_values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.eq_ignore_ascii_case("chunked"))
        .collect();
    resp.headers.remove("transfer-encoding");
    let new_val = if other_directives.is_empty() {
        "chunked".to_owned()
    } else {
        format!("{}, chunked", other_directives.join(", "))
    };
    let _ = resp.insert_header("transfer-encoding", new_val);
}

/// Phase 2 — Inject pre-computed CORS, security, and custom site headers.
///
/// Headers were computed once in `do_request_filter` and stored in
/// `RequestCtx.extra_headers` to avoid re-computing on every response.
pub struct InjectExtraHeadersFilter {
    pub headers: Vec<(String, String)>,
}

impl ResponseFilter for InjectExtraHeadersFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        // Strip Pingora's default `Server: Pingora` banner — it leaks the
        // proxy software name, helping attackers target Pingora-specific CVEs.
        // Upstream-set Server headers (from a real backend) are left alone since
        // Pingora only injects this value for its own locally-generated responses
        // (502/503/504 error pages); we strip it here before the client sees it.
        if resp
            .headers
            .get("server")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("Pingora"))
            .unwrap_or(false)
        {
            resp.remove_header("server");
        }

        // Age header for cache hits (RFC 7234 §5.1, #49).
        //
        // Remove any `Age` value carried by the stored response before
        // inserting the freshly computed one — prevents double-counting when
        // a cached response already has an `Age` header from a prior hop.
        if let Some(age_secs) = req_ctx.cache_age_secs() {
            resp.headers.remove("age");
            resp.insert_header("age", age_secs.to_string())?;
        }

        for (name, value) in &self.headers {
            resp.insert_header(name.clone(), value.clone())?;
        }
        Ok(ResponseFilterOutcome::Continue)
    }
}

/// Phase 3 — Apply static `responseTransform` (set / remove headers).
pub struct ResponseTransformFilter {
    pub transform: HeaderTransformConfig,
}

impl ResponseFilter for ResponseTransformFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        if let Some(remove) = &self.transform.remove_headers {
            for name in remove {
                resp.headers.remove(name.as_str());
            }
        }
        if let Some(set) = &self.transform.set_headers {
            for (name, value) in set {
                resp.insert_header(name.clone(), value.clone())?;
            }
        }
        Ok(ResponseFilterOutcome::Continue)
    }
}

/// Phase 4 — Inject `X-Response-Time` header when enabled.
pub struct ResponseTimeFilter {
    pub rt_cfg: Option<ResponseTimeConfig>,
    pub start_time: std::time::Instant,
}

impl ResponseFilter for ResponseTimeFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        if response_time::is_enabled(self.rt_cfg.as_ref()) {
            let digits = response_time::decimal_digits(self.rt_cfg.as_ref());
            let elapsed = self.start_time.elapsed();
            let value = response_time::format_elapsed(elapsed, digits);
            resp.insert_header("x-response-time", value)?;
        }
        Ok(ResponseFilterOutcome::Continue)
    }
}

/// Phase 4.5 — W3C `Server-Timing` response header.
///
/// Emits timing entries visible in browser DevTools → Network → Timing panel:
/// - `total;dur=<ms>` — time from request received to upstream response headers
/// - `upstream;dur=<ms>` — upstream TTFB (only when an upstream request was made)
///
/// Enabled per-site with `serverTiming: true`.
pub struct ServerTimingFilter {
    pub start_time: std::time::Instant,
    pub upstream_start: Option<std::time::Instant>,
}

impl ResponseFilter for ServerTimingFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        let total_ms = self.start_time.elapsed().as_secs_f64() * 1000.0;
        let value = match self.upstream_start {
            Some(us) => {
                let upstream_ms = us.elapsed().as_secs_f64() * 1000.0;
                format!("total;dur={total_ms:.1}, upstream;dur={upstream_ms:.1}")
            }
            None => format!("total;dur={total_ms:.1}"),
        };
        resp.insert_header("server-timing", value)?;
        Ok(ResponseFilterOutcome::Continue)
    }
}

/// Retry specification passed to `RetryOnErrorFilter` without a borrow
/// of `RetryState` (which would create a lifetime dependency on `RequestCtx`).
#[derive(Debug)]
pub struct RetrySpec {
    pub has_attempts_left: bool,
    pub has_5xx_condition: bool,
}

/// Phase 5 — Trigger a Pingora retry when the upstream returns a 5xx status
/// and the route has a `retry` config with `"5xx"` in its conditions list.
///
/// Also handles the stale-if-error gap (#48): when `stale_on_error` is set,
/// triggers the error path even without a retry config (or when retry budget is
/// exhausted) so that Pingora can call `should_serve_stale()` and serve a
/// cached stale response instead of forwarding the 5xx to the client.
///
/// Returns `RetryUpstream` (terminal) — the caller must propagate a Pingora
/// `Custom("5xx_retry")` error to activate the retry / stale-fallback machinery.
pub struct RetryOnErrorFilter {
    pub retry: Option<RetrySpec>,
    /// Trigger the error path on 5xx even when retry is not configured (or
    /// exhausted), so that `should_serve_stale()` can serve a stale response.
    ///
    /// Derived from `cache.staleIfErrorSecs > 0`.
    pub stale_on_error: bool,
}

impl ResponseFilter for RetryOnErrorFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        if resp.status.as_u16() >= 500 {
            // Retry takes priority when budget and conditions are available.
            if let Some(spec) = &self.retry {
                if spec.has_attempts_left && spec.has_5xx_condition {
                    return Ok(ResponseFilterOutcome::RetryUpstream);
                }
            }
            // Stale-if-error fallback (#48): trigger the Pingora error path so
            // that `should_serve_stale()` can serve a cached stale response.
            // Handles two cases:
            // 1. No retry config (retry = None).
            // 2. Last retry attempt exhausted (has_attempts_left = false).
            if self.stale_on_error {
                return Ok(ResponseFilterOutcome::RetryUpstream);
            }
        }
        Ok(ResponseFilterOutcome::Continue)
    }
}

/// Phase 6 — Signal that the response body should be replaced with a generic
/// JSON error when `maskErrors: true` and the upstream returned a 5xx status.
///
/// Returns `MaskBody` (terminal) — the caller sets
/// `RequestCtx.mask_upstream_body = true` and overwrites `Content-Type` /
/// `Content-Length`.
pub struct ErrorMaskFilter {
    pub mask_enabled: bool,
}

impl ResponseFilter for ErrorMaskFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        if self.mask_enabled && resp.status.as_u16() >= 500 {
            return Ok(ResponseFilterOutcome::MaskBody);
        }
        Ok(ResponseFilterOutcome::Continue)
    }
}

// ── Phase 7: Rhai / WASM on_response middleware ───────────────────────────────

use crate::config::schema::MiddlewareEntry;

/// Phase 7 — Run Rhai and WASM middleware entries that are configured for the
/// response phase.
///
/// - **WASM** (`type: "wasm"`): if the module exports `on_response(status) -> i32`,
///   it is called here.  The export is optional — modules without it are skipped.
/// - **Rhai** (`type: "script"`, `phase: "response"`): runs the script with
///   `upstream.status`, `upstream.header("Name")`, `response.set_header()`, etc.
///
/// All header mutations collected by the plugins are applied to `resp`.
pub struct MiddlewareResponseFilter {
    pub middleware: Vec<MiddlewareEntry>,
}

impl ResponseFilter for MiddlewareResponseFilter {
    fn apply(
        &self,
        #[cfg_attr(not(any(feature = "rhai", feature = "wasm")), allow(unused_variables))]
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        // Status and headers are only needed by rhai/wasm plugins.
        #[cfg(any(feature = "rhai", feature = "wasm"))]
        let status = resp.status.as_u16();
        #[cfg(any(feature = "rhai", feature = "wasm"))]
        let headers: std::collections::HashMap<String, String> = resp
            .headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|vs| (k.as_str().to_ascii_lowercase(), vs.to_owned()))
            })
            .collect();

        for entry in &self.middleware {
            match entry.r#type.as_str() {
                // ── Rhai response scripts ─────────────────────────────────────
                #[cfg(feature = "rhai")]
                "script" => {
                    let phase = entry.phase.as_deref().unwrap_or("request");
                    if phase != "response" {
                        continue;
                    }
                    let Some(ref path) = entry.path else { continue };
                    let outcome = crate::filter::script::run_script_response(
                        path,
                        status,
                        headers.clone(),
                        entry.config.as_ref(),
                    );
                    apply_response_mutations(resp, outcome.added_headers, outcome.removed_headers);
                }

                // ── WASM on_response ──────────────────────────────────────────
                #[cfg(feature = "wasm")]
                "wasm" => {
                    let Some(ref path) = entry.path else { continue };
                    let plugin_config = entry
                        .config
                        .as_ref()
                        .and_then(|v| serde_json::to_vec(v).ok())
                        .unwrap_or_default();
                    let ctx = crate::filter::wasm::WasmResponseContext {
                        status,
                        headers: headers.clone(),
                        plugin_config,
                    };
                    let outcome = crate::filter::wasm::run_wasm_response(ctx, path);
                    apply_response_mutations(resp, outcome.added_headers, outcome.removed_headers);
                    if let Some(body_bytes) = outcome.body {
                        // Store the override body in the upstream_response_body
                        // override slot — handled by upstream_response_body_filter.
                        // We signal this via a custom header that the body filter reads.
                        // (Using a header is simpler than extending RequestCtx here.)
                        let _ = resp.insert_header(
                            "x-conduit-wasm-body-override",
                            format!("{}", body_bytes.len()),
                        );
                        // Store body bytes via header value (base64 for safety).
                        use base64::Engine as _;
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&body_bytes);
                        let _ = resp.insert_header("x-conduit-wasm-body-b64", encoded);
                    }
                }

                _ => {}
            }
        }

        Ok(ResponseFilterOutcome::Continue)
    }
}

/// Apply header mutations to a Pingora response header.
#[cfg(any(feature = "rhai", feature = "wasm"))]
fn apply_response_mutations(
    resp: &mut ResponseHeader,
    added: Vec<(String, String)>,
    removed: Vec<String>,
) {
    for name in removed {
        resp.remove_header(&name);
    }
    for (name, value) in added {
        let _ = resp.insert_header(name.clone(), value.as_str());
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;
    use pingora_http::ResponseHeader;

    fn make_resp(status: u16) -> ResponseHeader {
        ResponseHeader::build(StatusCode::from_u16(status).unwrap(), None).unwrap()
    }

    fn dummy_ctx() -> RequestCtx {
        use crate::proxy::ctx::UpstreamTarget;
        RequestCtx::new(
            0,
            UpstreamTarget::Proxy {
                addr: "localhost:4000".to_owned(),
                tls: false,
                sni: String::new(),
                strip_prefix: None,
                rewrite: None,
                mirror_url: None,
                upstream_tls: None,
            },
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
    }

    // ── CrlfProtectionFilter ─────────────────────────────────────────────────

    #[test]
    fn crlf_filter_passes_clean_headers() {
        // The CRLF filter strips headers whose values contain \r or \n.
        // Standard http API rejects CRLF values, so we test the "no-op" path:
        // clean headers must pass through unchanged.
        let mut resp = make_resp(200);
        resp.insert_header("x-custom", "clean-value").unwrap();
        resp.insert_header("content-type", "text/html").unwrap();
        let ctx = dummy_ctx();
        let filter = CrlfProtectionFilter {
            allow_duplicate_chunked: false,
        };
        let result = filter.apply(&mut resp, &ctx).unwrap();
        assert!(matches!(result, ResponseFilterOutcome::Continue));
        assert!(resp.headers.get("x-custom").is_some());
        assert!(resp.headers.get("content-type").is_some());
    }

    #[test]
    fn crlf_filter_keeps_clean_headers() {
        let mut resp = make_resp(200);
        resp.insert_header("content-type", "application/json")
            .unwrap();
        resp.insert_header("x-custom", "value").unwrap();
        let ctx = dummy_ctx();
        CrlfProtectionFilter {
            allow_duplicate_chunked: false,
        }
        .apply(&mut resp, &ctx)
        .unwrap();
        assert!(resp.headers.get("content-type").is_some());
        assert!(resp.headers.get("x-custom").is_some());
    }

    #[test]
    fn crlf_filter_deduplicates_chunked_encoding() {
        // Upstream sends Transfer-Encoding: chunked, chunked — should be normalised.
        let mut resp = make_resp(200);
        resp.insert_header("transfer-encoding", "chunked, chunked")
            .unwrap();
        let ctx = dummy_ctx();
        CrlfProtectionFilter {
            allow_duplicate_chunked: false,
        }
        .apply(&mut resp, &ctx)
        .unwrap();
        let te: Vec<_> = resp
            .headers
            .get_all("transfer-encoding")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(te, vec!["chunked"], "duplicate chunked deduplicated");
    }

    #[test]
    fn crlf_filter_allow_duplicate_chunked_passes_through() {
        // When allowDuplicateChunked is true the filter should not modify TE.
        let mut resp = make_resp(200);
        resp.insert_header("transfer-encoding", "chunked, chunked")
            .unwrap();
        let ctx = dummy_ctx();
        CrlfProtectionFilter {
            allow_duplicate_chunked: true,
        }
        .apply(&mut resp, &ctx)
        .unwrap();
        let te: Vec<_> = resp
            .headers
            .get_all("transfer-encoding")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(te, vec!["chunked, chunked"], "duplicate chunked preserved");
    }

    // ── InjectExtraHeadersFilter ─────────────────────────────────────────────

    #[test]
    fn inject_filter_adds_headers() {
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        InjectExtraHeadersFilter {
            headers: vec![
                ("x-served-by".to_owned(), "conduit".to_owned()),
                ("x-version".to_owned(), "1".to_owned()),
            ],
        }
        .apply(&mut resp, &ctx)
        .unwrap();
        assert_eq!(resp.headers.get("x-served-by").unwrap(), "conduit");
        assert_eq!(resp.headers.get("x-version").unwrap(), "1");
    }

    #[test]
    fn inject_filter_strips_pingora_server_banner() {
        let mut resp = make_resp(200);
        resp.insert_header("server", "Pingora").unwrap();
        let ctx = dummy_ctx();
        InjectExtraHeadersFilter { headers: vec![] }
            .apply(&mut resp, &ctx)
            .unwrap();
        assert!(
            resp.headers.get("server").is_none(),
            "Pingora banner removed"
        );
    }

    #[test]
    fn inject_filter_keeps_upstream_server_header() {
        let mut resp = make_resp(200);
        resp.insert_header("server", "nginx/1.24").unwrap();
        let ctx = dummy_ctx();
        InjectExtraHeadersFilter { headers: vec![] }
            .apply(&mut resp, &ctx)
            .unwrap();
        assert!(resp.headers.get("server").is_some(), "upstream server kept");
    }

    // ── ResponseTransformFilter ──────────────────────────────────────────────

    #[test]
    fn transform_filter_sets_and_removes() {
        let mut resp = make_resp(200);
        resp.insert_header("x-remove-me", "old").unwrap();
        let ctx = dummy_ctx();
        ResponseTransformFilter {
            transform: crate::config::schema::HeaderTransformConfig {
                set_headers: Some(
                    [("x-added".to_owned(), "yes".to_owned())]
                        .iter()
                        .cloned()
                        .collect(),
                ),
                remove_headers: Some(vec!["x-remove-me".to_owned()]),
            },
        }
        .apply(&mut resp, &ctx)
        .unwrap();
        assert!(resp.headers.get("x-remove-me").is_none());
        assert_eq!(resp.headers.get("x-added").unwrap(), "yes");
    }

    // ── RetryOnErrorFilter ───────────────────────────────────────────────────

    #[test]
    fn retry_triggers_on_5xx_with_budget_and_condition() {
        let mut resp = make_resp(503);
        let ctx = dummy_ctx();
        let r = RetryOnErrorFilter {
            retry: Some(RetrySpec {
                has_attempts_left: true,
                has_5xx_condition: true,
            }),
            stale_on_error: false,
        };
        assert!(matches!(
            r.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::RetryUpstream
        ));
    }

    #[test]
    fn retry_does_not_trigger_on_2xx() {
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        let r = RetryOnErrorFilter {
            retry: Some(RetrySpec {
                has_attempts_left: true,
                has_5xx_condition: true,
            }),
            stale_on_error: false,
        };
        assert!(matches!(
            r.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    #[test]
    fn retry_does_not_trigger_without_budget() {
        let mut resp = make_resp(500);
        let ctx = dummy_ctx();
        let r = RetryOnErrorFilter {
            retry: Some(RetrySpec {
                has_attempts_left: false,
                has_5xx_condition: true,
            }),
            stale_on_error: false,
        };
        assert!(matches!(
            r.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    #[test]
    fn retry_does_not_trigger_without_5xx_condition() {
        let mut resp = make_resp(500);
        let ctx = dummy_ctx();
        let r = RetryOnErrorFilter {
            retry: Some(RetrySpec {
                has_attempts_left: true,
                has_5xx_condition: false,
            }),
            stale_on_error: false,
        };
        assert!(matches!(
            r.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    // ── ErrorMaskFilter ──────────────────────────────────────────────────────

    #[test]
    fn mask_triggers_on_5xx_when_enabled() {
        let mut resp = make_resp(500);
        let ctx = dummy_ctx();
        let r = ErrorMaskFilter { mask_enabled: true };
        assert!(matches!(
            r.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::MaskBody
        ));
    }

    #[test]
    fn mask_skips_on_2xx() {
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        let r = ErrorMaskFilter { mask_enabled: true };
        assert!(matches!(
            r.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    #[test]
    fn mask_skips_when_disabled() {
        let mut resp = make_resp(500);
        let ctx = dummy_ctx();
        let r = ErrorMaskFilter {
            mask_enabled: false,
        };
        assert!(matches!(
            r.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    // ── ResponseFilterChain ──────────────────────────────────────────────────

    #[test]
    fn chain_stops_at_first_non_continue() {
        struct RetryAlways;
        impl ResponseFilter for RetryAlways {
            fn apply(
                &self,
                _: &mut ResponseHeader,
                _: &dyn ResponseCtx,
            ) -> Result<ResponseFilterOutcome> {
                Ok(ResponseFilterOutcome::RetryUpstream)
            }
        }
        struct ShouldNotRun;
        impl ResponseFilter for ShouldNotRun {
            fn apply(
                &self,
                _: &mut ResponseHeader,
                _: &dyn ResponseCtx,
            ) -> Result<ResponseFilterOutcome> {
                panic!("filter should not be called after terminal outcome")
            }
        }
        let chain = ResponseFilterChain::new()
            .push(RetryAlways)
            .push(ShouldNotRun);
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        assert!(matches!(
            chain.run(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::RetryUpstream
        ));
    }

    #[test]
    fn chain_returns_continue_when_all_pass() {
        let chain = ResponseFilterChain::new()
            .push(CrlfProtectionFilter {
                allow_duplicate_chunked: false,
            })
            .push(InjectExtraHeadersFilter { headers: vec![] });
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        assert!(matches!(
            chain.run(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    // ── ResponseTimeFilter ────────────────────────────────────────────────────

    #[test]
    fn response_time_filter_adds_header_when_enabled() {
        use std::time::Instant;
        let filter = ResponseTimeFilter {
            rt_cfg: Some(crate::config::schema::ResponseTimeConfig::Enabled(true)),
            start_time: Instant::now(),
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        assert!(
            resp.headers.get("x-response-time").is_some(),
            "x-response-time header must be injected when enabled"
        );
        let val = resp
            .headers
            .get("x-response-time")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(val.ends_with("ms"), "value must end with 'ms': {val}");
    }

    #[test]
    fn response_time_filter_skips_header_when_disabled() {
        use std::time::Instant;
        let filter = ResponseTimeFilter {
            rt_cfg: Some(crate::config::schema::ResponseTimeConfig::Enabled(false)),
            start_time: Instant::now(),
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        assert!(
            resp.headers.get("x-response-time").is_none(),
            "x-response-time must NOT be added when disabled"
        );
    }

    #[test]
    fn response_time_filter_skips_header_when_cfg_none() {
        use std::time::Instant;
        let filter = ResponseTimeFilter {
            rt_cfg: None,
            start_time: Instant::now(),
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        assert!(
            resp.headers.get("x-response-time").is_none(),
            "no cfg → no header"
        );
    }

    #[test]
    fn response_time_filter_with_decimal_digits() {
        use crate::config::schema::{ResponseTimeConfig, ResponseTimeOptions};
        use std::time::Instant;
        let filter = ResponseTimeFilter {
            rt_cfg: Some(ResponseTimeConfig::Options(ResponseTimeOptions {
                digits: Some(2),
            })),
            start_time: Instant::now(),
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        let val = resp
            .headers
            .get("x-response-time")
            .expect("header must be present")
            .to_str()
            .unwrap();
        // Must end with "ms" and contain a decimal point.
        assert!(val.ends_with("ms"), "must end with ms: {val}");
        // With 2 digits the value may be "0.00ms" for very fast calls.
        assert!(val.contains('.'), "must contain decimal point: {val}");
    }

    // ── RetryOnErrorFilter edge cases ─────────────────────────────────────────

    #[test]
    fn retry_none_config_never_retries() {
        let filter = RetryOnErrorFilter {
            retry: None,
            stale_on_error: false,
        };
        let mut resp = make_resp(503);
        let ctx = dummy_ctx();
        assert!(matches!(
            filter.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    #[test]
    fn stale_on_error_triggers_retry_upstream_on_5xx() {
        // When stale_on_error=true and no retry config, a 5xx should trigger
        // RetryUpstream so Pingora can call should_serve_stale().
        let filter = RetryOnErrorFilter {
            retry: None,
            stale_on_error: true,
        };
        let mut resp = make_resp(503);
        let ctx = dummy_ctx();
        assert!(matches!(
            filter.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::RetryUpstream
        ));
    }

    #[test]
    fn stale_on_error_does_not_trigger_on_2xx() {
        // stale_on_error should not affect successful responses.
        let filter = RetryOnErrorFilter {
            retry: None,
            stale_on_error: true,
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        assert!(matches!(
            filter.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    #[test]
    fn stale_on_error_with_retry_exhausted_still_triggers() {
        // Even when retry budget is exhausted (has_attempts_left=false),
        // stale_on_error should still trigger RetryUpstream for stale cache.
        let filter = RetryOnErrorFilter {
            retry: Some(RetrySpec {
                has_attempts_left: false,
                has_5xx_condition: true,
            }),
            stale_on_error: true,
        };
        let mut resp = make_resp(500);
        let ctx = dummy_ctx();
        assert!(matches!(
            filter.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::RetryUpstream
        ));
    }

    #[test]
    fn mask_triggers_on_502() {
        let filter = ErrorMaskFilter { mask_enabled: true };
        let mut resp = make_resp(502);
        let ctx = dummy_ctx();
        assert!(matches!(
            filter.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::MaskBody
        ));
    }

    #[test]
    fn mask_triggers_on_504() {
        let filter = ErrorMaskFilter { mask_enabled: true };
        let mut resp = make_resp(504);
        let ctx = dummy_ctx();
        assert!(matches!(
            filter.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::MaskBody
        ));
    }

    #[test]
    fn mask_skips_on_404() {
        // 4xx is not a server error, should not be masked.
        let filter = ErrorMaskFilter { mask_enabled: true };
        let mut resp = make_resp(404);
        let ctx = dummy_ctx();
        assert!(matches!(
            filter.apply(&mut resp, &ctx).unwrap(),
            ResponseFilterOutcome::Continue
        ));
    }

    // ── ServerTimingFilter ────────────────────────────────────────────────────

    #[test]
    fn server_timing_filter_total_only_when_no_upstream() {
        let filter = ServerTimingFilter {
            start_time: std::time::Instant::now(),
            upstream_start: None,
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        let val = resp
            .headers
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(val.starts_with("total;dur="), "must have total: {val}");
        assert!(
            !val.contains("upstream"),
            "no upstream when upstream_start is None"
        );
    }

    #[test]
    fn server_timing_filter_includes_upstream_when_set() {
        let start = std::time::Instant::now();
        let filter = ServerTimingFilter {
            start_time: start,
            upstream_start: Some(start),
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        let val = resp
            .headers
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(val.contains("total;dur="), "must have total");
        assert!(val.contains("upstream;dur="), "must have upstream: {val}");
    }

    // ── InjectExtraHeadersFilter — Age header (RFC 7234 §5.1, #49) ──────────

    #[cfg(feature = "cache")]
    #[test]
    fn inject_filter_injects_age_header_when_cache_age_set() {
        // When RequestCtx.cache_age_secs() is Some, the filter must inject
        // an `Age` header with the computed value.
        let filter = InjectExtraHeadersFilter { headers: vec![] };
        let mut resp = make_resp(200);
        let mut ctx = dummy_ctx();
        ctx.cache = Some(conduit_cache::CacheReqState {
            cache_age_secs: Some(42),
            ..Default::default()
        });
        filter.apply(&mut resp, &ctx).unwrap();
        assert_eq!(
            resp.headers.get("age").and_then(|v| v.to_str().ok()),
            Some("42"),
            "Age header must be injected with computed value"
        );
    }

    #[test]
    fn inject_filter_no_age_header_when_not_cache_hit() {
        // cache_age_secs = None (non-cached response) → no Age header added.
        let filter = InjectExtraHeadersFilter { headers: vec![] };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx(); // cache_age_secs defaults to None
        filter.apply(&mut resp, &ctx).unwrap();
        assert!(
            resp.headers.get("age").is_none(),
            "Age header must not be injected for non-cached responses"
        );
    }

    #[cfg(feature = "cache")]
    #[test]
    fn inject_filter_replaces_existing_age_header() {
        // When the cached response already carries an Age header from a prior
        // hop, it must be replaced with the freshly computed value to prevent
        // double-counting.
        let filter = InjectExtraHeadersFilter { headers: vec![] };
        let mut resp = make_resp(200);
        resp.insert_header("age", "10").unwrap(); // stale Age from stored response
        let mut ctx = dummy_ctx();
        ctx.cache = Some(conduit_cache::CacheReqState {
            cache_age_secs: Some(75),
            ..Default::default()
        });
        filter.apply(&mut resp, &ctx).unwrap();
        let values: Vec<_> = resp.headers.get_all("age").iter().collect();
        assert_eq!(values.len(), 1, "exactly one Age header must remain");
        assert_eq!(
            values[0].to_str().unwrap(),
            "75",
            "Age header must reflect the recomputed value, not the stale one"
        );
    }

    // ── InjectExtraHeadersFilter edge cases ───────────────────────────────────

    #[test]
    fn inject_filter_empty_headers_is_noop() {
        let filter = InjectExtraHeadersFilter { headers: vec![] };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        // No extra headers added, no crash.
        assert!(resp.headers.get("x-custom").is_none());
    }

    #[test]
    fn inject_filter_multiple_headers_all_added() {
        let filter = InjectExtraHeadersFilter {
            headers: vec![
                ("x-a".to_owned(), "1".to_owned()),
                ("x-b".to_owned(), "2".to_owned()),
                ("x-c".to_owned(), "3".to_owned()),
            ],
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        assert_eq!(resp.headers.get("x-a").unwrap(), "1");
        assert_eq!(resp.headers.get("x-b").unwrap(), "2");
        assert_eq!(resp.headers.get("x-c").unwrap(), "3");
    }

    // ── ResponseFilterChain::build ────────────────────────────────────────────

    #[test]
    fn build_basic_chain_works_without_site() {
        use crate::config::schema::AppConfig;
        // build() with no matching site (site_idx=999) must not panic.
        let ctx = dummy_ctx();
        let config = AppConfig::default();
        let chain = ResponseFilterChain::build(&ctx, &config);
        let mut resp = make_resp(200);
        // Must run without panicking.
        chain.run(&mut resp, &ctx).unwrap();
    }

    #[test]
    fn build_chain_with_mask_errors_enabled() {
        use crate::config::schema::AppConfig;
        use crate::config::schema::SiteConfig;
        let mut config = AppConfig::default();
        config.sites.push(SiteConfig {
            mask_errors: Some(true),
            ..Default::default()
        });
        let mut ctx = dummy_ctx();
        ctx.site_idx = 0;
        let chain = ResponseFilterChain::build(&ctx, &config);
        // A 500 response should be masked.
        let mut resp = make_resp(500);
        let outcome = chain.run(&mut resp, &ctx).unwrap();
        assert!(
            matches!(outcome, ResponseFilterOutcome::MaskBody),
            "mask_errors=true + 5xx → MaskBody"
        );
    }

    // ── MiddlewareResponseFilter ──────────────────────────────────────────────

    #[test]
    fn middleware_response_filter_empty_middleware_is_noop() {
        let filter = MiddlewareResponseFilter { middleware: vec![] };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        let outcome = filter.apply(&mut resp, &ctx).unwrap();
        assert!(matches!(outcome, ResponseFilterOutcome::Continue));
    }

    #[test]
    #[cfg(feature = "rhai")]
    fn middleware_response_filter_request_phase_script_is_skipped() {
        use crate::config::schema::MiddlewareEntry;
        // A script with phase="request" must be skipped in response phase.
        let filter = MiddlewareResponseFilter {
            middleware: vec![MiddlewareEntry {
                r#type: "script".to_owned(),
                path: Some("nonexistent.rhai".to_owned()),
                phase: Some("request".to_owned()), // request phase → skip in response
                config: None,
            }],
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        // Must not panic even though the file doesn't exist (script skipped).
        let outcome = filter.apply(&mut resp, &ctx).unwrap();
        assert!(matches!(outcome, ResponseFilterOutcome::Continue));
    }

    // ── ResponseTransformFilter edge cases ────────────────────────────────────

    #[test]
    fn transform_filter_no_op_when_empty_config() {
        use crate::config::schema::HeaderTransformConfig;
        let filter = ResponseTransformFilter {
            transform: HeaderTransformConfig {
                set_headers: None,
                remove_headers: None,
            },
        };
        let mut resp = make_resp(200);
        resp.insert_header("x-keep", "yes").unwrap();
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        // Existing header should be preserved.
        assert_eq!(resp.headers.get("x-keep").unwrap(), "yes");
    }

    #[test]
    fn transform_filter_only_remove() {
        use crate::config::schema::HeaderTransformConfig;
        let filter = ResponseTransformFilter {
            transform: HeaderTransformConfig {
                set_headers: None,
                remove_headers: Some(vec!["x-remove".to_owned()]),
            },
        };
        let mut resp = make_resp(200);
        resp.insert_header("x-remove", "bye").unwrap();
        resp.insert_header("x-keep", "yes").unwrap();
        let ctx = dummy_ctx();
        filter.apply(&mut resp, &ctx).unwrap();
        assert!(
            resp.headers.get("x-remove").is_none(),
            "x-remove must be gone"
        );
        assert_eq!(resp.headers.get("x-keep").unwrap(), "yes");
    }

    // ── apply_response_mutations ──────────────────────────────────────────────

    #[test]
    #[cfg(any(feature = "rhai", feature = "wasm"))]
    fn apply_response_mutations_adds_and_removes() {
        let mut resp = make_resp(200);
        resp.insert_header("x-old", "value").unwrap();
        apply_response_mutations(
            &mut resp,
            vec![("x-new".to_owned(), "injected".to_owned())],
            vec!["x-old".to_owned()],
        );
        assert!(resp.headers.get("x-old").is_none(), "x-old must be removed");
        assert_eq!(resp.headers.get("x-new").unwrap(), "injected");
    }

    #[test]
    #[cfg(any(feature = "rhai", feature = "wasm"))]
    fn apply_response_mutations_empty_vecs_is_noop() {
        let mut resp = make_resp(200);
        resp.insert_header("x-keep", "yes").unwrap();
        apply_response_mutations(&mut resp, vec![], vec![]);
        assert_eq!(resp.headers.get("x-keep").unwrap(), "yes");
    }

    // ── dedup_chunked_transfer_encoding ──────────────────────────────────────

    #[test]
    fn dedup_te_no_op_when_no_te_header() {
        let mut resp = make_resp(200);
        dedup_chunked_transfer_encoding(&mut resp);
        assert!(resp.headers.get("transfer-encoding").is_none());
    }

    #[test]
    fn dedup_te_no_op_for_single_non_comma_chunked() {
        let mut resp = make_resp(200);
        resp.insert_header("transfer-encoding", "chunked").unwrap();
        dedup_chunked_transfer_encoding(&mut resp);
        let vals: Vec<_> = resp
            .headers
            .get_all("transfer-encoding")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(vals, vec!["chunked"]);
    }

    #[test]
    fn dedup_te_removes_duplicate_chunked_headers() {
        let mut resp = make_resp(200);
        // Append two separate Transfer-Encoding: chunked headers.
        resp.headers.append(
            "transfer-encoding",
            http::header::HeaderValue::from_static("chunked"),
        );
        resp.headers.append(
            "transfer-encoding",
            http::header::HeaderValue::from_static("chunked"),
        );
        dedup_chunked_transfer_encoding(&mut resp);
        let vals: Vec<_> = resp
            .headers
            .get_all("transfer-encoding")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(vals.len(), 1, "exactly one TE header expected after dedup");
        assert_eq!(vals[0], "chunked");
    }

    #[test]
    fn dedup_te_collapses_comma_separated_duplicates() {
        let mut resp = make_resp(200);
        resp.insert_header("transfer-encoding", "chunked, chunked")
            .unwrap();
        dedup_chunked_transfer_encoding(&mut resp);
        let vals: Vec<_> = resp
            .headers
            .get_all("transfer-encoding")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(vals.len(), 1);
        assert_eq!(
            vals[0].to_lowercase().matches("chunked").count(),
            1,
            "chunked should appear exactly once"
        );
    }

    #[test]
    fn dedup_te_preserves_other_directives() {
        let mut resp = make_resp(200);
        resp.insert_header("transfer-encoding", "gzip, chunked, chunked")
            .unwrap();
        dedup_chunked_transfer_encoding(&mut resp);
        let val = resp
            .headers
            .get("transfer-encoding")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            val.contains("gzip"),
            "gzip directive must be preserved: {val}"
        );
        assert_eq!(
            val.to_lowercase().matches("chunked").count(),
            1,
            "chunked must appear exactly once: {val}"
        );
    }
}
