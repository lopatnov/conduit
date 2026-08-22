//! Request-side orchestration for [`ConduitProxy`].
//!
//! This module hosts the bulk of the request-processing pipeline that used to
//! live directly on `ConduitProxy` in `service.rs`:
//!
//! - `do_request_filter` — guard-chain orchestration, routing, dispatch
//! - `run_guard_filters` — builds and runs the `FilterChain`
//! - `dispatch_local` / `build_handler` — local-handler dispatch
//! - `collect_upstream_infos` — health-endpoint upstream summaries
//! - `try_retry_connect_error` / `try_retry_proxy_error` — retry decision logic
//! - `record_failed_upstream_for_retry` / `retry_budget_allows` — retry bookkeeping
//! - the `request_filter`, `request_body_filter`, `request_cache_filter`,
//!   `should_serve_stale`, `cache_key_callback`, `upstream_peer`, and
//!   `upstream_request_filter` trait-method bodies (called from thin delegators
//!   in the `impl ProxyHttp for ConduitProxy` block in `service.rs`)
//! - request-side helpers: path rewriting, header transforms, mirroring,
//!   forwarded-header injection, peer address/option resolution, body buffering
//!
//! Pure mechanical split from `service.rs` — no behavioral change.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;
use pingora_cache::CacheKey;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::RequestHeader;
use pingora_proxy::Session;

