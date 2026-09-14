//! Boundary types for proxy-target resolution (issue #143, PR A2 of a 3-PR
//! plan).
//!
//! [`ProxyOutcome`]/[`ProxyResolution`] replace the previous
//! `RouteResolution.upstream: UpstreamTarget` return shape for the *inner*
//! resolution functions (`routing::resolve`/`routing::groups`/
//! `routing::routes_resolve`) — those functions used to build a full
//! `RouteResolution`, which embeds `UpstreamTarget::Local(LocalHandler)`.
//! `LocalHandler` is the vocabulary shared by the root crate's health/
//! metrics/ACME/hot-reload/static/upload local handlers (`CLAUDE.md`
//! decision #21) and must stay in the root crate — it cannot cross into the
//! future `conduit-proxy-http` crate (issue #143 PR B) the way `ProxyUpstream`
//! can. Narrowing the inner functions' return type to `ProxyOutcome`/
//! `ProxyResolution` now means PR B can move them without also having to
//! somehow split `LocalHandler` out of the root crate first.
//!
//! The root-crate glue code (`router.rs`'s `route_site`/`resolve_legacy_proxy`/
//! `resolve_routes_array`, `routes.rs`'s `match_routes`) still builds the
//! final `RouteResolution` — it destructures a `ProxyUpstream` into the
//! existing `UpstreamTarget::Proxy { .. }` variant at that single glue point,
//! rather than `UpstreamTarget::Proxy` itself being changed to wrap
//! `ProxyUpstream` (which would churn every existing `UpstreamTarget::Proxy
//! { addr, tls, sni, .. }` match site in `request_phase.rs`/`response_phase.rs`/
//! `routes.rs` for no benefit — see this PR's own description).

use crate::config::schema::{RewriteRule, UpstreamTlsConfig};
use crate::proxy::ctx::UpstreamTarget;
use crate::proxy::routing::state::ProxyReqState;

/// The upstream a proxy route resolved to — byte-identical payload to
/// `UpstreamTarget::Proxy`'s struct-variant fields (`src/proxy/ctx.rs`).
#[derive(Debug)]
pub(crate) struct ProxyUpstream {
    pub(crate) addr: String,
    pub(crate) tls: bool,
    pub(crate) sni: String,
    pub(crate) strip_prefix: Option<String>,
    pub(crate) rewrite: Option<Vec<RewriteRule>>,
    pub(crate) mirror_url: Option<String>,
    pub(crate) upstream_tls: Option<UpstreamTlsConfig>,
}

/// Outcome of resolving one proxy route's target.
#[derive(Debug)]
pub(crate) enum ProxyOutcome {
    /// Forward to this upstream.
    Upstream(ProxyUpstream),
    /// Circuit open (every healthy peer at its connection cap, #156) or
    /// `sticky.strict` with an unhealthy pin (#39). Root maps this to
    /// `LocalHandler::Overloaded` → 503.
    Overloaded,
    /// A route matched but produced no usable upstream: malformed URL, empty
    /// healthy set, or capacity resolution returned nothing. This replaces
    /// the bare `None`/`fallback_result()` the two matchers used to return —
    /// carrying [`ProxyResolution::state`] through this case is what fixes
    /// #415 (see `routing::resolve::resolve_proxy_routes`'s doc comment).
    Unresolved,
}

/// Result of resolving one proxy route: the outcome, plus the per-request
/// [`ProxyReqState`] accumulated while resolving it (retry state, per-route
/// timeout/pool/http2, sticky cookie, rate-limit/priority stamp, ...).
///
/// `state` is populated regardless of `outcome` — including `Unresolved` —
/// so a route's rate-limit/priority stamp survives even when its target
/// fails to resolve (#415).
#[derive(Debug)]
pub(crate) struct ProxyResolution {
    pub(crate) outcome: ProxyOutcome,
    pub(crate) state: ProxyReqState,
}

impl ProxyResolution {
    /// Convenience constructor for the `Unresolved` outcome.
    pub(crate) fn unresolved(state: ProxyReqState) -> Self {
        Self {
            outcome: ProxyOutcome::Unresolved,
            state,
        }
    }

    /// Convenience constructor for the `Overloaded` outcome.
    pub(crate) fn overloaded(state: ProxyReqState) -> Self {
        Self {
            outcome: ProxyOutcome::Overloaded,
            state,
        }
    }

    /// Convenience constructor for the `Upstream` outcome.
    pub(crate) fn upstream(upstream: ProxyUpstream, state: ProxyReqState) -> Self {
        Self {
            outcome: ProxyOutcome::Upstream(upstream),
            state,
        }
    }
}

/// Convert `router::url_to_proxy_upstream`'s `Option<UpstreamTarget>` result
/// into a [`ProxyUpstream`] for [`ProxyOutcome::Upstream`].
///
/// `url_to_proxy_upstream` only ever returns `Some(UpstreamTarget::Proxy
/// { .. })` or `None` (never `Local`/`Upload`) — confirmed by reading its
/// implementation — so `None` here means either the input itself was `None`
/// (malformed URL) or, in principle, some other `UpstreamTarget` variant that
/// never actually occurs in practice.
pub(crate) fn upstream_target_into_proxy_upstream(target: UpstreamTarget) -> Option<ProxyUpstream> {
    match target {
        UpstreamTarget::Proxy {
            addr,
            tls,
            sni,
            strip_prefix,
            rewrite,
            mirror_url,
            upstream_tls,
        } => Some(ProxyUpstream {
            addr,
            tls,
            sni,
            strip_prefix,
            rewrite,
            mirror_url,
            upstream_tls,
        }),
        _ => None,
    }
}
