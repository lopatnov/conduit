//! Per-request proxy-routing types shared across [`ConduitProxy`]'s
//! request-processing pipeline, split out of the former monolithic
//! `request_phase.rs` (2942 production lines) ahead of issue #144's
//! `proxy` feature-gating work (PR 1 of 2 -- pure file reorganization,
//! zero behavior change).
//!
//! - [`filter`] -- `do_request_filter` orchestration, the guard chain,
//!   post-routing rate-limit/priority-shedding helpers, and the
//!   [`pingora_proxy::ProxyHttp::request_filter`] trait-method body.
//! - [`handlers`] -- `dispatch_local` / `build_handler` local-handler
//!   dispatch, `collect_upstream_infos`, and the `OverloadedHandler` /
//!   `PlainNotFoundHandler` local handlers.
//! - [`retry`] -- retry decision logic (`try_retry_connect_error` /
//!   `try_retry_proxy_error`), retry bookkeeping (`record_failed_upstream_for_retry`,
//!   `retry_budget_allows`), conn-slot acquire/release (#216), and
//!   `select_retry_target` (#216 part 2).
//! - [`peer`] -- `upstream_peer`, `resolve_peer_addr`, `apply_peer_options`.
//! - [`dns`] -- the hostname resolution cache (#232) and
//!   `filter_preferred_family`.
//! - [`body`] -- `request_body_filter` and request-body buffering helpers.
//! - [`transform`] -- `upstream_request_filter`, path rewriting, header
//!   transforms, forwarded-header injection, and mirroring.
//! - [`cache`] -- `request_cache_filter`, `should_serve_stale`,
//!   `cache_key_callback`.
//!
//! `HandlerKind` / `GuardCtx` / [`handler_kind_of`] stay here since every
//! submodule above needs them.

mod body;
mod cache;
// The hostname-resolution cache is only reachable from `peer.rs`'s upstream-peer path,
// which exists whenever a proxied upstream (`proxy`) or the internal upload loopback
// upstream (`upload`) can be selected (issue #144).
#[cfg(any(feature = "proxy", feature = "upload"))]
mod dns;
mod filter;
mod handlers;
mod peer;
mod retry;
mod transform;

pub(crate) use body::request_body_filter;
pub(crate) use cache::{cache_key_callback, request_cache_filter, should_serve_stale};
pub(crate) use filter::request_filter;
pub(crate) use peer::upstream_peer;
pub(crate) use retry::{error_while_proxy, fail_to_connect};
pub(crate) use transform::upstream_request_filter;

use crate::config::schema::{
    ApiKeyConfig, BasicAuthConfig, CorsConfig, IpFilterConfig, LimitsConfig, MiddlewareEntry,
    RateLimitConfig,
};
use crate::proxy::ctx::{LocalHandler, UpstreamTarget};

#[derive(Clone)]
pub(crate) enum HandlerKind {
    Health,
    AcmeChallenge,
    Metrics,
    StaticFile,
    Fallback,
    HotReloadSse,
    HotReloadJs,
    Proxy,
    /// All upstream connections are at the configured max limit — circuit open.
    /// Returns 503 Service Unavailable without contacting any upstream.
    Overloaded,
}

/// All per-request guard data bundled into one value to keep `run_guard_filters`
/// within clippy's argument-count limit (7).
// Fields that are only read when optional features are compiled in.
// Allow dead_code for the base (no-feature) build — they ARE used with --features full.
#[allow(dead_code)]
pub(crate) struct GuardCtx {
    ip_cfg: Option<IpFilterConfig>,
    limits_cfg: Option<LimitsConfig>,
    /// Security headers config — used by `AllowedHostsGuard`.
    security_cfg: Option<crate::config::schema::SecurityHeadersConfig>,
    /// The matched site's own `host:` config value — used by `AllowedHostsGuard`
    /// as a default-safe fallback when `allowedHosts` is not explicitly set.
    site_host: Option<String>,
    /// Incoming `Host` header value — checked against `allowedHosts`.
    host: String,
    rate_limit_cfg: Option<RateLimitConfig>,
    basic_auth_cfg: Option<BasicAuthConfig>,
    api_key_cfg: Option<ApiKeyConfig>,
    cors_cfg: Option<CorsConfig>,
    redirect_result: Option<(String, u16)>,
    /// Middleware chain entries — Rhai `type: "script"` entries are executed
    /// after the built-in filters and redirects.
    middleware: Vec<MiddlewareEntry>,
    handler_kind: HandlerKind,
    is_preflight: bool,
    sec_only: Vec<(String, String)>,
    origin: Option<String>,
    extra_headers: Vec<(String, String)>,
    /// Request info forwarded to Rhai scripts and WASM plugins.
    script_method: String,
    script_path: String,
    script_query: String,
    script_headers: std::collections::HashMap<String, String>,
    /// Remote client IP — used by WASM plugins.
    client_ip: String,
    /// Fault injection config (chaos testing).
    /// Field always present; guard only pushed when `--features fault-injection`.
    fault_injection_cfg: Option<crate::config::schema::FaultInjectionConfig>,
    /// JWT auth config — validated in step 6c.
    jwt_auth_cfg: Option<crate::config::schema::JwtAuthConfig>,
    /// Forward-auth config — validated in step 6d.
    /// Field always present; guard only pushed when `--features forward-auth`.
    forward_auth_cfg: Option<crate::config::schema::ForwardAuthConfig>,
    /// Consumer model auth config.
    /// Field always present; guard only pushed when `--features consumers`.
    consumers_cfg: Option<crate::config::schema::ConsumersConfig>,
    /// Site label for Prometheus metrics (`host:port` or `"*"`).
    site_label: String,
}

