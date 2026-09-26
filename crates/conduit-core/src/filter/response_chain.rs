//! Phase-ordered response pipeline vocabulary (symmetric to `filter::chain`).
//!
//! The concrete [`ResponseFilterChain`]-equivalent assembly (`build()`, which
//! reads `AppConfig`/`SiteConfig`) and every concrete phase filter (CRLF
//! protection, header injection, response transform, ...) live in the root
//! crate's `src/filter/response_chain.rs`. This module owns only the
//! abstract contract: [`ResponseFilter`], [`ResponseFilterOutcome`], and the
//! narrow [`ResponseCtx`] view.
//!
//! [`ResponseFilterChain`]: https://docs.rs/lopatnov-conduit (root crate)

use pingora_core::Result;
use pingora_http::ResponseHeader;

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

/// Narrow read-only view of request context exposed to [`ResponseFilter::apply`].
///
/// `ResponseFilter` implementors currently only ever read `cache_age_secs`
/// from the request context (every other filter captures what it needs into
/// its own struct fields at chain-build time instead of reading the request
/// context per-request). This trait exists so `ResponseFilter` itself
/// doesn't name the concrete `RequestCtx` type, which carries feature-gated
/// fields (JWT claims, WASM plugin state, etc.) that belong to higher layers
/// — unblocking `ResponseFilter` from living in this Layer-0 core crate
/// (#114/#120/#126).
pub trait ResponseCtx: Send + Sync {
    /// Age (seconds) of a cache-hit response, for the `Age` response header
    /// (RFC 7234 §5.1, #49). `None` for non-cached responses.
    fn cache_age_secs(&self) -> Option<u64>;
}

/// A single phase in the response filter pipeline.
pub trait ResponseFilter: Send + Sync {
    fn apply(
        &self,
        resp: &mut ResponseHeader,
        req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome>;
}
