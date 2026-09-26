//! Request-phase guard-chain orchestration and routing dispatch.
//!
//! `do_request_filter` is the top of the per-request pipeline: builds the
//! `FilterChain` ([`run_guard_filters`]), routes the request, applies
//! post-routing rate-limit/priority-shedding checks, and dispatches to
//! either a local handler or Pingora's own upstream-proxy path.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.
//!
//! ~761 production lines, over `conventions.md`'s 400-line soft limit --
//! deliberately kept as one file rather than split further: this is a
//! single coherent concern (the guard-chain orchestration this module's
//! name describes), and every function here is called, directly or
//! transitively, from exactly one entry point (`do_request_filter`). A
//! further split would scatter one call graph across more files without
//! reducing complexity, purely to satisfy the line count -- the same
//! tradeoff already accepted for `retry.rs` in this same PR.

use std::sync::atomic::Ordering;

use pingora_core::Result;
use pingora_proxy::Session;

use crate::config::schema::{MiddlewareEntry, SiteConfig};
use crate::filter::auth;
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
use crate::filter::{cors, redirects, security_headers};
use crate::handler::response;
use crate::proxy::ctx::{AcceptEncoding, RequestCtx};
use crate::proxy::request::transform::extract_host;
use crate::proxy::request::{handler_kind_of, GuardCtx, HandlerKind};
use crate::proxy::router;
use crate::proxy::service::ConduitProxy;

