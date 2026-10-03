//! Boundary types for proxy-target resolution (issue #143 — introduced in
//! PR A2, issue #419, moved here in PR B, issue #143 itself).
//!
//! [`ProxyOutcome`]/[`ProxyResolution`] replace what used to be a full
//! `RouteResolution.upstream: UpstreamTarget` return shape for the *inner*
//! resolution functions ([`crate::resolve`]/`crate::groups`/
//! `crate::routes_resolve`) — those functions used to build a full
//! `RouteResolution`, which embeds `UpstreamTarget::Local(LocalHandler)`.
//! `LocalHandler` is the vocabulary shared by the root crate's health/
//! metrics/ACME/hot-reload/static/upload local handlers (`CLAUDE.md`
//! decision #21) and must stay in the root crate — it cannot cross into this
//! crate the way `ProxyUpstream` can. Narrowing the inner functions' return
//! type to `ProxyOutcome`/`ProxyResolution` is what let PR B move them here
//! without also having to somehow split `LocalHandler` out of the root
//! crate first.
//!
//! The root-crate glue code (`router.rs`'s `route_site`/`resolve_legacy_proxy`/
//! `resolve_routes_array`, `routes.rs`'s `match_routes`) still builds the
//! final `RouteResolution` — it destructures a [`ProxyUpstream`] into the
//! existing `UpstreamTarget::Proxy { .. }` variant at that single glue point,
//! rather than `UpstreamTarget::Proxy` itself being changed to wrap
//! `ProxyUpstream` (which would churn every existing `UpstreamTarget::Proxy
//! { addr, tls, sni, .. }` match site in `request_phase.rs`/`response_phase.rs`
//! for no benefit).
//!
//! ## `url_to_proxy_upstream` is a deliberate small duplicate, not a shared call
//!
//! This module's own private `url_to_proxy_upstream` has the same URL-parsing body
//! as the root crate's `router::url_to_proxy_upstream`, just returning
//! [`ProxyUpstream`] instead of `UpstreamTarget`. Before this crate existed,
//! every routing-resolution function called the root crate's
//! `router::url_to_proxy_upstream` and then converted its `UpstreamTarget`
//! result down to a `ProxyUpstream` via a conversion helper that lived here.
//! That's no longer possible: this crate cannot call back into the root
//! crate that depends on it. Rather than teach the root's
//! `url_to_proxy_upstream` to take/return `ProxyUpstream` (which would force
//! `router.rs`'s own tests and its `ProxyConfig::Single` call site to change
//! for no functional reason), this ~10-line helper is duplicated here with
//! the narrower return type — `ProxyUpstream`'s fields are already
//! byte-identical to `UpstreamTarget::Proxy`'s, so the two bodies are
//! trivially kept in sync by inspection.

use conduit_upstream::UpstreamTlsConfig;

use crate::config::RewriteRule;
use crate::state::ProxyReqState;

/// The upstream a proxy route resolved to — byte-identical payload to the
/// root crate's `UpstreamTarget::Proxy`'s struct-variant fields
/// (`src/proxy/ctx.rs`).
#[derive(Debug)]
pub struct ProxyUpstream {
    pub addr: String,
    pub tls: bool,
    pub sni: String,
    pub strip_prefix: Option<String>,
    pub rewrite: Option<Vec<RewriteRule>>,
    pub mirror_url: Option<String>,
    pub upstream_tls: Option<UpstreamTlsConfig>,
}

/// Outcome of resolving one proxy route's target.
#[derive(Debug)]
pub enum ProxyOutcome {
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
    /// #415 (see `crate::resolve::resolve_proxy_routes`'s doc comment).
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
pub struct ProxyResolution {
    pub outcome: ProxyOutcome,
    pub state: ProxyReqState,
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
    #[cfg(feature = "proxy")]
    pub(crate) fn overloaded(state: ProxyReqState) -> Self {
        Self {
            outcome: ProxyOutcome::Overloaded,
            state,
        }
    }

    /// Convenience constructor for the `Upstream` outcome.
    #[cfg(feature = "proxy")]
    pub(crate) fn upstream(upstream: ProxyUpstream, state: ProxyReqState) -> Self {
        Self {
            outcome: ProxyOutcome::Upstream(upstream),
            state,
        }
    }
}

/// Convert a target URL + optional strip prefix into a [`ProxyUpstream`].
///
/// See this module's own doc comment ("`url_to_proxy_upstream` is a
/// deliberate small duplicate") for why this isn't shared with the root
/// crate's identically-shaped `router::url_to_proxy_upstream`.
#[cfg(feature = "proxy")]
pub(crate) fn url_to_proxy_upstream(
    url: &str,
    strip_prefix: Option<String>,
) -> Option<ProxyUpstream> {
    let addr = conduit_upstream::targets::url_to_host_port(url)?;
    let tls = conduit_upstream::targets::url_is_tls(url);
    let sni = if tls {
        conduit_upstream::targets::url_host(url)
    } else {
        String::new()
    };
    Some(ProxyUpstream {
        addr,
        tls,
        sni,
        strip_prefix,
        rewrite: None,
        mirror_url: None,
        upstream_tls: None,
    })
}
