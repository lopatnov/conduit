//! Chain of Responsibility vocabulary for request guard filters.
//!
//! The concrete [`FilterChain`] assembly and every concrete guard (IP
//! filter, rate limiting, auth, ...) live in the root crate's
//! `src/filter/chain.rs`, since they name config-schema types this crate
//! must not know about. This module owns only the abstract contract:
//! [`RequestFilter`], [`FilterContext`], and [`FilterOutcome`].

use std::sync::atomic::AtomicUsize;

use async_trait::async_trait;
use pingora_core::Result;
use pingora_proxy::Session;

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

/// Data shared by every filter in a single request's guard chain.
///
/// Deliberately narrow: any state read by only one guard (e.g. the shared
/// rate limiter, per-IP connection counts) lives on that guard's own struct
/// instead, so this type — and the [`RequestFilter`] trait built on it —
/// stays Layer-0-clean (see #114/#126).
pub struct FilterContext<'a> {
    /// The live HTTP session — filters may read headers and write responses.
    pub session: &'a mut Session,
    /// Headers to attach to any rejection response (CORS, security, custom).
    pub extra_headers: &'a [(String, String)],
    /// Inflight counter; each filter that writes a rejection response
    /// decrements it.
    pub inflight: &'a AtomicUsize,
}

/// A single guard filter in the request pipeline.
#[async_trait]
pub trait RequestFilter: Send + Sync {
    /// Inspect the request and decide whether to continue, reject, or bypass.
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome>;
}