impl ConduitProxy {
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
            site_label: site_label.clone(),
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
        // Reuses the same `site_label` guards.site_label was built from —
        // scopes the route-level bucket key so two sites with the same route
        // key and client don't collide (#304's route-level twin).
        if self
            .enforce_route_rate_limit(session, &req_ctx, &site_label)
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
            req_ctx.jwt = conduit_auth_jwt::guard::extract_claims_from_session(
                session,
                jwt_auth_cfg.as_ref(),
            );
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
            chain = chain.push(LimitsGuard {
                cfg,
                ip_conn_counts: std::sync::Arc::clone(&self.state.ip_conn_counts),
                client_ip: guards.client_ip.clone(),
            });
        }

        // 5. Token-bucket rate limiting.
        if let Some(cfg) = guards.rate_limit_cfg {
            chain = chain.push(RateLimitGuard {
                cfg,
                site_label: guards.site_label.clone(),
                rate_limiter: std::sync::Arc::clone(&self.state.rate_limiter),
                #[cfg(feature = "redis")]
                redis_rate_limiter: self.state.redis_rate_limiter.clone(),
            });
        }

        // 6. Consumer model auth (identifies consumer, injects X-Consumer-ID).
        #[cfg(feature = "consumers")]
        if let Some(cfg) = guards.consumers_cfg {
            chain = chain.push(ConsumersGuard {
                cfg,
                path: guards.script_path.clone(),
                rate_limiter: std::sync::Arc::clone(&self.state.rate_limiter),
                #[cfg(feature = "redis")]
                redis_rate_limiter: self.state.redis_rate_limiter.clone(),
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
        req_ctx.limits.ip_conn_slot = Some(crate::filter::chain::IpConnSlotGuard {
            ip,
            counts: std::sync::Arc::clone(&self.state.ip_conn_counts),
        });
    }

    /// Per-route token-bucket rate limiting, applied after the site-level
    /// guard chain — once the route is known.
    ///
    /// Reads `req_ctx.proxy.route_rate_limit`, stamped at routing time by whichever
    /// matcher actually matched (`proxy` map or `routes[]` — see
    /// `router::RouteRateLimit`, issue #360). This replaced a second,
    /// post-routing path matcher (`router::find_route_rate_limit`, deleted)
    /// that only ever scanned `site.proxy` — a site with both `routes[]` and
    /// a `proxy` map could get the *non-selected* mechanism's rate limit
    /// applied to a `routes[]`-served request. `site_label` scopes the bucket
    /// key so the same route key on two different sites doesn't collide (see
    /// `rate_limit::route_key`).
    ///
    /// Returns `Ok(true)` when the request was rejected with 429 (response
    /// written, inflight counters decremented), `Ok(false)` to continue.
    async fn enforce_route_rate_limit(
        &self,
        session: &mut Session,
        req_ctx: &RequestCtx,
        site_label: &str,
    ) -> Result<bool> {
        let Some(route_rl) = req_ctx.proxy.route_rate_limit.as_ref() else {
            return Ok(false);
        };
        let rl_cfg = &route_rl.config;
        let route_key = route_rl.route_key.as_str();
        // Borrow the path directly from the session — only needed for the
        // `skipPaths` check below, no allocation on the rate-limit hot path.
        let path = session.req_header().uri.path();
        // #307: skipPaths is a documented, undisclaimed route-level field
        // (unlike dryRun/store, which the schema explicitly states are
        // site-level-only) — wire it up. Useful for a broad route pattern
        // that wants specific sub-paths exempted from its own rate limit.
        if rl_cfg
            .skip_paths
            .as_deref()
            .is_some_and(|sp| auth::is_path_skipped(Some(sp), path))
        {
            return Ok(false);
        }
        let client_key = rate_limit::extract_client_key(rl_cfg, session);
        // #322: route-level `store: "redis://..."` now actually routes
        // through Redis, mirroring the site-level check in
        // `filter::chain::rate_limit_allowed`. Scoped by
        // `redis_route_scope` (site+route, no client_key folded in — the
        // Redis client takes that as its own parameter) so two routes
        // (or the same route on two sites) never share a counter.
        #[cfg(feature = "redis")]
        if rl_cfg
            .store
            .as_deref()
            .is_some_and(conduit_config_core::scheme::is_redis_url)
        {
            if let Some(rrl) = &self.state.redis_rate_limiter {
                let scope = rate_limit::redis_route_scope(site_label, route_key);
                let allowed = rrl
                    .check(
                        &scope,
                        &client_key,
                        rl_cfg.limit,
                        rl_cfg.burst.unwrap_or(0),
                        rl_cfg.window_secs,
                    )
                    .await;
                return self
                    .finish_route_rate_limit(session, req_ctx, route_key, allowed)
                    .await;
            }
        }
        let key = rate_limit::route_key(site_label, route_key, &client_key);
        // Routed through the shared MAX_BUCKETS-capped admission point
        // (issue #305) instead of a hand-rolled, uncapped
        // entry()/or_insert_with() — this was a real DoS bypass on the
        // documented `keyBy: "header:X-Name"` usage pattern, since this map
        // is shared with the site-level limiter's own cap check.
        let allowed = conduit_ratelimit::check_key_for(&self.state.rate_limiter, &key, rl_cfg);
        self.finish_route_rate_limit(session, req_ctx, route_key, allowed)
            .await
    }

    /// Shared tail of [`Self::enforce_route_rate_limit`] for both the Redis
    /// and in-memory paths: on rejection, write 429 and unwind the inflight
    /// counters. Route-level `rateLimit` has no `dryRun` mode (the schema
    /// rejects the field there — it's a site/consumer-only option, see
    /// `docs/configuration.md`'s rate-limiting section), so unlike the
    /// site-level and per-consumer checks, there is no warn-and-continue
    /// branch here.
    async fn finish_route_rate_limit(
        &self,
        session: &mut Session,
        req_ctx: &RequestCtx,
        route_key: &str,
        allowed: bool,
    ) -> Result<bool> {
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
    /// are shed with 503.  Priority is read from `req_ctx.proxy.route_priority`,
    /// stamped at routing time by whichever matcher actually matched
    /// (`proxy` map or `routes[]` — see `router::route_limits_from_target`,
    /// issue #360) instead of being re-derived here via a second,
    /// post-routing path matcher that could disagree with routing.
    ///
    /// SECURITY: We intentionally do NOT trust the `X-Priority` header from
    /// downstream clients — an attacker could send `X-Priority: 100` to
    /// bypass load shedding entirely.  The header is stripped here to
    /// prevent it from leaking to the upstream as well.
    ///
    /// `site` is the route-resolved site from the request's config snapshot
    /// (passed in by `do_request_filter`) so this shares the routing snapshot
    /// — still needed here for `site.limits`.
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
        let route_priority = req_ctx.proxy.route_priority.unwrap_or(50);
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
}

// ── Trait-method bodies (called from thin delegators in `impl ProxyHttp`) ────

/// Body of [`pingora_proxy::ProxyHttp::request_filter`].
pub(crate) async fn request_filter(
    proxy: &ConduitProxy,
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
) -> Result<bool> {
    proxy.do_request_filter(session, ctx).await
}