use crate::config::schema::{
    ApiKeyConfig, BasicAuthConfig, ConnectionPoolConfig, CorsConfig, HealthCheckConfig,
    IpFilterConfig, LimitsConfig, MiddlewareEntry, ProxyTimeout, RateLimitConfig, SiteConfig,
};
#[cfg(feature = "consumers")]
use crate::filter::chain::ConsumersGuard;
#[cfg(feature = "fault-injection")]
use crate::filter::chain::FaultInjectionGuard;
#[cfg(feature = "forward-auth")]
use crate::filter::chain::ForwardAuthGuard;
use crate::filter::chain::{
    AllowedHostsGuard, ApiKeyGuard, BasicAuthGuard, CorsPreflight, FilterChain, FilterContext,
    HealthBypass, IpGuard, LimitsGuard, MiddlewareGuard, RateLimitGuard, RedirectGuard,
    XRequestIdGuard,
};
use crate::filter::rate_limit;
use crate::filter::{compression, cors, redirects, response_time, security_headers};
#[cfg(feature = "acme")]
use crate::handler::acme_challenge as acme_handler;
use crate::handler::response;
use crate::handler::{
    fallback, health, hot_reload as hot_reload_handler, metrics as metrics_handler, static_files,
    LocalHandlerImpl,
};
use crate::proxy::cache as proxy_cache;
#[cfg(feature = "cache")]
use crate::proxy::cache_disk;
#[cfg(feature = "redis")]
use crate::proxy::cache_redis;
use crate::proxy::ctx::{AcceptEncoding, LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::router;
use crate::proxy::service::ConduitProxy;
use crate::proxy::upstream;

#[cfg(feature = "cache")]
use pingora_cache::storage::Storage as CacheStorage;

impl ConduitProxy {
    /// Record a failed upstream attempt immediately before triggering a Pingora
    /// retry.
    ///
    /// Updates all passive health state for the upstream that just returned
    /// `status`:
    ///
    /// - Releases the connection slot (`conn_dec`) — only if this attempt
    ///   actually held one (`upstream_conn_slot`) — so the next
    ///   `upstream_peer()` call can acquire a new slot for the retry target.
    /// - Records latency into the EWMA and increments `consecutive_5xx` via
    ///   `record_request_latency`.
    /// - Runs outlier-detection ejection (`maybe_eject`) if configured.
    /// - Decrements the Prometheus `upstream_active_connections` gauge and
    ///   increments `upstream_requests_total` / `upstream_latency_seconds`.
    /// - Appends the failed attempt to `failed_upstream_attempts` (currently
    ///   write-only — see that field's doc comment and issue #218), then
    ///   clears `proxy_upstream_url` and `upstream_conn_slot` so the next
    ///   `upstream_peer()` starts fresh with no inherited slot.
    ///
    /// Without this, a successful retry on a different backend would silently
    /// absorb the failure without updating the health record of the backend that
    /// actually failed.
    pub(super) fn record_failed_upstream_for_retry(
        &self,
        ctx: &mut Option<RequestCtx>,
        config: &crate::config::schema::AppConfig,
        status: u16,
    ) {
        let req_ctx_mut = match ctx.as_mut() {
            Some(c) => c,
            None => return,
        };
        // Use take() to extract the URL and simultaneously clear the field,
        // avoiding a clone and the explicit `= None` at the end of the function.
        let url = match req_ctx_mut.proxy_upstream_url.take() {
            Some(u) => u,
            None => return,
        };
        // Also clear the slot flag: the next attempt's URL (set below by
        // upstream_peer's retry-restore) never goes through conn_inc, so it
        // must start without an inherited slot to release.
        let had_conn_slot = std::mem::take(&mut req_ctx_mut.upstream_conn_slot);

        let elapsed_us = req_ctx_mut.start_time.elapsed().as_micros() as u64;
        // Release the connection slot for the failed upstream immediately —
        // only if this request actually held one.
        if had_conn_slot {
            self.state.upstream_health.conn_dec(&url);
        }
        crate::proxy::health::record_request_latency(
            &self.state.upstream_health,
            &url,
            elapsed_us,
            status,
        );

        // Trigger outlier detection for the failed upstream.
        let site_idx = req_ctx_mut.site_idx;
        if let Some(od) = config
            .sites
            .get(site_idx)
            .and_then(|s| s.outlier_detection.as_ref())
        {
            crate::proxy::health::maybe_eject(&self.state.upstream_health, &url, od);
        }

        // Update Prometheus per-upstream metrics so the active-connections gauge
        // doesn't leak (it was incremented by upstream_request_filter when we
        // first forwarded to this backend).
        self.state
            .metrics
            .upstream_active_connections
            .with_label_values(&[&url])
            .dec();
        self.state
            .metrics
            .upstream_requests_total
            .with_label_values(&[&url, &status.to_string()])
            .inc();
        if let Some(upstream_secs) = req_ctx_mut
            .upstream_start
            .map(|t| t.elapsed().as_secs_f64())
        {
            self.state
                .metrics
                .upstream_latency_seconds
                .with_label_values(&[&url])
                .observe(upstream_secs);
        }
        // Reset upstream_start for the retry attempt.
        req_ctx_mut.upstream_start = None;
        // Push to failed list for structured logging / future use.
        // proxy_upstream_url was already cleared by the take() above.
        req_ctx_mut.failed_upstream_attempts.push((url, status));
    }

    /// Check the retry budget and increment `retry_inflight` if a retry is allowed.
    ///
    /// Returns `true` when the retry may proceed, `false` when the budget is
    /// exhausted and the retry should be suppressed.
    ///
    /// Concurrent requests may race past this check, so the budget is a *soft*
    /// limit — occasional over-budget retries are acceptable.
    pub(super) fn retry_budget_allows(&self, retry: &mut RetryState) -> bool {
        if let Some(budget_pct) = retry.budget_percent {
            let inflight = self.state.inflight.load(Ordering::Relaxed).max(1) as f64;
            let current_retries = self.state.retry_inflight.load(Ordering::Relaxed) as f64;
            let limit = (inflight * budget_pct / 100.0).ceil() as usize;
            let current = current_retries as usize;
            if current >= limit {
                tracing::debug!(
                    budget_pct,
                    current_retries,
                    inflight,
                    "retry budget exhausted — suppressing retry"
                );
                return false;
            }
        }
        self.state.retry_inflight.fetch_add(1, Ordering::Relaxed);
        retry.is_retrying = true;
        true
    }

    /// Core request-processing pipeline.
    ///
    /// Called by Pingora's [`ProxyHttp::request_filter`] hook (via the thin
    /// delegator in `service.rs`).  Returns `Ok(true)` when a response has
    /// already been written (request handled locally or rejected), `Ok(false)`
    /// to continue to the upstream proxy path.
    ///
    /// Pipeline order:
    /// 1. Inflight counter increment + OTel span start.
    /// 2. Route the request → populate `RequestCtx` with upstream, retry, cache cfg.
    /// 3. Extract per-site guards from config.
    /// 4. Run `FilterChain`: XRequestId → IP filter → CORS preflight → health bypass
    ///    → AllowedHosts → limits → rate-limit → consumers → basic-auth → API-key
    ///    → JWT → forward-auth → redirect → fault injection → middleware.
    /// 5. Per-route rate limit check.
    /// 6. Priority-based load shedding.
    /// 7. JWT claims extraction for header template expansion.
    /// 8. Dispatch to local handler or proxy upstream.
    pub(super) async fn do_request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Option<RequestCtx>,
    ) -> Result<bool> {
        self.state.inflight.fetch_add(1, Ordering::Relaxed);
        self.state.metrics.active_connections.inc();

        // ── OpenTelemetry: start span for this request ────────────────────────
        // The span is stored in RequestCtx and ended in logging() after the
        // response is sent.  When otlp feature is disabled this block compiles
        // to nothing.
        #[cfg(feature = "otlp")]
        let otel_span_start = {
            use opentelemetry::global;
            use opentelemetry::trace::{SpanKind, Tracer};
            let method = session.req_header().method.as_str().to_owned();
            let path = session.req_header().uri.path().to_owned();
            let name = format!("{method} {path}");
            let tracer = global::tracer("conduit");
            let span = tracer
                .span_builder(name)
                .with_kind(SpanKind::Server)
                .start(&tracer);
            Some(span)
        };

        // ── Gather request metadata before borrowing the config ───────────────
        let request_origin = cors::request_origin(session);
        let is_cors_preflight = cors::is_preflight(session);

        // ── Load config once — extract all per-site filters ───────────────────
        // A single owned snapshot drives routing AND the post-guard helpers
        // below, so they all observe the same config that resolved
        // `req_ctx.site_idx` — never a newer hot-reloaded snapshot (avoids the
        // routing-vs-helper TOCTOU drift raised in #91).  `load_full()` returns
        // an owned `Arc` (cheap refcount bump, no alloc) that is safe to hold
        // across the guard-chain `.await` below, unlike a `load()` guard.
        let config = self.state.config.load_full();
        let (
            mut req_ctx,
            ip_cfg,
            limits_cfg,
            rate_limit_cfg,
            basic_auth_cfg,
            api_key_cfg,
            cors_cfg,
            security_cfg,
            site_host,
            redirect_result,
            custom_headers,
            middleware,
            script_method,
            script_path_str,
            script_query,
            script_headers,
            extracted_client_ip,
            fault_injection_cfg,
            jwt_auth_cfg,
            forward_auth_cfg,
            consumers_cfg,
            site_label,
        ) = {
            let host = extract_host(session);
            let path_and_query = session
                .req_header()
                .uri
                .path_and_query()
                .map(|pq| pq.as_str().to_owned())
                .unwrap_or_else(|| session.req_header().uri.path().to_owned());
            let path = session.req_header().uri.path().to_owned();

            let client_ip = session
                .client_addr()
                .and_then(|a| a.as_inet())
                .map(|a| a.ip().to_string())
                .unwrap_or_default();

            let method = session.req_header().method.as_str().to_owned();
            let query = session.req_header().uri.query().map(str::to_owned);

            // Extract the local port so that port-differentiated virtual hosts
            // (e.g. port 8080 public site vs. port 8081 admin site) are routed
            // to the correct SiteConfig even when no explicit `host` is set.
            // Pingora's SocketAddr wraps std::net::SocketAddr; use as_inet() to
            // reach the standard type and its .port() method.
            let server_port: u16 = session
                .as_ref()
                .server_addr()
                .and_then(|a| a.as_inet())
                .map(|a| a.port())
                .unwrap_or(80);

            let req_ctx = router::route_request(
                &config,
                &host,
                &path,
                &method,
                &session.req_header().headers,
                query.as_deref(),
                &client_ip,
                server_port,
                &self.state.round_robin,
                &self.state.upstream_health,
                self.state.upload_addr,
            );
            let site = config.sites.get(req_ctx.site_idx);

            let ip_cfg = site.and_then(|s| s.ip_filter.clone());
            let limits_cfg = site.and_then(|s| s.limits.clone());
            let rate_limit_cfg = site.and_then(|s| s.rate_limit.clone());
            let basic_auth_cfg = site.and_then(|s| s.basic_auth.clone());
            let api_key_cfg = site.and_then(|s| s.api_key.clone());
            let cors_cfg = site.and_then(|s| s.cors.clone());
            let security_cfg = site.and_then(|s| s.security_headers.clone());
            let redirect_result = site
                .and_then(|s| s.redirects.as_deref())
                .and_then(|rules| redirects::apply_redirects(rules, &path_and_query));
            // Custom response headers defined in site.headers — applied to every response.
            let custom_headers: Vec<(String, String)> = site
                .and_then(|s| s.headers.as_ref())
                .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            // Middleware chain entries for this site.
            let middleware: Vec<MiddlewareEntry> = site
                .and_then(|s| s.middleware.as_ref())
                .cloned()
                .unwrap_or_default();
            // Collect request headers for Rhai scripts / WASM plugins (lower-cased keys).
            // Built lazily: only when at least one middleware entry is configured.
            // On the hot path (no middleware) this avoids O(n_headers) allocations
            // that would otherwise happen on every single request.
            let req_headers_for_script: std::collections::HashMap<String, String> =
                if middleware.is_empty() {
                    Default::default()
                } else {
                    session
                        .req_header()
                        .headers
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|vs| (k.as_str().to_ascii_lowercase(), vs.to_owned()))
                        })
                        .collect()
                };
            // Site label for Prometheus metrics.
            let site_label: String = match site {
                Some(s) => {
                    let host = s.host.as_deref().unwrap_or("*");
                    let port = s.port.unwrap_or(80);
                    format!("{host}:{port}")
                }
                None => "*".to_owned(),
            };

            (
                req_ctx,
                ip_cfg,
                limits_cfg,
                rate_limit_cfg,
                basic_auth_cfg,
                api_key_cfg,
                cors_cfg,
                security_cfg,
                site.and_then(|s| s.host.clone()),
                redirect_result,
                custom_headers,
                middleware,
                method,
                path,
                query.unwrap_or_default(),
                req_headers_for_script,
                client_ip,
                site.and_then(|s| s.fault_injection.clone()),
                site.and_then(|s| s.jwt_auth.clone()),
                site.and_then(|s| s.forward_auth.clone()),
                site.and_then(|s| s.consumers.clone()),
                site_label,
            )
        };

        // Parse Accept-Encoding header once and store it in the request context.
        let ae_str = session
            .req_header()
            .headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        req_ctx.accept_enc = AcceptEncoding::parse(ae_str);

        // Compute security headers once; reused both in the full extra_headers set
        // and in the preflight path that injects security headers without CORS headers.
        let sec_only: Vec<(String, String)> = security_cfg
            .as_ref()
            .map(security_headers::header_entries)
            .unwrap_or_default();

        // ── Build planned response headers (CORS + security + custom) ────────
        // These are injected into every response written for this request.
        {
            let cors_hdrs = cors_cfg
                .as_ref()
                .map(|c| cors::response_headers(c, request_origin.as_deref()))
                .unwrap_or_default();
            req_ctx.extra_headers = cors_hdrs
                .into_iter()
                .chain(sec_only.iter().cloned())
                .chain(custom_headers)
                .collect();
        }

        let handler_kind = handler_kind_of(&req_ctx.upstream);

        // ── Guard filters (ip, cors, limits, auth, redirects, scripts) ──────────
        // Extract Host header once for AllowedHostsGuard.
        let incoming_host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let guards = GuardCtx {
            ip_cfg,
            limits_cfg,
            security_cfg: security_cfg.clone(),
            site_host,
            host: incoming_host,
            rate_limit_cfg,
            basic_auth_cfg,
            api_key_cfg,
            cors_cfg,
            redirect_result,
            middleware,
            handler_kind: handler_kind.clone(),
            is_preflight: is_cors_preflight,
            sec_only,
            origin: request_origin,
            extra_headers: req_ctx.extra_headers.clone(),
            script_method,
            script_path: script_path_str,
            script_query,
            script_headers,
            client_ip: extracted_client_ip,
            fault_injection_cfg,
            jwt_auth_cfg: jwt_auth_cfg.clone(), // clone — jwt_cfg needed below for claim extraction
            forward_auth_cfg,
            consumers_cfg,
            site_label,
        };
        if self.run_guard_filters(session, guards).await? {
            return Ok(true);
        }

        // Resolve the matched site from the same snapshot used for routing, so
        // the post-guard helpers below act on the exact config that produced
        // `req_ctx.site_idx` — not a possibly newer hot-reloaded snapshot (#91).
        let site = config.sites.get(req_ctx.site_idx);

        // If per-IP connection limiting is configured and the request was allowed,
        // store the client IP so logging() can decrement the counter on completion.
        self.store_ip_conn_slot(session, &mut req_ctx, site);

        // ── Per-route rate limiting (applied after site-level guard chain) ──────
        // Checked here — after routing — so we know which route was matched.
        if self
            .enforce_route_rate_limit(session, &req_ctx, site)
            .await?
        {
            return Ok(true);
        }

        // ── Priority-based load shedding (post-routing) ───────────────────────
        if self
            .shed_low_priority_request(session, &req_ctx, site)
            .await?
        {
            return Ok(true);
        }

        // ── JWT claims extraction for header template substitution ─────────────
        // Only available when compiled with --features jwt.
        #[cfg(feature = "jwt")]
        {
            req_ctx.jwt_claims = jwt_claims_from_session(session, jwt_auth_cfg.as_ref());
        }

        // ── Attach OTel span to request context ───────────────────────────────
        #[cfg(feature = "otlp")]
        {
            req_ctx.otel_span = otel_span_start;
        }

        // ── Dispatch ──────────────────────────────────────────────────────────
        *ctx = Some(req_ctx);
        self.dispatch_local(session, ctx, handler_kind).await
    }

    /// Run all guard filters in pipeline order.
    ///
    /// Returns `Ok(true)` when a filter has already written a response and
    /// decremented the inflight counter (caller must return `Ok(true)` too).
    /// Returns `Ok(false)` to continue to the dispatcher.
    /// Run the guard filter chain for this request.
    ///
    /// Builds a [`FilterChain`] from the pre-computed [`GuardCtx`] and runs it.
    /// Each filter is independent — adding a new guard means implementing
    /// [`crate::filter::chain::RequestFilter`] and pushing it into the chain here,
    /// with no other changes required in this file.
    ///
    /// Returns `Ok(true)` when a filter wrote a rejection response (Pingora
    /// should stop the pipeline), `Ok(false)` to continue with normal dispatch.
    async fn run_guard_filters(&self, session: &mut Session, guards: GuardCtx) -> Result<bool> {
        let is_bypass = matches!(
            guards.handler_kind,
            HandlerKind::Health
                | HandlerKind::AcmeChallenge
                | HandlerKind::HotReloadSse
                | HandlerKind::HotReloadJs
        );

        // Build the chain.  Filters run in the order they are pushed.
        let mut chain = FilterChain::new();

        // 0. X-Request-ID — inject before any other processing so the ID is
        //    available to upstream and all downstream filters.
        chain = chain.push(XRequestIdGuard);

        // 1. IP filter — always pushed so the runtime deny-list (POST /ip-deny)
        //    works even when no static `ipFilter` config is present.
        //    Uses the default empty config when not configured (all IPs pass
        //    unless the dynamic_deny list has entries).
        chain = chain.push(IpGuard {
            cfg: guards.ip_cfg.unwrap_or_default(),
            dynamic_deny: self.state.dynamic_deny.clone(),
        });

        // 2. CORS preflight — runs before auth; browsers send OPTIONS without credentials.
        if let Some(cfg) = guards.cors_cfg {
            chain = chain.push(CorsPreflight {
                cfg,
                is_preflight: guards.is_preflight,
                origin: guards.origin,
                sec_headers: guards.sec_only,
            });
        }

        // 3. Health / ACME / hot-reload bypass — skips all remaining guards.
        chain = chain.push(HealthBypass { bypass: is_bypass });

        // 3a. AllowedHosts: Host header allowlist (after bypass so health is exempt).
        chain = chain.push(AllowedHostsGuard {
            security_cfg: guards.security_cfg.clone(),
            site_host: guards.site_host.clone(),
            host: guards.host.clone(),
        });

        // 4. Request size / header limits.
        if let Some(cfg) = guards.limits_cfg {
            chain = chain.push(LimitsGuard { cfg });
        }

        // 5. Token-bucket rate limiting.
        if let Some(cfg) = guards.rate_limit_cfg {
            chain = chain.push(RateLimitGuard {
                cfg,
                site_label: guards.site_label.clone(),
            });
        }

        // 6. Consumer model auth (identifies consumer, injects X-Consumer-ID).
        #[cfg(feature = "consumers")]
        if let Some(cfg) = guards.consumers_cfg {
            chain = chain.push(ConsumersGuard {
                cfg,
                path: guards.script_path.clone(),
            });
        }

        // 6a. Basic Auth.
        if let Some(cfg) = guards.basic_auth_cfg {
            chain = chain.push(BasicAuthGuard { cfg });
        }

        // 6b. API-key Auth.
        if let Some(cfg) = guards.api_key_cfg {
            chain = chain.push(ApiKeyGuard { cfg });
        }

        // 6c. JWT Bearer-token Auth.
        #[cfg(feature = "jwt")]
        if let Some(cfg) = guards.jwt_auth_cfg {
            {
                use crate::filter::chain::JwtGuard;
                chain = chain.push(JwtGuard {
                    cfg,
                    path: guards.script_path.clone(),
                });
            }
        }

        // 6d. Forward Auth — delegate to external auth service.
        #[cfg(feature = "forward-auth")]
        if let Some(cfg) = guards.forward_auth_cfg {
            chain = chain.push(ForwardAuthGuard {
                cfg,
                path: guards.script_path.clone(),
            });
        }

        // 7. Redirects.
        chain = chain.push(RedirectGuard {
            result: guards.redirect_result,
        });

        // 8. Fault injection (chaos testing — disabled in production).
        #[cfg(feature = "fault-injection")]
        if let Some(cfg) = guards.fault_injection_cfg {
            chain = chain.push(FaultInjectionGuard { cfg });
        }

        // 9. Middleware pipeline: Rhai scripts + WASM plugins in declared order.
        chain = chain.push(MiddlewareGuard {
            middleware: guards.middleware,
            req_path: guards.script_path,
            method: guards.script_method,
            query: guards.script_query,
            headers: guards.script_headers,
            client_ip: guards.client_ip.clone(),
        });

        let mut ctx = FilterContext {
            session,
            extra_headers: &guards.extra_headers,
            inflight: &self.state.inflight,
            rate_limiter: &self.state.rate_limiter,
            #[cfg(feature = "redis")]
            redis_rate_limiter: self.state.redis_rate_limiter.as_ref(),
            ip_conn_counts: &self.state.ip_conn_counts,
            client_ip: guards.client_ip,
        };

        chain.run(&mut ctx).await
    }

    /// If per-IP connection limiting is configured for the matched site, store
    /// the client IP in the context so logging() can release the slot on
    /// completion.
    ///
    /// `site` is the route-resolved site from the request's config snapshot
    /// (passed in by `do_request_filter`) so this never re-reads a different
    /// config than the one that produced `req_ctx.site_idx`.
    fn store_ip_conn_slot(
        &self,
        session: &Session,
        req_ctx: &mut RequestCtx,
        site: Option<&SiteConfig>,
    ) {
        let per_ip_limit_configured = site
            .and_then(|s| s.limits.as_ref())
            .and_then(|l| l.max_connections_per_ip)
            .is_some();
        if !per_ip_limit_configured {
            return;
        }
        let ip = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip().to_string())
            .unwrap_or_default();
        if ip.is_empty() {
            return;
        }
        // Store the RAII guard; it will automatically decrement the slot
        // counter when RequestCtx is dropped at the end of logging() — no
        // manual fetch_sub needed.
        req_ctx.ip_conn_slot = Some(crate::filter::chain::IpConnSlotGuard {
            ip,
            counts: std::sync::Arc::clone(&self.state.ip_conn_counts),
        });
    }

    /// Per-route token-bucket rate limiting, applied after the site-level
    /// guard chain — once the route is known.
    ///
    /// `site` is the route-resolved site from the request's config snapshot
    /// (passed in by `do_request_filter`) so this shares the routing snapshot.
    ///
    /// Returns `Ok(true)` when the request was rejected with 429 (response
    /// written, inflight counters decremented), `Ok(false)` to continue.
    async fn enforce_route_rate_limit(
        &self,
        session: &mut Session,
        req_ctx: &RequestCtx,
        site: Option<&SiteConfig>,
    ) -> Result<bool> {
        // Borrow the path directly from the session — `find_route_rate_limit`
        // takes `&str`, so there is no need to allocate an owned String on the
        // rate-limit hot path.
        let path = session.req_header().uri.path();
        let Some(site) = site else {
            return Ok(false);
        };
        let Some((rl_cfg, route_key)) = router::find_route_rate_limit(site, path) else {
            return Ok(false);
        };
        let key = format!(
            "route:{route_key}:{}",
            rate_limit::extract_client_key(&rl_cfg, session)
        );
        let allowed = {
            self.state
                .rate_limiter
                .entry(key)
                .or_insert_with(|| {
                    rate_limit::TokenBucket::new(
                        rl_cfg.limit,
                        rl_cfg.burst.unwrap_or(0),
                        rl_cfg.window_secs,
                    )
                })
                .try_consume()
        };
        if allowed {
            return Ok(false);
        }
        let extra = req_ctx.extra_headers.clone();
        self.state
            .metrics
            .rate_limit_rejected_total
            .with_label_values(&[&format!("route:{}", route_key)])
            .inc();
        response::write_response(
            session,
            429,
            "text/plain",
            bytes::Bytes::from_static(b"Too Many Requests"),
            &extra,
        )
        .await?;
        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
        self.state.metrics.active_connections.dec();
        Ok(true)
    }

    /// Priority-based load shedding (post-routing).
    ///
    /// When the site is above its priority threshold, low-priority routes
    /// are shed with 503.  Priority is determined solely by the route config
    /// (proxy.*.priority).
    ///
    /// SECURITY: We intentionally do NOT trust the `X-Priority` header from
    /// downstream clients — an attacker could send `X-Priority: 100` to
    /// bypass load shedding entirely.  The header is stripped here to
    /// prevent it from leaking to the upstream as well.
    ///
    /// `site` is the route-resolved site from the request's config snapshot
    /// (passed in by `do_request_filter`) so this shares the routing snapshot.
    ///
    /// Returns `Ok(true)` when the request was shed with 503 (response
    /// written, inflight counters decremented), `Ok(false)` to continue.
    async fn shed_low_priority_request(
        &self,
        session: &mut Session,
        req_ctx: &RequestCtx,
        site: Option<&SiteConfig>,
    ) -> Result<bool> {
        // Strip X-Priority from the incoming request so it cannot be used
        // by the upstream to grant itself elevated priority on retries.
        let _ = session.req_header_mut().remove_header("x-priority");

        // Borrow the path directly from the session — it is only used for the
        // route priority lookup below, which takes `&str`.
        let path = session.req_header().uri.path();
        let Some(site) = site else {
            return Ok(false);
        };
        let Some(limits) = &site.limits else {
            return Ok(false);
        };
        let (Some(max_inflight), Some(threshold)) =
            (limits.max_inflight_requests, limits.priority_threshold)
        else {
            return Ok(false);
        };
        let current = self.state.inflight.load(Ordering::Relaxed) as f64;
        let load_fraction = current / max_inflight as f64;
        if load_fraction < threshold {
            return Ok(false);
        }
        // Base priority from route config, optionally elevated by the
        // RFC 9218 standard `Priority: u=<N>` header.  Browsers and CDNs
        // set this header; Conduit maps urgency 0–7 to 100–2 and takes the
        // maximum so that clients can signal high urgency but not bypass
        // server-assigned priority.
        let route_priority = router::find_route_priority(site, path).unwrap_or(50);
        let rfc9218_priority = session
            .req_header()
            .headers
            .get("priority")
            .and_then(|v| v.to_str().ok())
            .and_then(router::parse_rfc9218_priority);
        // Clamp downward only: the RFC 9218 header may lower effective
        // priority (making shedding more likely) but never raise it above
        // the operator-configured value.  Allowing clients to raise their
        // own priority would let any request bypass load shedding.
        let effective_priority = rfc9218_priority.map_or(route_priority, |p| p.min(route_priority));
        if effective_priority >= 50 {
            return Ok(false);
        }
        let extra = req_ctx.extra_headers.clone();
        response::write_response(
            session,
            503,
            "application/json",
            bytes::Bytes::from_static(
                b"{\"error\":\"Service Unavailable\",\"reason\":\"load shedding\",\"status\":503}",
            ),
            &extra,
        )
        .await?;
        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
        self.state.metrics.active_connections.dec();
        Ok(true)
    }

    /// Dispatch a request to the appropriate local handler.
    ///
    /// Returns `Ok(true)` for local handlers (response fully written) or
    /// `Ok(false)` for proxy/upload targets (Pingora continues the pipeline).
    ///
    /// Adding a new local handler: implement [`LocalHandlerImpl`] in its module,
    /// then add one arm to [`Self::build_handler`] — this function stays unchanged.
    async fn dispatch_local(
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
                Some(Box::new(metrics_handler::MetricsHandler {
                    token,
                    extra_headers: extra,
                }))
            }

            HandlerKind::StaticFile => {
                let config = self.state.config.load();
                let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                let compress_opts = config
                    .sites
                    .get(site_idx)
                    .and_then(|s| s.compression.as_ref())
                    .and_then(compression::effective);
                let fallback_site = config.sites.get(site_idx).cloned();
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
                    compress_opts,
                    accept_enc,
                    fallback_site,
                }))
            }

            HandlerKind::Fallback => {
                let config = self.state.config.load();
                let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                let site = config.sites.get(site_idx).cloned();
                Some(Box::new(fallback::FallbackHandler {
                    site,
                    extra_headers: extra,
                }))
            }

            HandlerKind::HotReloadJs => Some(Box::new(hot_reload_handler::HotReloadJsHandler {
                extra_headers: extra,
            })),

            HandlerKind::HotReloadSse => {
                let rx = self.state.hot_reload_tx.subscribe();
                Some(Box::new(hot_reload_handler::HotReloadSseHandler {
                    extra_headers: extra,
                    rx: Some(rx),
                }))
            }

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

    /// Evaluate whether a connect-phase error should trigger a retry.
    pub(super) fn try_retry_connect_error(
        &self,
        session: &Session,
        retry: &mut RetryState,
        e: &mut Box<pingora_core::Error>,
    ) {
        use pingora_core::ErrorType::*;
        let is_conn_err = matches!(
            e.etype(),
            ConnectRefused
                | ConnectNoRoute
                | ConnectError
                | ConnectProxyFailure
                | BindError
                | SocketError
        );
        let is_timeout = matches!(e.etype(), ConnectTimedout);
        let condition = if is_conn_err {
            "connection_error"
        } else {
            "timeout"
        };
        // Only retry safe/idempotent HTTP methods — RFC 7231 § 4.2.2.
        let method = session.req_header().method.as_str();
        if is_safe_http_method(method)
            && ((is_conn_err && retry.has_condition("connection_error"))
                || (is_timeout && retry.has_condition("timeout")))
            && self.retry_budget_allows(retry)
        {
            e.set_retry(true);
            self.state
                .metrics
                .retry_attempts_total
                .with_label_values(&["<connect>", condition])
                .inc();
        }
    }

    /// Evaluate whether a proxy-phase error should trigger a retry.
    pub(super) fn try_retry_proxy_error(
        &self,
        session: &Session,
        retry: &mut RetryState,
        e: &mut Box<pingora_core::Error>,
    ) {
        use pingora_core::ErrorType::*;
        let is_timeout = matches!(e.etype(), ReadTimedout | WriteTimedout);
        let is_5xx_retry = matches!(e.etype(), Custom("5xx_retry"));
        let condition = if is_timeout { "timeout" } else { "5xx" };
        // Only retry safe/idempotent methods.
        let method = session.req_header().method.as_str();
        if is_safe_http_method(method)
            && ((is_timeout && retry.has_condition("timeout"))
                || (is_5xx_retry && retry.has_condition("5xx")))
            && self.retry_budget_allows(retry)
        {
            e.set_retry(true);
            let route = session.req_header().uri.path().to_owned();
            self.state
                .metrics
                .retry_attempts_total
                .with_label_values(&[route.as_str(), condition])
                .inc();
        }
    }
}

