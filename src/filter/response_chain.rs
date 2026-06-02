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

// ── Outcome ───────────────────────────────────────────────────────────────────

/// What a response filter returns after processing.
pub enum ResponseFilterOutcome {
    /// Apply the changes and continue to the next filter.
    Continue,
    /// This response should be retried against the upstream.
    ///
    /// Returned by `RetryOnErrorFilter` on 5xx when retries are configured.
    /// The caller must propagate a Pingora `Custom("5xx_retry")` error.
    RetryUpstream,
    /// The response body should be replaced with a generic error JSON.
    ///
    /// Returned by `ErrorMaskFilter` on 5xx when `maskErrors: true` is set.
    /// The caller sets `RequestCtx.mask_upstream_body = true` and updates
    /// the `Content-Type` / `Content-Length` headers.
    MaskBody,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A single phase in the response filter pipeline.
pub trait ResponseFilter: Send + Sync {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        req_ctx: &RequestCtx,
    ) -> Result<ResponseFilterOutcome>;
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
        req_ctx: &RequestCtx,
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

        // Phase 1 — Security: strip header-injection characters.
        chain = chain.push(CrlfProtectionFilter);

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

        // Phase 5 — 5xx retry (terminates chain if fired).
        chain = chain.push(RetryOnErrorFilter {
            retry: req_ctx.retry.as_ref().map(|r| RetrySpec {
                has_attempts_left: r.has_attempts_left(),
                has_5xx_condition: r.has_condition("5xx"),
            }),
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

/// Phase 1 — Strip response headers whose values contain CR or LF characters.
///
/// Prevents header-injection (CRLF injection) attacks where an upstream
/// embeds newlines in header values to splice additional HTTP headers into
/// the client response.
pub struct CrlfProtectionFilter;

impl ResponseFilter for CrlfProtectionFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &RequestCtx,
    ) -> Result<ResponseFilterOutcome> {
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
        Ok(ResponseFilterOutcome::Continue)
    }
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
        _req_ctx: &RequestCtx,
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
        _req_ctx: &RequestCtx,
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
        _req_ctx: &RequestCtx,
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

/// Retry specification passed to `RetryOnErrorFilter` without a borrow
/// of `RetryState` (which would create a lifetime dependency on `RequestCtx`).
pub struct RetrySpec {
    pub has_attempts_left: bool,
    pub has_5xx_condition: bool,
}

/// Phase 5 — Trigger a Pingora retry when the upstream returns a 5xx status
/// and the route has a `retry` config with `"5xx"` in its conditions list.
///
/// Returns `RetryUpstream` (terminal) — the caller must propagate a Pingora
/// `Custom("5xx_retry")` error to activate the retry machinery.
pub struct RetryOnErrorFilter {
    pub retry: Option<RetrySpec>,
}

impl ResponseFilter for RetryOnErrorFilter {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        _req_ctx: &RequestCtx,
    ) -> Result<ResponseFilterOutcome> {
        if let Some(spec) = &self.retry {
            if resp.status.as_u16() >= 500 && spec.has_attempts_left && spec.has_5xx_condition {
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
        _req_ctx: &RequestCtx,
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
        _req_ctx: &RequestCtx,
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