/// Classify a request's upstream target into a `HandlerKind` for filter routing.
pub(crate) fn handler_kind_of(upstream: &UpstreamTarget) -> HandlerKind {
    match upstream {
        UpstreamTarget::Local(LocalHandler::Health) => HandlerKind::Health,
        UpstreamTarget::Local(LocalHandler::AcmeChallenge { .. }) => HandlerKind::AcmeChallenge,
        UpstreamTarget::Local(LocalHandler::Metrics { .. }) => HandlerKind::Metrics,
        UpstreamTarget::Local(LocalHandler::StaticFile { .. }) => HandlerKind::StaticFile,
        UpstreamTarget::Local(LocalHandler::HotReloadSse) => HandlerKind::HotReloadSse,
        UpstreamTarget::Local(LocalHandler::HotReloadJs) => HandlerKind::HotReloadJs,
        UpstreamTarget::Local(LocalHandler::Overloaded) => HandlerKind::Overloaded,
        UpstreamTarget::Local(LocalHandler::Fallback) => HandlerKind::Fallback,
        // Deliberately exhaustive — no `_ =>` catch-all (issue #144). A catch-all here
        // silently routed any *new* `LocalHandler`/`UpstreamTarget` variant to Pingora's
        // `upstream_peer`, which is exactly the "falls through to a 502" bug class of
        // #341/#342. `Upload` is proxied too: the upload service is an internal loopback
        // upstream reached through `upstream_peer`, so it stays `HandlerKind::Proxy` even
        // in a build without the `proxy` feature.
        UpstreamTarget::Proxy { .. } | UpstreamTarget::Upload { .. } => HandlerKind::Proxy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── handler_kind_of ───────────────────────────────────────────────────────

    #[test]
    fn handler_kind_health() {
        let upstream = UpstreamTarget::Local(LocalHandler::Health);
        assert!(matches!(handler_kind_of(&upstream), HandlerKind::Health));
    }

    #[test]
    fn handler_kind_overloaded() {
        let upstream = UpstreamTarget::Local(LocalHandler::Overloaded);
        assert!(matches!(
            handler_kind_of(&upstream),
            HandlerKind::Overloaded
        ));
    }

    #[test]
    fn handler_kind_proxy() {
        let upstream = UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        };
        assert!(matches!(handler_kind_of(&upstream), HandlerKind::Proxy));
    }

    /// `Upload` is a proxied upstream (the upload service is an internal loopback
    /// upstream reached through `upstream_peer`), so it must classify as
    /// `HandlerKind::Proxy` in every build — including one without the `proxy` feature.
    /// If this ever regresses to `Fallback`, `--features upload` without `proxy` would
    /// answer every upload request with a 404 instead of reaching the upload service.
    #[test]
    fn handler_kind_upload_is_proxied() {
        let upstream = UpstreamTarget::Upload {
            addr: "127.0.0.1:4000".parse().unwrap(),
        };
        assert!(matches!(handler_kind_of(&upstream), HandlerKind::Proxy));
    }

    #[test]
    fn handler_kind_fallback_for_unknown_local() {
        let upstream = UpstreamTarget::Local(LocalHandler::Fallback);
        assert!(matches!(handler_kind_of(&upstream), HandlerKind::Fallback));
    }

    #[test]
    fn handler_kind_hot_reload_sse() {
        let upstream = UpstreamTarget::Local(LocalHandler::HotReloadSse);
        assert!(matches!(
            handler_kind_of(&upstream),
            HandlerKind::HotReloadSse
        ));
    }
}