// ── Trait-method bodies (called from thin delegators in `impl ProxyHttp`) ────

/// Body of [`pingora_proxy::ProxyHttp::request_filter`].
pub(super) async fn request_filter(
    proxy: &ConduitProxy,
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
) -> Result<bool> {
    proxy.do_request_filter(session, ctx).await
}

/// Body of [`pingora_proxy::ProxyHttp::upstream_peer`].
pub(super) async fn upstream_peer(
    proxy: &ConduitProxy,
    ctx: &mut Option<RequestCtx>,
) -> Result<Box<HttpPeer>> {
    let req_ctx = ctx.as_mut().expect("ctx set in request_filter");

    if let Some(ref retry) = req_ctx.retry {
        apply_backoff(retry).await;
    }

    let (addr_str, tls, sni) = resolve_peer_addr(req_ctx)?;

    // For retry attempts (#47 companion fix): restore proxy_upstream_url to
    // the URL for THIS attempt so that logging(), access log, and EWMA
    // tracking all reflect the *actual* upstream that served the response.
    //
    // resolve_peer_addr() already incremented retry.attempt, so the URL
    // used in this call is urls[(attempt - 1) % len].
    if let Some(ref retry) = req_ctx.retry {
        if retry.attempt > 1 {
            // This is a retry (attempt was >0 before incrementing).
            let idx = (retry.attempt - 1) % retry.urls.len();
            req_ctx.proxy_upstream_url = Some(retry.urls[idx].clone());
            // This attempt never went through conn_inc, so it must not
            // inherit a slot to release (record_failed_upstream_for_retry
            // already clears this, but assert the invariant here too since
            // this is the exact site a future change could reintroduce it).
            req_ctx.upstream_conn_slot = false;
        }
    }

    // Derive fallback timeout from `limits.timeoutSecs` on the matched site.
    // Computed here (rather than only below, before `apply_peer_options`) so
    // the same effective connect deadline also bounds DNS resolution below —
    // otherwise a stalled resolver could hold a request open indefinitely
    // with no timeout at all (CodeRabbit finding on PR #227).
    let limits_timeout_secs = {
        let cfg = proxy.state.config.load();
        cfg.sites
            .get(req_ctx.site_idx)
            .and_then(|s| s.limits.as_ref())
            .and_then(|l| l.timeout_secs)
    };
    let resolution_timeout = req_ctx
        .proxy_timeout
        .as_ref()
        .and_then(|t| t.connect_ms)
        .or_else(|| limits_timeout_secs.map(|s| s.saturating_mul(1000)))
        .map(Duration::from_millis);

    let resolve_start = Instant::now();
    let socket_addr = resolve_socket_addr(&addr_str, resolution_timeout).await?;
    let resolve_elapsed = resolve_start.elapsed();
    let mut peer = HttpPeer::new(socket_addr, tls, sni);

    // Negotiate HTTP/2 with the upstream when the route sets `http2: true`.
    if req_ctx.proxy_http2 {
        peer.options.alpn = pingora_core::upstreams::peer::ALPN::H2H1;
    }

    // Apply upstream TLS settings (cert verification, custom server name).
    if let UpstreamTarget::Proxy {
        ref upstream_tls,
        ref sni,
        ..
    } = req_ctx.upstream
    {
        if let Some(tls_cfg) = upstream_tls {
            if let Some(verify) = tls_cfg.verify {
                peer.options.verify_cert = verify;
                peer.options.verify_hostname = verify;
            }
            if let Some(ref server_name) = tls_cfg.server_name {
                peer.options.alternative_cn = Some(server_name.clone());
            }
        }
        // Always set the SNI for TLS connections (already done by HttpPeer::new
        // but explicit here for clarity).
        let _ = sni; // sni already used in HttpPeer::new above
    }

    apply_peer_options(
        &mut peer,
        req_ctx.proxy_timeout.as_ref(),
        req_ctx.proxy_pool.as_ref(),
        limits_timeout_secs,
    );

    // Gitar finding on PR #227: `resolution_timeout` above and
    // `connection_timeout` here are derived from the same configured value,
    // but were two independent deadlines — a hostname upstream stalling at
    // both DNS resolution and TCP connect could consume up to 2x the
    // configured `connectMs`. Sharing one budget instead.
    peer.options.connection_timeout =
        remaining_budget(peer.options.connection_timeout, resolve_elapsed);

    Ok(Box::new(peer))
}

/// Subtract time already spent (e.g. on DNS resolution) from a connect
/// deadline, so two sequential phases share one budget instead of each
/// getting a fresh full timeout.
///
/// `saturating_sub` floors at zero rather than going negative, so a phase
/// that already consumed the whole budget correctly leaves zero time for
/// the next one (which then fails immediately) rather than silently
/// granting it a fresh deadline. `None` (no deadline configured) passes
/// through unchanged — there is no budget to share.
fn remaining_budget(deadline: Option<Duration>, elapsed: Duration) -> Option<Duration> {
    deadline.map(|d| d.saturating_sub(elapsed))
}

