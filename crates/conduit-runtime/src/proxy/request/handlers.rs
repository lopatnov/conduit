//! Local-handler dispatch (`dispatch_local` / `build_handler`), the
//! health-endpoint upstream summary helper, and the Circuit Breaker /
//! fallback-of-last-resort local handlers.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.

use std::sync::atomic::Ordering;

use async_trait::async_trait;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::config::schema::HealthCheckConfig;
#[cfg(feature = "compression")]
use crate::filter::compression;
use crate::filter::response_time;
#[cfg(feature = "acme")]
use crate::handler::acme_challenge as acme_handler;
#[cfg(feature = "hotreload")]
use crate::handler::hot_reload as hot_reload_handler;
use crate::handler::response;
#[cfg(feature = "static")]
use crate::handler::{fallback, static_files};
use crate::handler::{health, metrics as metrics_handler, LocalHandlerImpl};
use crate::proxy::ctx::{LocalHandler, RequestCtx, UpstreamTarget};
use crate::proxy::request::HandlerKind;
use crate::proxy::service::ConduitProxy;

impl ConduitProxy {
    /// Dispatch a request to the appropriate local handler.
    ///
    /// Returns `Ok(true)` for local handlers (response fully written) or
    /// `Ok(false)` for proxy/upload targets (Pingora continues the pipeline).
    ///
    /// Adding a new local handler: implement [`LocalHandlerImpl`] in its module,
    /// then add one arm to [`Self::build_handler`] — this function stays unchanged.
    pub(super) async fn dispatch_local(
        &self,
        session: &mut Session,
        ctx: &mut Option<RequestCtx>,
        handler_kind: HandlerKind,
    ) -> Result<bool> {
        // Inject X-Response-Time before building the handler so it is included
        // in the extra_headers that every handler receives.
        if let Some(req_ctx) = ctx.as_mut() {
            let config = self.state.config.load();
            let site = config.sites.get(req_ctx.site_idx);
            let rt_cfg = site.and_then(|s| s.response_time.as_ref());
            if response_time::is_enabled(rt_cfg) {
                let digits = response_time::decimal_digits(rt_cfg);
                let elapsed = req_ctx.start_time.elapsed();
                let value = response_time::format_elapsed(elapsed, digits);
                req_ctx
                    .extra_headers
                    .push(("x-response-time".to_owned(), value));
            }
        }

        let Some(mut handler) = self.build_handler(handler_kind, ctx) else {
            return Ok(false); // HandlerKind::Proxy — let Pingora continue
        };
        handler.handle(session).await?;
        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Build the concrete [`LocalHandlerImpl`] for `kind`, extracting all
    /// required data from `ctx` and `self.state` up-front.
    ///
    /// Returns `None` for [`HandlerKind::Proxy`] (no local handling needed).
    /// To add a new handler, add one arm here — `dispatch_local` is unchanged.
    fn build_handler(
        &self,
        kind: HandlerKind,
        ctx: &Option<RequestCtx>,
    ) -> Option<Box<dyn LocalHandlerImpl>> {
        let extra = ctx
            .as_ref()
            .map(|c| c.extra_headers.clone())
            .unwrap_or_default();

        match kind {
            HandlerKind::Proxy => None,

            HandlerKind::AcmeChallenge => {
                #[cfg(feature = "acme")]
                {
                    let token = if let Some(RequestCtx {
                        upstream: UpstreamTarget::Local(LocalHandler::AcmeChallenge { token }),
                        ..
                    }) = ctx.as_ref()
                    {
                        token.clone()
                    } else {
                        unreachable!()
                    };
                    Some(Box::new(acme_handler::AcmeChallengeHandler {
                        token,
                        challenges: self.state.acme_challenges.clone(),
                        extra_headers: extra,
                    }))
                }
                #[cfg(not(feature = "acme"))]
                None
            }

            HandlerKind::Health => {
                let upstream_infos = self.collect_upstream_infos(ctx);
                Some(Box::new(health::HealthHandler {
                    extra_headers: extra,
                    upstream_infos,
                }))
            }

            HandlerKind::Metrics => {
                let token = if let Some(RequestCtx {
                    upstream: UpstreamTarget::Local(LocalHandler::Metrics { token }),
                    ..
                }) = ctx.as_ref()
                {
                    token.as_deref().map(str::to_owned)
                } else {
                    unreachable!()
                };
                #[cfg(feature = "compression")]
                let config = self.state.config.load();
                #[cfg(feature = "compression")]
                let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                #[cfg(feature = "compression")]
                let compress_opts = config
                    .sites
                    .get(site_idx)
                    .and_then(|s| s.compression.as_ref())
                    .and_then(compression::effective);
                #[cfg(feature = "compression")]
                let accept_enc = ctx
                    .as_ref()
                    .map(|c| c.accept_enc.clone())
                    .unwrap_or_default();
                Some(Box::new(metrics_handler::MetricsHandler {
                    token,
                    extra_headers: extra,
                    #[cfg(feature = "compression")]
                    compress_opts,
                    #[cfg(feature = "compression")]
                    accept_enc,
                }))
            }

            HandlerKind::StaticFile => {
                #[cfg(feature = "static")]
                {
                    let config = self.state.config.load();
                    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                    #[cfg(feature = "compression")]
                    let compress_opts = config
                        .sites
                        .get(site_idx)
                        .and_then(|s| s.compression.as_ref())
                        .and_then(compression::effective);
                    let fallback = config.sites.get(site_idx).and_then(|s| s.fallback.clone());
                    let accept_enc = ctx
                        .as_ref()
                        .map(|c| c.accept_enc.clone())
                        .unwrap_or_default();
                    let (roots, options, strip_prefix) = if let Some(RequestCtx {
                        upstream:
                            UpstreamTarget::Local(LocalHandler::StaticFile {
                                roots,
                                options,
                                strip_prefix,
                            }),
                        ..
                    }) = ctx.as_ref()
                    {
                        (roots.clone(), options.clone(), strip_prefix.clone())
                    } else {
                        unreachable!()
                    };
                    Some(Box::new(static_files::StaticFileHandler {
                        roots,
                        options,
                        strip_prefix,
                        extra_headers: extra,
                        #[cfg(feature = "compression")]
                        compress_opts,
                        accept_enc,
                        fallback,
                    }))
                }
                #[cfg(not(feature = "static"))]
                None
            }

            HandlerKind::Fallback => {
                #[cfg(feature = "static")]
                {
                    let config = self.state.config.load();
                    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                    let fallback = config.sites.get(site_idx).and_then(|s| s.fallback.clone());
                    #[cfg(feature = "compression")]
                    let compress_opts = config
                        .sites
                        .get(site_idx)
                        .and_then(|s| s.compression.as_ref())
                        .and_then(compression::effective);
                    #[cfg(feature = "compression")]
                    let accept_enc = ctx
                        .as_ref()
                        .map(|c| c.accept_enc.clone())
                        .unwrap_or_default();
                    Some(Box::new(fallback::FallbackHandler {
                        fallback,
                        extra_headers: extra,
                        #[cfg(feature = "compression")]
                        compress_opts,
                        #[cfg(feature = "compression")]
                        accept_enc,
                    }))
                }
                // `HandlerKind::Fallback` is the universal "nothing else
                // matched" terminal case (router.rs/routes.rs construct
                // `LocalHandler::Fallback` for any unmatched request on any
                // site, not just a static-file miss) — it must always
                // return `Some`, never fall through to `dispatch_local`'s
                // `HandlerKind::Proxy` path, or an unmatched request would
                // reach `upstream_peer()` with no real upstream and surface
                // as a 502/500 instead of the plain 404 every disabled
                // feature otherwise degrades to. Without `static`,
                // `sites[i].fallback`'s configured behavior (custom body,
                // byAccept, file serving) is unavailable — matching
                // `feature_warnings()`'s own "fallback responses (including
                // the site's default 404) will be disabled" wording — so
                // this always serves the same bare 404 `FallbackHandler`
                // already serves today when no `fallback:` is configured at
                // all.
                #[cfg(not(feature = "static"))]
                Some(Box::new(PlainNotFoundHandler {
                    extra_headers: extra,
                }))
            }

            // `HotReloadJs`/`HotReloadSse` can only be produced by
            // `router.rs`'s routing when the `hotreload` feature is
            // compiled in (`is_hot_reload_js_path`/`is_hot_reload_sse_path`
            // are themselves gated the same way, matching issue #341's
            // ACME-challenge fix) — so the `None` arm below is genuinely
            // unreachable, not a request-visible degradation, same
            // reasoning as `HandlerKind::StaticFile`'s own `None` arm
            // without `static`.
            #[cfg(feature = "hotreload")]
            HandlerKind::HotReloadJs => Some(Box::new(hot_reload_handler::HotReloadJsHandler {
                extra_headers: extra,
            })),
            #[cfg(not(feature = "hotreload"))]
            HandlerKind::HotReloadJs => None,

            #[cfg(feature = "hotreload")]
            HandlerKind::HotReloadSse => {
                let rx = self.state.hot_reload_tx.subscribe();
                Some(Box::new(hot_reload_handler::HotReloadSseHandler {
                    extra_headers: extra,
                    rx: Some(rx),
                }))
            }
            #[cfg(not(feature = "hotreload"))]
            HandlerKind::HotReloadSse => None,

            HandlerKind::Overloaded => Some(Box::new(OverloadedHandler {
                extra_headers: extra,
            })),
        }
    }

    /// Collect `(url, is_healthy)` pairs for the health endpoint when
    /// `healthCheck.includeUpstreams` is enabled.
    /// Collect per-upstream health info for the health-check handler.
    ///
    /// Returns an empty vec when `healthCheck.includeUpstreams` is not set.
    /// When enabled, returns extended data per upstream (healthy, latency,
    /// ejection status, consecutive 5xx) drawn from the UpstreamRegistry.
    pub(super) fn collect_upstream_infos(
        &self,
        ctx: &Option<RequestCtx>,
    ) -> Vec<crate::handler::health::UpstreamHealthInfo> {
        let req_ctx = match ctx.as_ref() {
            Some(c) => c,
            None => return vec![],
        };
        let config = self.state.config.load();
        let site = match config.sites.get(req_ctx.site_idx) {
            Some(s) => s,
            None => return vec![],
        };
        let include = site
            .health_check
            .as_ref()
            .and_then(|hc| match hc {
                HealthCheckConfig::Options(opts) => opts.include_upstreams,
                _ => None,
            })
            .unwrap_or(false);
        if !include {
            return vec![];
        }
        use crate::handler::health::UpstreamHealthInfo;
        use crate::proxy::upstream as us;
        let mut urls: Vec<String> = Vec::new();
        if let Some(proxy) = &site.proxy {
            urls.extend(us::target_urls_from_proxy(proxy));
        }
        if let Some(routes) = &site.routes {
            for rc in routes {
                if let Some(rt) = &rc.proxy {
                    urls.extend(us::target_urls(rt));
                }
            }
        }
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        urls.into_iter()
            .map(|url| {
                let entry = self.state.upstream_health.statuses.get(&url);
                let healthy = entry.as_ref().map(|e| e.healthy).unwrap_or(true);
                let latency_ms = entry.as_ref().and_then(|e| e.latency_ms);
                let ejected = entry
                    .as_ref()
                    .and_then(|e| e.ejected_until_secs)
                    .map(|until| until > now_secs)
                    .unwrap_or(false);
                let consecutive_5xx = entry.as_ref().map(|e| e.consecutive_5xx).unwrap_or(0);
                UpstreamHealthInfo {
                    url,
                    healthy,
                    latency_ms,
                    ejected,
                    consecutive_5xx,
                }
            })
            .collect()
    }
}

// ── Circuit Breaker handler ───────────────────────────────────────────────────

/// Returns `503 Service Unavailable` when all upstreams for the matched route
/// are at the configured `maxConnectionsPerUpstream` limit.
struct OverloadedHandler {
    extra_headers: Vec<(String, String)>,
}

#[async_trait]
impl LocalHandlerImpl for OverloadedHandler {
    async fn handle(&mut self, session: &mut Session) -> pingora_core::Result<()> {
        response::write_response(
            session,
            503,
            "application/json",
            bytes::Bytes::from_static(
                b"{\"error\":\"Service Unavailable\",\"status\":503,\"reason\":\"upstream_overloaded\"}",
            ),
            &self.extra_headers,
        )
        .await
    }
}

// ── Fallback-of-last-resort handler (no `static` feature) ─────────────────────

/// Plain `404 Not Found` used for `HandlerKind::Fallback` when the `static`
/// feature isn't compiled in — see the `#[cfg(not(feature = "static"))]`
/// arm of `build_handler` for why this must exist unconditionally rather
/// than returning `None`.
#[cfg(not(feature = "static"))]
struct PlainNotFoundHandler {
    extra_headers: Vec<(String, String)>,
}

#[cfg(not(feature = "static"))]
#[async_trait]
impl LocalHandlerImpl for PlainNotFoundHandler {
    async fn handle(&mut self, session: &mut Session) -> pingora_core::Result<()> {
        response::write_response(
            session,
            404,
            "text/plain",
            bytes::Bytes::from_static(b"Not Found"),
            &self.extra_headers,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::proxy::ctx::ProxyReqState;
    use crate::proxy::service::AppState;

    // Duplicated across this file, `peer.rs`, `retry.rs`, and `transform.rs`'s
    // test modules -- `peer.rs` holds the "canonical" original location.
    fn make_ctx(upstream: UpstreamTarget) -> RequestCtx {
        RequestCtx::new(0, upstream, ProxyReqState::default(), None)
    }

    // ── build_handler ─────────────────────────────────────────────────────────

    #[test]
    fn build_handler_proxy_returns_none() {
        let proxy = make_proxy();
        let result = proxy.build_handler(HandlerKind::Proxy, &None);
        assert!(
            result.is_none(),
            "Proxy handler must return None (uses Pingora path)"
        );
    }

    #[test]
    fn build_handler_health_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Health)));
        let result = proxy.build_handler(HandlerKind::Health, &ctx);
        assert!(result.is_some(), "Health handler must return Some");
    }

    #[test]
    fn build_handler_metrics_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Metrics {
            token: None,
        })));
        let result = proxy.build_handler(HandlerKind::Metrics, &ctx);
        assert!(result.is_some(), "Metrics handler must return Some");
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn build_handler_hot_reload_js_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::HotReloadJs)));
        let result = proxy.build_handler(HandlerKind::HotReloadJs, &ctx);
        assert!(result.is_some(), "HotReloadJs handler must return Some");
    }

    #[test]
    #[cfg(feature = "hotreload")]
    fn build_handler_hot_reload_sse_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::HotReloadSse)));
        let result = proxy.build_handler(HandlerKind::HotReloadSse, &ctx);
        assert!(result.is_some(), "HotReloadSse handler must return Some");
    }

    #[test]
    #[cfg(feature = "static")]
    fn build_handler_static_file_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::StaticFile {
            roots: vec![std::path::PathBuf::from("./dist")],
            options: std::sync::Arc::new(Default::default()),
            strip_prefix: None,
        })));
        let result = proxy.build_handler(HandlerKind::StaticFile, &ctx);
        assert!(result.is_some(), "StaticFile handler must return Some");
    }

    #[test]
    #[cfg(not(feature = "static"))]
    fn build_handler_static_file_returns_none_without_feature() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::StaticFile {
            roots: vec![std::path::PathBuf::from("./dist")],
            options: std::sync::Arc::new(Default::default()),
            strip_prefix: None,
        })));
        let result = proxy.build_handler(HandlerKind::StaticFile, &ctx);
        assert!(
            result.is_none(),
            "StaticFile handler must return None without the `static` feature"
        );
    }

    #[test]
    fn build_handler_fallback_returns_some() {
        // `HandlerKind::Fallback` must return `Some` in BOTH feature states —
        // it's the universal "nothing else matched" terminal case (see the
        // `#[cfg(not(feature = "static"))]` arm's own doc comment), not
        // exclusive to the `static` feature. Returning `None` here would
        // make `dispatch_local` treat an unmatched request as `Proxy` and
        // hand it to `upstream_peer()`, which has no real upstream to use.
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Fallback)));
        let result = proxy.build_handler(HandlerKind::Fallback, &ctx);
        assert!(result.is_some(), "Fallback handler must return Some");
    }

    #[test]
    fn build_handler_overloaded_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Overloaded)));
        let result = proxy.build_handler(HandlerKind::Overloaded, &ctx);
        assert!(result.is_some(), "Overloaded handler must return Some");
    }

    // `make_proxy` is duplicated here (and in `retry.rs`'s own test module) --
    // both this file's `collect_upstream_infos` tests below and `retry.rs`'s
    // `retry_budget_allows` tests need it, and neither module depends on the
    // other. See `retry.rs`'s copy for the "canonical" original location.
    fn make_proxy() -> ConduitProxy {
        let config = crate::config::schema::AppConfig::default();
        let state = AppState::new(config, std::path::PathBuf::from("."), None);
        ConduitProxy {
            state: std::sync::Arc::new(state),
        }
    }

    // ── collect_upstream_infos ────────────────────────────────────────────────

    #[test]
    fn collect_upstream_infos_none_ctx_returns_empty() {
        let proxy = make_proxy();
        let result = proxy.collect_upstream_infos(&None);
        assert!(result.is_empty(), "None ctx must return empty list");
    }

    #[test]
    fn collect_upstream_infos_no_include_upstreams_returns_empty() {
        let config = crate::config::schema::AppConfig {
            sites: vec![crate::config::schema::SiteConfig::default()],
            ..Default::default()
        };
        let state = AppState::new(config, std::path::PathBuf::from("."), None);
        let proxy = ConduitProxy {
            state: std::sync::Arc::new(state),
        };
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::Health)));
        let result = proxy.collect_upstream_infos(&ctx);
        assert!(result.is_empty(), "no include_upstreams → empty list");
    }
}