/// Body of [`pingora_proxy::ProxyHttp::request_body_filter`].
///
/// Buffer request body chunks for retry replay (stale-if-error pattern).
///
/// Only buffers when:
///   1. The route has `retry` configured.
///   2. The body is within `limits.maxBodyBufferBytes` (default 1 MiB).
///   3. `body_too_large` flag is not already set.
///
/// Uses the linkerd2-proxy ReplayBody pattern: accumulate `Bytes` chunks
/// (cheap reference-counted clones) into `RequestCtx.body_buffer`.  On
/// overflow the buffer is discarded and `body_too_large` is set — retries
/// still happen but without body replay.
pub(super) async fn request_body_filter(
    proxy: &ConduitProxy,
    body: &mut Option<bytes::Bytes>,
    ctx: &mut Option<RequestCtx>,
) -> pingora_core::Result<()> {
    let Some(req_ctx) = ctx.as_mut() else {
        return Ok(());
    };

    let chunk_len = body.as_ref().map(|c| c.len()).unwrap_or(0);
    req_ctx.actual_body_bytes += chunk_len as u64;

    // Enforce maxBodyBytes on the ACTUAL received bytes.
    // The LimitsGuard only checks the Content-Length header; chunked clients bypass it.
    if chunk_len > 0 {
        let max_body = {
            let config = proxy.state.config.load();
            config
                .sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.max_body_bytes)
        };
        if enforce_max_body_bytes(req_ctx, body, chunk_len, max_body) {
            return Ok(());
        }

        // Slow-loris upload defense: leaky-bucket minimum-rate check (#51).
        //
        // Pattern: freenginx `ngx_http_request_body.c` commit b85480cc
        //          (client_body_min_rate).
        //
        // Algorithm:
        //   `excess += chunk_bytes − min_rate × elapsed_secs`
        //
        // "Excess" is the surplus above the minimum rate (positive =
        // client sending fast, negative = client is behind).
        // Surplus is capped at one second's worth (min_rate bytes) to
        // prevent unlimited credit from fast initial bursts.
        // When the client falls more than one second behind
        // (excess < -min_rate) the connection is closed with 408.
        let min_rate_opt = {
            let config = proxy.state.config.load();
            config
                .sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.min_upload_rate_bytes_per_sec)
                .filter(|&r| r > 0)
        };
        if let Some(min_rate) = min_rate_opt {
            let now = std::time::Instant::now();
            // elapsed_secs = 0 on the first chunk so the chunk bytes are
            // credited to the bucket immediately (no time-based drain).
            let elapsed_secs = req_ctx
                .upload_last_chunk
                .map(|last| now.duration_since(last).as_secs_f64())
                .unwrap_or(0.0);
            if upload_rate_step(
                &mut req_ctx.upload_excess_bytes,
                chunk_len,
                min_rate,
                elapsed_secs,
            ) {
                tracing::debug!(
                    min_rate,
                    excess = req_ctx.upload_excess_bytes,
                    "upload rate below minimum — closing connection (408)"
                );
                *body = None;
                return Err(pingora_core::Error::explain(
                    pingora_core::ErrorType::HTTPStatus(408),
                    "upload rate below minUploadRateBytesPerSec",
                ));
            }
            req_ctx.upload_last_chunk = Some(now);
        }
    }

    // Retry body buffering (separate from size enforcement).
    // Only buffer when retry is configured (otherwise wasteful).
    if req_ctx.retry.is_none() || req_ctx.body_too_large {
        return Ok(());
    }

    if let Some(chunk) = body.as_ref() {
        let max_bytes = {
            let config = proxy.state.config.load();
            config
                .sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.max_body_buffer_bytes)
                .unwrap_or(1_048_576) // default 1 MiB
        };
        buffer_body_chunk(req_ctx, chunk, max_bytes);
    }
    Ok(())
}

/// Body of [`pingora_proxy::ProxyHttp::upstream_request_filter`].
pub(super) async fn upstream_request_filter(
    proxy: &ConduitProxy,
    session: &mut Session,
    upstream_request: &mut RequestHeader,
    ctx: &mut Option<RequestCtx>,
) -> Result<()> {
    // Record the moment we start forwarding to the upstream so that
    // `logging()` can compute upstream_response_time_ms.
    // Also increment the per-upstream active-connections gauge.
    if let Some(req_ctx) = ctx.as_mut() {
        req_ctx.upstream_start = Some(std::time::Instant::now());
        if let Some(url) = req_ctx.proxy_upstream_url.as_deref() {
            proxy
                .state
                .metrics
                .upstream_active_connections
                .with_label_values(&[url])
                .inc();
            // Per-upstream selection counters (#40).
            crate::proxy::health::record_upstream_selected(&proxy.state.upstream_health, url);
        }
    }

    append_forwarded_headers(session, upstream_request, &proxy.state, ctx)?;
    apply_upstream_path_transforms(upstream_request, ctx)?;

    // Request header transformation with optional JWT template substitution.
    {
        let config = proxy.state.config.load();
        if let Some(req_ctx) = ctx.as_ref() {
            let site = config.sites.get(req_ctx.site_idx);
            if let Some(transform) = site.and_then(|s| s.request_transform.as_ref()) {
                apply_header_transform_request_with_claims(
                    upstream_request,
                    transform,
                    &req_ctx.jwt_claims,
                )?;
            }
        }
    }

    // Traffic mirroring: fire-and-forget copy to the mirror backend.
    if let Some(req_ctx) = ctx.as_ref() {
        if let UpstreamTarget::Proxy {
            mirror_url: Some(ref mirror),
            ..
        } = req_ctx.upstream
        {
            fire_mirror_request(mirror, session, upstream_request);
        }
    }

    Ok(())
}

/// Body of [`pingora_proxy::ProxyHttp::request_cache_filter`].
///
/// Enable the cache for proxy routes that carry a `cache` configuration.
///
/// Called by Pingora after `request_filter`; only reached for upstream-bound
/// requests (local handlers return `Ok(true)` in `request_filter`).
pub(super) fn request_cache_filter(
    _proxy: &ConduitProxy,
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
) -> Result<()> {
    // The full cache implementation is only compiled when --features cache is active.
    #[cfg(feature = "cache")]
    {
        let Some(req_ctx) = ctx.as_ref() else {
            return Ok(());
        };
        let Some(ref cfg) = req_ctx.proxy_cache_cfg else {
            return Ok(());
        };

        // Select storage backend based on store string.
        let storage: &'static (dyn CacheStorage + Sync) = if cfg.store == "memory" {
            proxy_cache::cache_storage()
        } else if cfg.store.starts_with("redis://") || cfg.store.starts_with("rediss://") {
            #[cfg(feature = "redis")]
            {
                match cache_redis::get_or_create(&cfg.store) {
                    Some(s) => s,
                    None => {
                        tracing::warn!(
                            store = %cfg.store,
                            "Redis cache unavailable — caching disabled for this request"
                        );
                        return Ok(());
                    }
                }
            }
            #[cfg(not(feature = "redis"))]
            {
                tracing::warn!(
                    store = %cfg.store,
                    "Redis cache requires --features redis — caching disabled"
                );
                return Ok(());
            }
        } else if let Some(dir) = cfg.store.strip_prefix("disk:") {
            cache_disk::get_or_create(dir)
        } else {
            tracing::warn!(
                store = %cfg.store,
                "unsupported cache store — caching disabled for this route"
            );
            return Ok(());
        };

        // Check request-side policy (method, cookies, authorization, skip-paths).
        let method = session.req_header().method.as_str();
        let path = session.req_header().uri.path();
        let has_cookie = session.req_header().headers.contains_key("cookie");
        let has_authorization = session.req_header().headers.contains_key("authorization");

        if !proxy_cache::should_cache_request(cfg, method, has_cookie, has_authorization, path) {
            return Ok(());
        }

        // Pass the cache-key lock to prevent thundering herd on cache miss:
        // only one request fetches from upstream; concurrent requests wait for
        // the cached response instead of all hitting the upstream at once.
        session
            .cache
            .enable(storage, None, None, Some(proxy_cache::cache_lock()), None);
    }
    // Without --features cache the entire block above is absent and we fall through.
    #[cfg(not(feature = "cache"))]
    let _ = (session, ctx);
    Ok(())
}

/// Body of [`pingora_proxy::ProxyHttp::should_serve_stale`].
///
/// Stale-while-revalidate / stale-if-error policy.
///
/// - When `error` is `None` Pingora is asking whether to serve a stale
///   response while background revalidation runs (SWR).  We return `true`
///   when the route's `staleWhileRevalidateSecs` is non-zero — Pingora has
///   already checked the stale window via `CacheMeta.serve_stale_while_revalidate()`.
///
/// - When `error` is `Some(_)` Pingora is asking whether to serve stale on
///   upstream error (stale-if-error).  We return `true` when
///   `staleIfErrorSecs` is non-zero and the error comes from upstream.
pub(super) fn should_serve_stale(
    ctx: &Option<RequestCtx>,
    error: Option<&pingora_core::Error>,
) -> bool {
    let Some(req_ctx) = ctx.as_ref() else {
        return false;
    };
    let Some(ref cfg) = req_ctx.proxy_cache_cfg else {
        return false;
    };
    match error {
        // SWR: serve stale while revalidating if window is configured.
        None => cfg.stale_while_revalidate_secs.unwrap_or(0) > 0,
        // Stale-if-error: serve stale on upstream failure.
        Some(e) => {
            cfg.stale_if_error_secs.unwrap_or(0) > 0
                && e.esource() == &pingora_core::ErrorSource::Upstream
        }
    }
}

/// Body of [`pingora_proxy::ProxyHttp::cache_key_callback`].
///
/// Build a deterministic cache key: namespace = Host header, primary = scheme:path[?query].
pub(super) fn cache_key_callback(
    proxy: &ConduitProxy,
    session: &Session,
    ctx: &mut Option<RequestCtx>,
) -> Result<CacheKey> {
    // Use extract_host() so the port suffix is stripped (e.g. "example.com:8080" → "example.com").
    // This keeps the cache key consistent with how the router matches virtual hosts.
    let host_str = extract_host(session);
    let host = host_str.as_str();

    // Derive scheme from whether the matched site has TLS configured.
    let scheme = {
        let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
        let config = proxy.state.config.load();
        if config
            .sites
            .get(site_idx)
            .and_then(|s| s.tls.as_ref())
            .is_some()
        {
            "https"
        } else {
            "http"
        }
    };

    let uri = &session.req_header().uri;
    let path = uri.path();
    let query = uri.query().filter(|q| !q.is_empty());

    // Vary-based cache key differentiation: include the specified request
    // header values so that different representations are stored separately.
    let vary_headers = {
        let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
        let config = proxy.state.config.load();
        config.sites.get(site_idx).and_then(|_s| {
            ctx.as_ref()
                .and_then(|c| c.proxy_cache_cfg.as_ref())
                .and_then(|cc| cc.vary_headers.clone())
        })
    };

    Ok(proxy_cache::build_cache_key(
        host,
        scheme,
        path,
        query,
        vary_headers.as_deref(),
        Some(&session.req_header().headers),
    ))
}

/// Body of [`pingora_proxy::ProxyHttp::fail_to_connect`].
pub(super) fn fail_to_connect(
    proxy: &ConduitProxy,
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
    mut e: Box<pingora_core::Error>,
) -> Box<pingora_core::Error> {
    if let Some(req_ctx) = ctx.as_mut() {
        if let Some(retry) = &mut req_ctx.retry {
            if retry.has_attempts_left() {
                proxy.try_retry_connect_error(session, retry, &mut e);
            }
        }
    }
    e
}

/// Body of [`pingora_proxy::ProxyHttp::error_while_proxy`].
pub(super) fn error_while_proxy(
    proxy: &ConduitProxy,
    peer: &HttpPeer,
    session: &mut Session,
    e: Box<pingora_core::Error>,
    ctx: &mut Option<RequestCtx>,
    client_reused: bool,
) -> Box<pingora_core::Error> {
    let mut e = e.more_context(format!("Peer: {peer}"));
    e.retry
        .decide_reuse(client_reused && !session.as_ref().retry_buffer_truncated());

    if let Some(req_ctx) = ctx.as_mut() {
        if let Some(retry) = &mut req_ctx.retry {
            if retry.has_attempts_left() {
                proxy.try_retry_proxy_error(session, retry, &mut e);
            }
        }
    }
    e
}

// ── request-side helpers ──────────────────────────────────────────────────────

/// Apply ±50 % jitter to a backoff duration.
///
/// Uses splitmix64 seeded from current nanoseconds — the same fast RNG used
/// elsewhere in the proxy.  Returns a value in `[ms/2, ms*3/2)`.
pub(crate) fn jitter_backoff_ms(ms: u64) -> u64 {
    if ms == 0 {
        return 0;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let mut x = seed
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(0x6c62272e07bb0142);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    // jitter ∈ [0, ms) → result ∈ [ms/2, ms/2 + ms) = [ms/2, 3ms/2)
    let jitter = x % ms;
    ms / 2 + jitter
}

/// Sleep for the configured backoff duration when this is a retry attempt (not the first try).
///
/// When `retry.backoff_jitter` is `true`, applies ±50 % randomness to spread
/// retries in time and avoid synchronized thundering herds.
async fn apply_backoff(retry: &RetryState) {
    if retry.attempt > 0 {
        if let Some(ms) = retry.backoff_ms {
            let effective_ms = if retry.backoff_jitter {
                jitter_backoff_ms(ms)
            } else {
                ms
            };
            tokio::time::sleep(Duration::from_millis(effective_ms)).await;
        }
    }
}

/// Returns `true` for HTTP methods that are safe to retry.
///
/// RFC 7231 § 4.2.2 defines **idempotent** methods: a request is idempotent
/// when repeating it has the same effect as sending it once.  We only retry
/// these to prevent double-mutations (double charges, double emails, etc.).
///
/// `PUT` and `DELETE` are technically idempotent but are excluded here because
/// in practice applications often treat them as non-idempotent.  Operators who
/// want to retry them can configure `retry.conditions: ["connection_error"]`
/// without worrying — this function is the default safety gate.
///
/// Safe to retry: GET, HEAD, OPTIONS, TRACE.
pub(super) fn is_safe_http_method(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "TRACE"
    )
}

/// Leaky-bucket minimum-upload-rate step (#51).
///
/// Updates `excess` (surplus bytes above the minimum rate) and returns
/// `true` when the client has fallen more than one second behind the
/// minimum rate and should be rejected with 408.
///
/// # Arguments
/// - `excess` — running surplus in bytes (positive = fast, negative = slow).
///   Modified in place.
/// - `chunk_len` — bytes received in this chunk.
/// - `min_rate` — minimum acceptable rate in bytes per second.
/// - `elapsed_secs` — seconds elapsed since the previous chunk.
///
/// # Algorithm
/// ```text
/// excess += chunk_len − min_rate × elapsed_secs
/// excess  = min(excess, min_rate)   // cap surplus (no unlimited burst credit)
/// reject  = excess < −min_rate       // more than one second of deficit
/// ```
pub(crate) fn upload_rate_step(
    excess: &mut f64,
    chunk_len: usize,
    min_rate: u64,
    elapsed_secs: f64,
) -> bool {
    *excess += chunk_len as f64 - min_rate as f64 * elapsed_secs;
    *excess = excess.min(min_rate as f64);
    *excess < -(min_rate as f64)
}

/// Enforce the `maxBodyBytes` hard limit on actual received bytes.
///
/// Returns `true` when the limit was exceeded (caller should return early).
/// Mutates `body` to `None` to drop the chunk and sets `body_too_large` on the
/// context.  Logs a warning only on the first violation.
pub(super) fn enforce_max_body_bytes(
    req_ctx: &mut RequestCtx,
    body: &mut Option<bytes::Bytes>,
    chunk_len: usize,
    max_body: Option<u64>,
) -> bool {
    let Some(max) = max_body else {
        return false;
    };
    if req_ctx.actual_body_bytes > max {
        // Drop this chunk — prevents forwarding to upstream.
        *body = None;
        let prev = req_ctx.actual_body_bytes - chunk_len as u64;
        if prev <= max {
            // Log only on first violation.
            tracing::warn!(
                actual = req_ctx.actual_body_bytes,
                max,
                "request body exceeded maxBodyBytes (chunked/no Content-Length) \
                 — body dropped, upstream will receive truncated request"
            );
        }
        req_ctx.body_buffer.clear();
        req_ctx.body_too_large = true;
        return true;
    }
    false
}

/// Buffer a body chunk for retry replay (linkerd ReplayBody pattern).
///
/// Clears the buffer and sets `body_too_large` when adding the chunk would
/// exceed `max_bytes`.
pub(super) fn buffer_body_chunk(req_ctx: &mut RequestCtx, chunk: &bytes::Bytes, max_bytes: u64) {
    let current_size: usize = req_ctx.body_buffer.iter().map(|b| b.len()).sum();
    if current_size + chunk.len() > max_bytes as usize {
        // Discard buffer — linkerd pattern: clear on overflow.
        req_ctx.body_buffer.clear();
        req_ctx.body_too_large = true;
        tracing::debug!(
            size = current_size + chunk.len(),
            max = max_bytes,
            "request body exceeded buffer limit — retry will not replay body"
        );
    } else {
        // Cheap clone: Bytes is reference-counted.
        req_ctx.body_buffer.push(chunk.clone());
    }
}

/// Resolve the upstream `(addr, tls, sni)` from the request context.
///
/// On a retry the address rotates through the URL list and the attempt counter
/// is incremented.  On the first attempt the values come from `ctx.upstream`.
pub(super) fn resolve_peer_addr(
    req_ctx: &mut RequestCtx,
) -> pingora_core::Result<(String, bool, String)> {
    if let Some(ref mut retry) = req_ctx.retry {
        let url = &retry.urls[retry.attempt % retry.urls.len()];
        let addr = upstream::url_to_host_port(url).ok_or_else(|| {
            pingora_core::Error::explain(
                pingora_core::ErrorType::ConnectProxyFailure,
                format!("invalid upstream address: {url}"),
            )
        })?;
        let tls = upstream::url_is_tls(url);
        let sni = if tls {
            upstream::url_host(url)
        } else {
            String::new()
        };
        retry.attempt += 1;
        Ok((addr, tls, sni))
    } else {
        match &req_ctx.upstream {
            UpstreamTarget::Proxy { addr, tls, sni, .. } => Ok((addr.clone(), *tls, sni.clone())),
            UpstreamTarget::Upload { addr } => Ok((addr.to_string(), false, String::new())),
            UpstreamTarget::Local(_) => Err(pingora_core::Error::explain(
                pingora_core::ErrorType::InternalError,
                "upstream_peer called for local handler",
            )),
        }
    }
}

/// Resolve a `host:port` string to a [`SocketAddr`], accepting both IP
/// literals and hostnames.
///
/// IP literals take a fast synchronous path (no behavior change from before
/// hostname support was added). Hostnames go through async DNS resolution
/// via [`tokio::net::lookup_host`] — this must NOT be `HttpPeer::new`'s own
/// resolution: that constructor takes `impl std::net::ToSocketAddrs`, which
/// resolves *synchronously* (blocking the async runtime thread) and
/// `.unwrap()`s the result (panics on resolution failure) — unacceptable
/// inside a per-request async hook. Resolving here first, then handing
/// `HttpPeer::new` an already-concrete `SocketAddr`, sidesteps both.
///
/// `resolution_timeout` bounds the DNS lookup with the same effective
/// connect deadline `apply_peer_options` derives for the connection itself
/// (`proxy.*.timeout.connectMs`, falling back to `limits.timeoutSecs`) —
/// without it, a stalled resolver could hold a request open indefinitely
/// with no deadline at all (CodeRabbit finding on PR #227). `None` (no
/// configured timeout at all) resolves without a deadline, matching
/// `apply_peer_options`'s own "absent config = no enforced timeout" default.
async fn resolve_socket_addr(
    addr_str: &str,
    resolution_timeout: Option<Duration>,
) -> pingora_core::Result<SocketAddr> {
    if let Ok(addr) = addr_str.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let lookup = tokio::net::lookup_host(addr_str);
    let addrs = match resolution_timeout {
        Some(d) => tokio::time::timeout(d, lookup).await.map_err(|_| {
            pingora_core::Error::explain(
                pingora_core::ErrorType::ConnectTimedout,
                format!("DNS resolution timed out for upstream address {addr_str}"),
            )
        })?,
        None => lookup.await,
    }
    .map_err(|e| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::ConnectProxyFailure,
            format!("DNS resolution failed for upstream address {addr_str}: {e}"),
        )
    })?;
    pick_preferred_addr(addrs).ok_or_else(|| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::ConnectProxyFailure,
            format!("no addresses found for upstream address {addr_str}"),
        )
    })
}

/// Pick a single address out of a hostname's DNS results, preferring IPv4.
///
/// `HttpPeer` accepts exactly one concrete `SocketAddr` — Pingora has no
/// multi-address / Happy-Eyeballs fallback (an unrelated Happy-Eyeballs
/// backlog item is `[🚫 BLOCKED]` in `CLAUDE.md` for the same reason: no
/// public API for parallel connection attempts). When a hostname resolves
/// to both families, the OS resolver's ordering is not a reliable signal
/// for which family the upstream actually listens on — glibc's
/// `getaddrinfo` prefers IPv6 by RFC 3484 default regardless of whether
/// the target has a real IPv6 listener. Preferring IPv4 deterministically
/// matches the overwhelmingly common case for self-hosted upstreams
/// (Docker service names, `localhost`, bare local dev servers) instead of
/// silently depending on resolver-order luck.
fn pick_preferred_addr(addrs: impl Iterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let addrs: Vec<SocketAddr> = addrs.collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
}

/// Apply per-route timeout, connection-pool settings, and global limits to an
/// `HttpPeer`.
///
/// Priority (highest → lowest):
/// 1. `proxy.*.timeout.*` — per-route fine-grained timeouts
/// 2. `limits.timeoutSecs` — site-wide fallback timeout
///
/// `limits.timeout_secs` is applied to all three timeout fields only when
/// the corresponding per-route field is absent.
pub(super) fn apply_peer_options(
    peer: &mut HttpPeer,
    timeout: Option<&ProxyTimeout>,
    pool: Option<&ConnectionPoolConfig>,
    limits_timeout_secs: Option<u64>,
) {
    let fallback_ms = limits_timeout_secs.map(|s| s.saturating_mul(1000));

    // connection_timeout
    peer.options.connection_timeout = timeout
        .and_then(|t| t.connect_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // read_timeout
    peer.options.read_timeout = timeout
        .and_then(|t| t.read_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // write_timeout
    peer.options.write_timeout = timeout
        .and_then(|t| t.send_ms)
        .or(fallback_ms)
        .map(Duration::from_millis);

    // first_byte_timeout — maps to read_timeout when explicitly set, allowing
    // operators to enforce a tight "time to first byte" window independently
    // of per-read I/O timeouts.  Takes precedence over readMs.
    if let Some(first_byte_ms) = timeout.and_then(|t| t.first_byte_ms) {
        peer.options.read_timeout = Some(Duration::from_millis(first_byte_ms));
    }

    if let Some(p) = pool {
        if let Some(secs) = p.idle_timeout_secs {
            peer.options.idle_timeout = Some(Duration::from_secs(secs));
        }
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

#[derive(Clone)]
enum HandlerKind {
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
struct GuardCtx {
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
fn handler_kind_of(upstream: &UpstreamTarget) -> HandlerKind {
    match upstream {
        UpstreamTarget::Local(LocalHandler::Health) => HandlerKind::Health,
        UpstreamTarget::Local(LocalHandler::AcmeChallenge { .. }) => HandlerKind::AcmeChallenge,
        UpstreamTarget::Local(LocalHandler::Metrics { .. }) => HandlerKind::Metrics,
        UpstreamTarget::Local(LocalHandler::StaticFile { .. }) => HandlerKind::StaticFile,
        UpstreamTarget::Local(LocalHandler::HotReloadSse) => HandlerKind::HotReloadSse,
        UpstreamTarget::Local(LocalHandler::HotReloadJs) => HandlerKind::HotReloadJs,
        UpstreamTarget::Local(LocalHandler::Overloaded) => HandlerKind::Overloaded,
        UpstreamTarget::Local(_) => HandlerKind::Fallback,
        _ => HandlerKind::Proxy,
    }
}

pub(super) fn extract_host(session: &Session) -> String {
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_owned())
        .unwrap_or_default()
}

/// Extract JWT claims from the `Authorization: Bearer` header for header
/// template substitution (`{{ jwt.<claim> }}`).
///
/// Returns `None` when JWT auth is not configured, the current path is in
/// `jwtAuth.skipPaths`, the header is missing or not a Bearer token, or the
/// token cannot be decoded.
///
/// The `skipPaths` check is required here, not just in [`JwtGuard`]: on a
/// skipped path `JwtGuard` lets the request through *without* verifying the
/// token's signature at all (see `jwt::jwt_prelude`), so if this function
/// didn't apply the same check it would decode and trust an attacker-forged,
/// unsigned token's claims for header-template substitution — effectively
/// spoofing `{{ jwt.<claim> }}` values (e.g. `{{ jwt.sub }}`) into whatever
/// upstream header a route's `requestTransform` injects them into, on any
/// path the operator intentionally exempted from auth.
///
/// [`JwtGuard`]: crate::filter::chain::JwtGuard
#[cfg(feature = "jwt")]
fn jwt_claims_from_session(
    session: &Session,
    jwt_cfg: Option<&crate::config::schema::JwtAuthConfig>,
) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    let jwt_cfg = jwt_cfg?;
    let path = session.req_header().uri.path();
    if let Some(skip) = &jwt_cfg.skip_paths {
        if crate::filter::auth::is_path_skipped(Some(skip.as_slice()), path) {
            return None;
        }
    }
    let auth_hdr = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())?;
    let token = crate::filter::jwt::extract_bearer(Some(auth_hdr))?;
    crate::filter::jwt::extract_claims_unchecked(token)
}

/// Append `X-Forwarded-For` and `X-Forwarded-Proto` headers to the upstream request.
pub(super) fn append_forwarded_headers(
    session: &Session,
    upstream_request: &mut RequestHeader,
    state: &crate::proxy::service::AppState,
    ctx: &Option<RequestCtx>,
) -> Result<()> {
    // X-Forwarded-For: chain or start a new entry.
    if let Some(ip) = session
        .client_addr()
        .and_then(|a| a.as_inet())
        .map(|a| a.ip().to_string())
    {
        let xff = match upstream_request
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            Some(existing) => format!("{existing}, {ip}"),
            None => ip,
        };
        upstream_request.insert_header("x-forwarded-for", xff)?;
    }

    // X-Forwarded-Proto: derive from whether the matched site has TLS.
    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
    let proto = if state
        .config
        .load()
        .sites
        .get(site_idx)
        .and_then(|s| s.tls.as_ref())
        .is_some()
    {
        "https"
    } else {
        "http"
    };
    upstream_request.insert_header("x-forwarded-proto", proto)?;

    // X-Forwarded-Host: original Host header so the upstream can reconstruct URLs.
    if let Some(host) = session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
    {
        upstream_request.insert_header("x-forwarded-host", host)?;
    }

    // Via: RFC 7230 §5.7 — identify the proxy hop.
    // Append to any existing Via header rather than replacing it.
    let via_value = match upstream_request
        .headers
        .get("via")
        .and_then(|v| v.to_str().ok())
    {
        Some(existing) => format!("{existing}, 1.1 conduit"),
        None => "1.1 conduit".to_owned(),
    };
    upstream_request.insert_header("via", via_value)?;

    Ok(())
}

/// Apply strip-prefix and path-rewrite transforms for proxy and upload targets.
pub(super) fn apply_upstream_path_transforms(
    upstream_request: &mut RequestHeader,
    ctx: &Option<RequestCtx>,
) -> Result<()> {
    let Some(ctx_ref) = ctx.as_ref() else {
        return Ok(());
    };
    match &ctx_ref.upstream {
        UpstreamTarget::Proxy {
            strip_prefix,
            rewrite,
            ..
        } => {
            let original = upstream_request.uri.path();
            let path = apply_path_strip(original, strip_prefix.as_deref());
            let path = apply_path_rewrites(&path, rewrite.as_deref());
            if path != upstream_request.uri.path() {
                let new_uri = rebuild_uri(&upstream_request.uri, &path)?;
                upstream_request.set_uri(new_uri);
            }
        }
        #[cfg(feature = "upload")]
        UpstreamTarget::Upload { .. } => {
            upstream_request.insert_header("x-conduit-site-idx", ctx_ref.site_idx.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

/// Strip `prefix` from `path`, returning `"/"` when stripping leaves an empty string.
pub(super) fn apply_path_strip(path: &str, prefix: Option<&str>) -> String {
    let Some(pfx) = prefix else {
        return path.to_owned();
    };
    let stripped = path.strip_prefix(pfx).unwrap_or("/");
    if stripped.is_empty() {
        "/".to_owned()
    } else {
        stripped.to_owned()
    }
}

/// Apply the first matching rewrite rule to `path` and return the (possibly unchanged) result.
pub(super) fn apply_path_rewrites(
    path: &str,
    rules: Option<&[crate::config::schema::RewriteRule]>,
) -> String {
    let Some(rules) = rules else {
        return path.to_owned();
    };
    let mut out = path.to_owned();
    for rule in rules {
        match get_rewrite_regex(&rule.from) {
            Some(re) if re.is_match(&out) => {
                out = re.replacen(&out, 1, rule.to.as_str()).into_owned();
                break;
            }
            None => {
                tracing::warn!(
                    pattern = %rule.from,
                    "rewrite rule regex error: invalid pattern (skipped)"
                );
            }
            _ => {}
        }
    }
    out
}

/// Apply request header transform with optional JWT template substitution.
///
/// Supports `{{ jwt.<claim> }}` syntax in header values — replaced with the
/// corresponding claim from the decoded JWT payload.  Unknown claims resolve
/// to an empty string.  Static values (no `{{`) are passed through unchanged.
///
/// Called from `upstream_request_filter` after claims are extracted by
/// `do_request_filter`.
pub(super) fn apply_header_transform_request_with_claims(
    req: &mut RequestHeader,
    transform: &crate::config::schema::HeaderTransformConfig,
    jwt_claims: &Option<std::collections::HashMap<String, serde_json::Value>>,
) -> pingora_core::Result<()> {
    if let Some(remove) = &transform.remove_headers {
        for name in remove {
            req.headers.remove(name.as_str());
        }
    }
    if let Some(set) = &transform.set_headers {
        for (name, value) in set {
            let resolved = if value.contains("{{") {
                expand_jwt_templates(value, jwt_claims)
            } else {
                value.clone()
            };
            req.insert_header(name.clone(), resolved)?;
        }
    }
    Ok(())
}

/// Expand `{{ jwt.<claim> }}` templates in a string.
///
/// Replaces all occurrences of `{{ jwt.CLAIM }}` with the corresponding value
/// from the JWT payload.  Unknown claims are replaced with an empty string.
/// Non-string claim values are JSON-serialized (e.g. numbers, arrays).
/// Expand `{{ jwt.<claim> }}` templates — exposed for unit tests via `pub(crate)`.
pub(crate) fn expand_jwt_templates(
    template: &str,
    claims: &Option<std::collections::HashMap<String, serde_json::Value>>,
) -> String {
    static JWT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = JWT_RE.get_or_init(|| {
        regex::Regex::new(r"\{\{\s*jwt\.(\w+)\s*\}\}").expect("jwt template regex")
    });

    re.replace_all(template, |caps: &regex::Captures<'_>| {
        let claim_name = &caps[1];
        match claims {
            Some(map) => match map.get(claim_name) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            },
            None => String::new(),
        }
    })
    .into_owned()
}

/// Fire-and-forget a copy of the current request to a mirror backend.
///
/// The mirror task is detached (spawned with `tokio::spawn`); its response is
/// discarded and any error is silently logged at DEBUG level.  The primary
/// request processing is unaffected by mirror success or failure.
///
/// **V1 limitation:** only the method, path, query, and request headers are
/// mirrored.  The request body is not buffered and is therefore not mirrored.
pub(super) fn fire_mirror_request(
    mirror_url: &str,
    session: &Session,
    upstream_request: &RequestHeader,
) {
    // Build the mirror URL: base URL + path + query from the upstream request.
    let path_and_query = upstream_request
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| upstream_request.uri.path());

    let target_url = {
        let base = mirror_url.trim_end_matches('/');
        format!("{base}{path_and_query}")
    };

    // Collect request headers (skip hop-by-hop and host).
    let method = upstream_request.method.clone();
    let mut headers = Vec::new();
    for (name, value) in upstream_request.headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "te"
                | "trailer"
                | "upgrade"
                | "proxy-authorization"
                | "proxy-authenticate"
                | "host"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            headers.push((n, v.to_owned()));
        }
    }
    // Add X-Mirrored-From so the mirror can distinguish shadow traffic.
    let primary_host = session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned();
    headers.push(("x-mirrored-from".to_owned(), primary_host));

    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "mirror: failed to build client");
                return;
            }
        };

        let mut req = client.request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
            &target_url,
        );
        for (name, value) in &headers {
            if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) {
                    req = req.header(header_name, header_value);
                }
            }
        }

        match req.send().await {
            Ok(resp) => {
                tracing::debug!(
                    url = %target_url,
                    status = resp.status().as_u16(),
                    "mirror: response received (discarded)"
                );
            }
            Err(e) => {
                tracing::debug!(url = %target_url, error = %e, "mirror: request failed");
            }
        }
    });
}

/// Return a compiled [`regex::Regex`] for `pattern`, using a process-wide cache
/// to avoid recompiling the same pattern on every request.
///
/// Rewrite patterns are plain (un-anchored) regexes so that `replacen` can
/// match anywhere in the path.  Invalid patterns are not stored; the caller
/// should log the error and skip the rule.
pub(super) fn get_rewrite_regex(pattern: &str) -> Option<regex::Regex> {
    static CACHE: std::sync::OnceLock<DashMap<String, regex::Regex>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(DashMap::new);
    if let Some(re) = cache.get(pattern) {
        return Some(re.clone());
    }
    let re = regex::Regex::new(pattern).ok()?;
    cache.insert(pattern.to_owned(), re.clone());
    Some(re)
}

pub(super) fn rebuild_uri(original: &http::Uri, new_path: &str) -> Result<http::Uri> {
    let pq = match original.query() {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path.to_string(),
    };
    let mut parts = http::uri::Parts::default();
    parts.scheme = original.scheme().cloned();
    parts.authority = original.authority().cloned();
    parts.path_and_query = Some(pq.parse().map_err(|_| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            "failed to build upstream URI",
        )
    })?);
    http::Uri::from_parts(parts).map_err(|_| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::InternalError,
            "failed to build upstream URI",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::schema::AppConfig;
    use crate::proxy::service::AppState;

    // ── jitter_backoff_ms ─────────────────────────────────────────────────────

    #[test]
    fn jitter_result_is_within_50_percent_range() {
        // Run 100 times to reduce flakiness from the time-seeded RNG.
        for _ in 0..100 {
            let ms = 200u64;
            let result = jitter_backoff_ms(ms);
            assert!(
                result >= ms / 2 && result < ms + ms / 2,
                "jitter result {result} must be in [{}, {})",
                ms / 2,
                ms + ms / 2
            );
        }
    }

    #[test]
    fn jitter_zero_ms_returns_zero() {
        assert_eq!(jitter_backoff_ms(0), 0);
    }

    #[test]
    fn jitter_one_ms_returns_zero_or_one() {
        let result = jitter_backoff_ms(1);
        // ms=1 → ms/2 = 0, jitter ∈ [0, 1) → result ∈ {0}
        assert!(result < 2, "result {result} out of range for ms=1");
    }

    // ── is_safe_http_method ───────────────────────────────────────────────────

    #[test]
    fn safe_methods_are_safe() {
        assert!(is_safe_http_method("GET"));
        assert!(is_safe_http_method("HEAD"));
        assert!(is_safe_http_method("OPTIONS"));
        assert!(is_safe_http_method("TRACE"));
    }

    #[test]
    fn unsafe_methods_are_not_safe() {
        assert!(!is_safe_http_method("POST"));
        assert!(!is_safe_http_method("PUT"));
        assert!(!is_safe_http_method("DELETE"));
        assert!(!is_safe_http_method("PATCH"));
    }

    #[test]
    fn safe_method_case_insensitive() {
        assert!(is_safe_http_method("get"));
        assert!(is_safe_http_method("Get"));
    }

    // ── apply_path_strip ──────────────────────────────────────────────────────

    #[test]
    fn apply_path_strip_removes_prefix() {
        assert_eq!(apply_path_strip("/api/v1/users", Some("/api")), "/v1/users");
    }

    #[test]
    fn apply_path_strip_no_prefix_returns_unchanged() {
        assert_eq!(apply_path_strip("/api/v1", None), "/api/v1");
    }

    #[test]
    fn apply_path_strip_exact_match_returns_root() {
        // Stripping the exact path leaves an empty string → normalize to "/".
        assert_eq!(apply_path_strip("/api", Some("/api")), "/");
    }

    #[test]
    fn apply_path_strip_no_match_returns_root() {
        // Prefix doesn't match → `strip_prefix` returns None → "/" returned.
        assert_eq!(apply_path_strip("/other", Some("/api")), "/");
    }

    // ── apply_path_rewrites ───────────────────────────────────────────────────

    #[test]
    fn apply_path_rewrites_no_rules_returns_unchanged() {
        assert_eq!(apply_path_rewrites("/v1/users", None), "/v1/users");
    }

    #[test]
    fn apply_path_rewrites_no_match_returns_unchanged() {
        let rules = vec![crate::config::schema::RewriteRule {
            from: "^/api/(.*)".to_owned(),
            to: "/v2/$1".to_owned(),
        }];
        assert_eq!(
            apply_path_rewrites("/other/path", Some(&rules)),
            "/other/path"
        );
    }

    #[test]
    fn apply_path_rewrites_matching_rule_transforms_path() {
        let rules = vec![crate::config::schema::RewriteRule {
            from: "^/v1/(.*)".to_owned(),
            to: "/v2/$1".to_owned(),
        }];
        assert_eq!(apply_path_rewrites("/v1/users", Some(&rules)), "/v2/users");
    }

    #[test]
    fn apply_path_rewrites_first_match_wins() {
        let rules = vec![
            crate::config::schema::RewriteRule {
                from: "^/v1/(.*)".to_owned(),
                to: "/first/$1".to_owned(),
            },
            crate::config::schema::RewriteRule {
                from: "^/v1/(.*)".to_owned(),
                to: "/second/$1".to_owned(),
            },
        ];
        assert_eq!(
            apply_path_rewrites("/v1/users", Some(&rules)),
            "/first/users"
        );
    }

    // ── expand_jwt_templates (additional cases) ───────────────────────────────

    #[test]
    fn expand_jwt_templates_no_template_unchanged() {
        let claims: std::collections::HashMap<String, serde_json::Value> = Default::default();
        let result = expand_jwt_templates("plain-value", &Some(claims));
        assert_eq!(result, "plain-value");
    }

    #[test]
    fn expand_jwt_templates_null_claims_returns_empty() {
        let result = expand_jwt_templates("{{ jwt.sub }}", &None);
        assert_eq!(result, "");
    }

    // ── resolve_peer_addr ─────────────────────────────────────────────────────

    fn make_ctx(upstream: UpstreamTarget) -> RequestCtx {
        RequestCtx::new(0, upstream, None, None, None, false, None, None, None)
    }

    #[test]
    fn resolve_peer_addr_proxy_returns_addr_tls_sni() {
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        let (addr, tls, sni) = resolve_peer_addr(&mut ctx).unwrap();
        assert_eq!(addr, "backend:4000");
        assert!(!tls);
        assert!(sni.is_empty());
    }

    #[test]
    fn resolve_peer_addr_https_sets_tls_and_sni() {
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "api.example.com:443".to_owned(),
            tls: true,
            sni: "api.example.com".to_owned(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        let (addr, tls, sni) = resolve_peer_addr(&mut ctx).unwrap();
        assert_eq!(addr, "api.example.com:443");
        assert!(tls);
        assert_eq!(sni, "api.example.com");
    }

    #[test]
    fn resolve_peer_addr_local_handler_returns_error() {
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        assert!(
            resolve_peer_addr(&mut ctx).is_err(),
            "local handler must return error"
        );
    }

    // ── resolve_socket_addr (#225: hostname upstreams) ──────────────────────────

    #[tokio::test]
    async fn resolve_socket_addr_ipv4_literal_fast_path() {
        let addr = resolve_socket_addr("127.0.0.1:4000", None).await.unwrap();
        assert_eq!(addr, "127.0.0.1:4000".parse().unwrap());
    }

    #[tokio::test]
    async fn resolve_socket_addr_ipv6_literal_fast_path() {
        let addr = resolve_socket_addr("[::1]:4000", None).await.unwrap();
        assert_eq!(addr, "[::1]:4000".parse().unwrap());
    }

    #[tokio::test]
    async fn resolve_socket_addr_resolves_localhost_hostname() {
        // The exact regression case from #225: "localhost:4000" previously
        // failed SocketAddr::parse and every request to such an upstream
        // would 502. localhost resolves via the OS hosts file/resolver
        // without needing network access, so this is safe to run in CI.
        let addr = resolve_socket_addr("localhost:4000", None).await.unwrap();
        assert!(
            addr.ip().is_loopback(),
            "localhost must resolve to a loopback address, got {addr}"
        );
        assert_eq!(addr.port(), 4000);
    }

    #[tokio::test]
    async fn resolve_socket_addr_unresolvable_hostname_returns_error() {
        let result = resolve_socket_addr("this-host-does-not-exist.invalid:4000", None).await;
        assert!(
            result.is_err(),
            "an unresolvable hostname must return an error, not panic"
        );
    }

    #[tokio::test]
    async fn resolve_socket_addr_ip_literal_ignores_zero_timeout() {
        // The IP-literal fast path never touches the resolver at all, so
        // even a timeout of 0 (which would fire instantly on any real DNS
        // lookup) must not affect it — this is the "no behavior change for
        // the previously-working case" guarantee from the PR description.
        let addr = resolve_socket_addr("127.0.0.1:4000", Some(Duration::ZERO))
            .await
            .unwrap();
        assert_eq!(addr, "127.0.0.1:4000".parse().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn resolve_socket_addr_hostname_respects_timeout() {
        // CodeRabbit finding on PR #227: without a bound, a stalled resolver
        // could hold the request open indefinitely. Racing a zero-duration
        // timeout against a *real* DNS lookup would be flaky — `localhost`
        // often resolves fast enough to win that race on some hosts (this
        // was caught locally: the first version of this test used a real
        // clock and failed non-deterministically). Using a paused virtual
        // clock instead makes this deterministic: `tokio::net::lookup_host`
        // runs on Tokio's blocking-thread pool, so it always returns
        // `Poll::Pending` on its first poll — it cannot complete
        // synchronously within that same poll. The zero-duration deadline
        // has therefore already elapsed on the paused clock by the time
        // `Timeout` checks it, so the timeout branch always wins.
        let result = resolve_socket_addr("localhost:4000", Some(Duration::ZERO)).await;
        assert!(
            result.is_err(),
            "a hostname lookup must respect an expired timeout, not block indefinitely"
        );
    }

    // ── pick_preferred_addr (Gitar finding on PR #227) ──────────────────────────

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port))
    }

    #[test]
    fn pick_preferred_addr_empty_returns_none() {
        assert_eq!(pick_preferred_addr(std::iter::empty()), None);
    }

    #[test]
    fn pick_preferred_addr_only_ipv6_returns_it() {
        let addr = v6(4000);
        assert_eq!(pick_preferred_addr(std::iter::once(addr)), Some(addr));
    }

    #[test]
    fn pick_preferred_addr_only_ipv4_returns_it() {
        let addr = v4(4000);
        assert_eq!(pick_preferred_addr(std::iter::once(addr)), Some(addr));
    }

    #[test]
    fn pick_preferred_addr_prefers_ipv4_when_ipv6_listed_first() {
        // The exact failure mode Gitar flagged on PR #227: glibc's
        // getaddrinfo (RFC 3484) commonly lists the IPv6 record before the
        // IPv4 one for "localhost", even when the target only listens on
        // IPv4. Taking .next() unconditionally would pick the IPv6 address
        // and fail to connect.
        let ipv4 = v4(4000);
        let ipv6 = v6(4000);
        let picked = pick_preferred_addr(vec![ipv6, ipv4].into_iter());
        assert_eq!(picked, Some(ipv4), "must prefer IPv4 regardless of order");
    }

    #[test]
    fn pick_preferred_addr_prefers_ipv4_when_ipv4_listed_first() {
        let ipv4 = v4(4000);
        let ipv6 = v6(4000);
        let picked = pick_preferred_addr(vec![ipv4, ipv6].into_iter());
        assert_eq!(picked, Some(ipv4));
    }

    // ── remaining_budget (Gitar finding on PR #227: shared connect budget) ──────

    #[test]
    fn remaining_budget_none_deadline_stays_none() {
        // No timeout configured at all -- nothing to share, passes through.
        assert_eq!(remaining_budget(None, Duration::from_millis(500)), None);
    }

    #[test]
    fn remaining_budget_subtracts_elapsed() {
        let deadline = Duration::from_millis(5000);
        let elapsed = Duration::from_millis(1200);
        assert_eq!(
            remaining_budget(Some(deadline), elapsed),
            Some(Duration::from_millis(3800))
        );
    }

    #[test]
    fn remaining_budget_zero_elapsed_is_unchanged() {
        // The IP-literal fast path in resolve_socket_addr never awaits DNS,
        // so elapsed is ~0 -- the configured deadline must be preserved
        // exactly, not just "close to" it.
        let deadline = Duration::from_millis(5000);
        assert_eq!(
            remaining_budget(Some(deadline), Duration::ZERO),
            Some(deadline)
        );
    }

    #[test]
    fn remaining_budget_elapsed_exceeding_deadline_saturates_to_zero() {
        // A resolution that already consumed the whole budget (or more, if
        // resolution_timeout itself elapsed) must leave zero time for the
        // next phase -- not underflow/panic, and not silently grant a fresh
        // deadline by wrapping.
        let deadline = Duration::from_millis(1000);
        let elapsed = Duration::from_millis(1500);
        assert_eq!(
            remaining_budget(Some(deadline), elapsed),
            Some(Duration::ZERO)
        );
    }

    // ── apply_peer_options ───────────────────────────────────────────────────

    #[test]
    fn apply_peer_options_sets_timeouts() {
        use crate::config::schema::ProxyTimeout;
        // Use IP address to avoid DNS lookup in tests
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let timeout = ProxyTimeout {
            connect_ms: Some(500),
            read_ms: Some(1000),
            send_ms: Some(2000),
            per_try_ms: None,
            first_byte_ms: None,
        };
        apply_peer_options(&mut peer, Some(&timeout), None, None);
        assert_eq!(
            peer.options.connection_timeout,
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(
            peer.options.read_timeout,
            Some(std::time::Duration::from_millis(1000))
        );
        assert_eq!(
            peer.options.write_timeout,
            Some(std::time::Duration::from_millis(2000))
        );
    }

    #[test]
    fn apply_peer_options_uses_fallback_timeout() {
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        // No per-route timeout, but global limits.timeoutSecs = 5s → 5000ms fallback
        apply_peer_options(&mut peer, None, None, Some(5));
        assert_eq!(
            peer.options.connection_timeout,
            Some(std::time::Duration::from_millis(5000))
        );
    }

    #[test]
    fn apply_peer_options_no_timeout_leaves_defaults() {
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        apply_peer_options(&mut peer, None, None, None);
        // No timeout set → remains None (Pingora's default).
        assert!(peer.options.connection_timeout.is_none());
        assert!(peer.options.read_timeout.is_none());
    }

    #[test]
    fn apply_peer_options_sets_idle_timeout() {
        use crate::config::schema::ConnectionPoolConfig;
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let pool = ConnectionPoolConfig {
            max_idle: None,
            idle_timeout_secs: Some(30),
        };
        apply_peer_options(&mut peer, None, Some(&pool), None);
        assert_eq!(
            peer.options.idle_timeout,
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn apply_peer_options_first_byte_ms_overrides_read_timeout() {
        use crate::config::schema::ProxyTimeout;
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let mut peer = HttpPeer::new(addr, false, String::new());
        let timeout = ProxyTimeout {
            connect_ms: None,
            read_ms: Some(30_000), // 30s general read timeout
            send_ms: None,
            per_try_ms: None,
            first_byte_ms: Some(500), // 500ms first-byte timeout overrides readMs
        };
        apply_peer_options(&mut peer, Some(&timeout), None, None);
        // first_byte_ms takes precedence over read_ms
        assert_eq!(
            peer.options.read_timeout,
            Some(std::time::Duration::from_millis(500))
        );
    }

    // ── get_rewrite_regex ─────────────────────────────────────────────────────

    #[test]
    fn get_rewrite_regex_compiles_valid_pattern() {
        let re = get_rewrite_regex("^/v1/(.*)");
        assert!(re.is_some(), "valid regex must compile");
        let re = re.unwrap();
        assert!(re.is_match("/v1/users"));
        assert!(!re.is_match("/v2/users"));
    }

    #[test]
    fn get_rewrite_regex_returns_none_for_invalid() {
        let re = get_rewrite_regex("[invalid");
        assert!(re.is_none(), "invalid regex must return None");
    }

    #[test]
    fn get_rewrite_regex_caches_compiled_pattern() {
        // Second call for the same pattern must return a cached copy.
        let r1 = get_rewrite_regex("^/api/(.*)");
        let r2 = get_rewrite_regex("^/api/(.*)");
        assert!(r1.is_some() && r2.is_some(), "both calls must succeed");
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
    fn build_handler_hot_reload_js_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::HotReloadJs)));
        let result = proxy.build_handler(HandlerKind::HotReloadJs, &ctx);
        assert!(result.is_some(), "HotReloadJs handler must return Some");
    }

    #[test]
    fn build_handler_hot_reload_sse_returns_some() {
        let proxy = make_proxy();
        let ctx = Some(make_ctx(UpstreamTarget::Local(LocalHandler::HotReloadSse)));
        let result = proxy.build_handler(HandlerKind::HotReloadSse, &ctx);
        assert!(result.is_some(), "HotReloadSse handler must return Some");
    }

    #[test]
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
    fn build_handler_fallback_returns_some() {
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

    // ── retry_budget_allows ───────────────────────────────────────────────────

    fn make_proxy() -> ConduitProxy {
        let config = crate::config::schema::AppConfig::default();
        let state = AppState::new(config, std::path::PathBuf::from("."), None);
        ConduitProxy {
            state: std::sync::Arc::new(state),
        }
    }

    #[test]
    fn retry_budget_allows_without_budget_config() {
        let proxy = make_proxy();
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None, // no budget limit
            is_retrying: false,
        };
        // No budget configured → always allows retry.
        assert!(proxy.retry_budget_allows(&mut retry));
        assert!(retry.is_retrying);
    }

    #[test]
    fn retry_budget_allows_within_budget() {
        let proxy = make_proxy();
        proxy.state.inflight.store(10, Ordering::Relaxed); // 10 inflight
        proxy.state.retry_inflight.store(0, Ordering::Relaxed); // 0 retries
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: Some(50.0), // 50% budget → up to 5 retries
            is_retrying: false,
        };
        assert!(
            proxy.retry_budget_allows(&mut retry),
            "within budget must allow"
        );
        assert!(retry.is_retrying);
    }

    #[test]
    fn retry_budget_denies_when_exhausted() {
        let proxy = make_proxy();
        proxy.state.inflight.store(10, Ordering::Relaxed); // 10 inflight
        proxy.state.retry_inflight.store(10, Ordering::Relaxed); // already 10 retries = 100% of budget
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: Some(50.0), // 50% → max 5, current=10 → denied
            is_retrying: false,
        };
        assert!(
            !proxy.retry_budget_allows(&mut retry),
            "exhausted budget must deny"
        );
        assert!(!retry.is_retrying);
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

    // ── apply_upstream_path_transforms ───────────────────────────────────────

    #[test]
    fn apply_upstream_path_transforms_strips_prefix() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api/v1/users", None).unwrap();
        let ctx = Some(make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: Some("/api".to_owned()),
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        }));
        apply_upstream_path_transforms(&mut req, &ctx).unwrap();
        assert_eq!(req.uri.path(), "/v1/users");
    }

    #[test]
    fn apply_upstream_path_transforms_no_prefix_unchanged() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api/v1/users", None).unwrap();
        let ctx = Some(make_ctx(UpstreamTarget::Proxy {
            addr: "backend:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        }));
        apply_upstream_path_transforms(&mut req, &ctx).unwrap();
        assert_eq!(req.uri.path(), "/api/v1/users");
    }

    #[test]
    fn apply_upstream_path_transforms_none_ctx_noop() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/original", None).unwrap();
        apply_upstream_path_transforms(&mut req, &None).unwrap();
        assert_eq!(req.uri.path(), "/original");
    }

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

    // ── apply_header_transform_request_with_claims ────────────────────────────

    #[test]
    fn header_transform_sets_header() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: Some(
                [("x-env".to_owned(), "production".to_owned())]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            remove_headers: None,
        };
        apply_header_transform_request_with_claims(&mut req, &transform, &None).unwrap();
        assert_eq!(req.headers.get("x-env").unwrap(), "production");
    }

    #[test]
    fn header_transform_removes_header() {
        use pingora_http::RequestHeader;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        req.insert_header("x-remove", "bye").unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: None,
            remove_headers: Some(vec!["x-remove".to_owned()]),
        };
        apply_header_transform_request_with_claims(&mut req, &transform, &None).unwrap();
        assert!(
            req.headers.get("x-remove").is_none(),
            "header must be removed"
        );
    }

    #[test]
    fn header_transform_with_jwt_template_substitution() {
        use pingora_http::RequestHeader;
        use std::collections::HashMap;
        let mut req = RequestHeader::build("GET", b"/api", None).unwrap();
        let transform = crate::config::schema::HeaderTransformConfig {
            set_headers: Some(
                [("x-user".to_owned(), "{{ jwt.sub }}".to_owned())]
                    .iter()
                    .cloned()
                    .collect(),
            ),
            remove_headers: None,
        };
        let mut claims = HashMap::new();
        claims.insert("sub".to_owned(), serde_json::json!("alice"));
        apply_header_transform_request_with_claims(&mut req, &transform, &Some(claims)).unwrap();
        assert_eq!(req.headers.get("x-user").unwrap(), "alice");
    }

    // ── rebuild_uri ───────────────────────────────────────────────────────────

    #[test]
    fn rebuild_uri_replaces_path() {
        let original: http::Uri = "/old/path".parse().unwrap();
        let new_uri = rebuild_uri(&original, "/new/path").unwrap();
        assert_eq!(new_uri.path(), "/new/path");
    }

    #[test]
    fn rebuild_uri_preserves_query() {
        let original: http::Uri = "/old/path?foo=bar".parse().unwrap();
        let new_uri = rebuild_uri(&original, "/new/path").unwrap();
        assert_eq!(new_uri.path(), "/new/path");
        assert_eq!(new_uri.query(), Some("foo=bar"));
    }

    #[test]
    fn rebuild_uri_no_query_keeps_no_query() {
        let original: http::Uri = "/path".parse().unwrap();
        let new_uri = rebuild_uri(&original, "/v2").unwrap();
        assert_eq!(new_uri.path(), "/v2");
        assert!(new_uri.query().is_none());
    }

    // ── resolve_peer_addr ─────────────────────────────────────────────────────

    #[test]
    fn resolve_peer_addr_with_retry_returns_first_url() {
        let retry = RetryState {
            urls: vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None,
            is_retrying: false,
        };
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "original:4000".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        ctx.retry = Some(retry);
        let (addr, _, _) = resolve_peer_addr(&mut ctx).unwrap();
        assert_eq!(addr, "a:4000");
        // Attempt should be incremented.
        assert_eq!(ctx.retry.unwrap().attempt, 1);
    }

    // ── upload_rate_step (leaky-bucket minimum rate, #51) ────────────────────

    #[test]
    fn upload_rate_step_at_exact_min_rate_keeps_excess_zero() {
        let mut excess = 0.0f64;
        let min_rate = 1024u64; // 1 KiB/s
                                // Exactly 1024 bytes in exactly 1 second → excess stays at 0.
        let rejected = upload_rate_step(&mut excess, 1024, min_rate, 1.0);
        assert!(!rejected, "exactly at min rate must not reject");
        assert!(
            (excess - 0.0).abs() < 0.01,
            "excess should be ~0, got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_above_min_rate_accumulates_surplus() {
        let mut excess = 0.0f64;
        let min_rate = 1024u64;
        // 2048 bytes in 1 second (twice the minimum rate) → surplus = 1024.
        let rejected = upload_rate_step(&mut excess, 2048, min_rate, 1.0);
        assert!(!rejected, "above min rate must not reject");
        // Surplus capped at min_rate (1024).
        assert!(
            (excess - 1024.0).abs() < 0.01,
            "surplus capped at min_rate: got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_surplus_is_capped_at_one_second() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // Enormous burst: 1_000_000 bytes in 0.01 seconds.
        upload_rate_step(&mut excess, 1_000_000, min_rate, 0.01);
        // Surplus must be capped at min_rate (1000) — not the raw 999_990.
        assert!(
            excess <= min_rate as f64 + 0.01,
            "surplus must be capped at min_rate: got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_below_min_rate_accumulates_deficit() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // 100 bytes in 1 second (1/10 of min rate) → deficit grows.
        let rejected = upload_rate_step(&mut excess, 100, min_rate, 1.0);
        // deficit = 100 - 1000 = -900; not yet past -1000 threshold.
        assert!(
            !rejected,
            "single slow chunk below min rate but deficit < min_rate"
        );
        assert!(excess < 0.0, "excess must be negative (deficit): {excess}");
    }

    #[test]
    fn upload_rate_step_rejects_when_deficit_exceeds_one_second() {
        let mut excess = -(1000f64 - 1.0); // just below the rejection threshold
        let min_rate = 1000u64;
        // One more tiny chunk with a 1-second gap: excess += 1 - 1000 → -1999+1 = -1999
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 1.0);
        assert!(rejected, "deficit > min_rate must trigger rejection");
        assert!(
            excess < -(min_rate as f64),
            "excess must be below -min_rate: {excess}"
        );
    }

    #[test]
    fn upload_rate_step_carries_over_surplus_for_slow_periods() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // First chunk: big burst that fills the surplus bucket.
        upload_rate_step(&mut excess, 10_000, min_rate, 0.0);
        // Surplus capped at 1000.
        assert!((excess - 1000.0).abs() < 0.01, "surplus capped: {excess}");

        // Second chunk: very slow (1 byte in 1 second).
        // excess += 1 - 1000 → 1000 + 1 - 1000 = 1.
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 1.0);
        assert!(!rejected, "surplus from burst must absorb one slow chunk");
        assert!(excess >= 0.0, "excess should remain non-negative: {excess}");
    }

    #[test]
    fn upload_rate_step_zero_elapsed_never_rejects() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // First call always has elapsed=0 (first chunk in request_body_filter).
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 0.0);
        assert!(!rejected, "first chunk (elapsed=0) must never reject");
    }

    // ── record_failed_upstream_for_retry ──────────────────────────────────────

    /// ctx = None → function must return without panicking.
    #[test]
    fn record_failed_upstream_ctx_none_is_noop() {
        let proxy = make_proxy();
        let mut ctx: Option<RequestCtx> = None;
        let config = AppConfig::default();
        // Must not panic.
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 500);
    }

    /// ctx is Some but proxy_upstream_url is None → function returns early,
    /// failed_upstream_attempts stays empty.
    #[test]
    fn record_failed_upstream_url_none_returns_early() {
        let proxy = make_proxy();
        let inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        // proxy_upstream_url defaults to None — no URL to record.
        assert!(inner.proxy_upstream_url.is_none());
        let mut ctx = Some(inner);
        let config = AppConfig::default();
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 503);
        // No attempt should have been pushed.
        assert!(ctx.unwrap().failed_upstream_attempts.is_empty());
    }

    /// Happy path: URL is set → it is taken (cleared), the attempt is pushed,
    /// and upstream_start is reset to None.
    #[test]
    fn record_failed_upstream_records_attempt_and_clears_url() {
        let proxy = make_proxy();
        let mut inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        inner.proxy_upstream_url = Some("http://backend:4000".to_owned());
        inner.upstream_start = None;
        let mut ctx = Some(inner);
        let config = AppConfig::default();
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 502);
        let req = ctx.as_ref().unwrap();
        // URL must have been taken (cleared).
        assert!(req.proxy_upstream_url.is_none());
        // The failed attempt must be recorded.
        assert_eq!(req.failed_upstream_attempts.len(), 1);
        assert_eq!(req.failed_upstream_attempts[0].0, "http://backend:4000");
        assert_eq!(req.failed_upstream_attempts[0].1, 502);
        // upstream_start should remain None (wasn't set).
        assert!(req.upstream_start.is_none());
    }

    /// When upstream_start is set, the latency histogram observe branch runs.
    #[test]
    fn record_failed_upstream_observes_latency_when_start_is_set() {
        let proxy = make_proxy();
        let mut inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        inner.proxy_upstream_url = Some("http://backend:4001".to_owned());
        inner.upstream_start = Some(std::time::Instant::now());
        let mut ctx = Some(inner);
        let config = AppConfig::default();
        // Must not panic even when upstream_start is Some.
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 500);
        let req = ctx.as_ref().unwrap();
        // upstream_start reset to None after recording.
        assert!(req.upstream_start.is_none());
        assert_eq!(req.failed_upstream_attempts.len(), 1);
    }

    /// When the site config has outlier_detection set, maybe_eject() is called.
    #[test]
    fn record_failed_upstream_triggers_outlier_detection_when_configured() {
        let proxy = make_proxy();
        let mut inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        inner.proxy_upstream_url = Some("http://backend:4002".to_owned());
        inner.site_idx = 0;
        let mut ctx = Some(inner);

        // Build an AppConfig with outlier detection enabled on site 0.
        let mut config = AppConfig::default();
        let site = crate::config::schema::SiteConfig {
            outlier_detection: Some(crate::config::schema::OutlierDetectionConfig {
                consecutive_5xx: Some(1),
                base_ejection_time_secs: Some(5),
                max_ejection_time_secs: Some(30),
                max_ejection_percent: Some(50),
            }),
            ..Default::default()
        };
        config.sites = vec![site];

        // Must not panic — exercises the maybe_eject() call inside the if-let branch.
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 503);
        let req = ctx.as_ref().unwrap();
        assert_eq!(req.failed_upstream_attempts.len(), 1);
    }
}
