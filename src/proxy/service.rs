use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_cache::storage::Storage as CacheStorage;
use pingora_cache::{CacheKey, NoCacheReason, RespCacheable};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use prometheus::{CounterVec, HistogramVec};

use crate::config::schema::{
    ApiKeyConfig, AppConfig, BasicAuthConfig, ConnectionPoolConfig, CorsConfig, HealthCheckConfig,
    IpFilterConfig, LimitsConfig, MiddlewareEntry, ProxyTimeout, RateLimitConfig,
};
use crate::filter::rate_limit::RateLimiter;
use crate::filter::rate_limit_redis::RedisRateLimiter;
use crate::filter::{
    auth, compression, cors, ip_filter, limits, logging, rate_limit, redirects, response_time,
    script, security_headers,
};
use crate::handler::{
    acme_challenge as acme_handler, fallback, health, hot_reload as hot_reload_handler,
    metrics as metrics_handler, response, static_files,
};
use crate::proxy::cache as proxy_cache;
use crate::proxy::ctx::{AcceptEncoding, LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::{cache_disk, cache_redis};
use crate::proxy::{router, upstream};
use crate::util::log_writer::LogWriter;

// ── Prometheus metrics (registered once per process) ─────────────────────────

static METRICS: OnceLock<Arc<ConduitMetrics>> = OnceLock::new();

pub struct ConduitMetrics {
    pub requests_total: CounterVec,
    pub request_duration_seconds: HistogramVec,
    /// Incremented for every proxy response served from the cache (phase = Hit).
    pub cache_hits_total: CounterVec,
    /// Incremented for every proxy cache miss (phase = Miss or Expired).
    pub cache_misses_total: CounterVec,
}

impl ConduitMetrics {
    pub fn global() -> Arc<Self> {
        METRICS
            .get_or_init(|| {
                let requests_total = prometheus::register_counter_vec!(
                    "conduit_requests_total",
                    "Total number of HTTP requests handled",
                    &["method", "status"]
                )
                .expect("register conduit_requests_total");

                let request_duration_seconds = prometheus::register_histogram_vec!(
                    "conduit_request_duration_seconds",
                    "HTTP request duration in seconds",
                    &["method", "status"],
                    vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]
                )
                .expect("register conduit_request_duration_seconds");

                let cache_hits_total = prometheus::register_counter_vec!(
                    "conduit_cache_hits_total",
                    "Number of proxy responses served from the cache",
                    &["route"]
                )
                .expect("register conduit_cache_hits_total");

                let cache_misses_total = prometheus::register_counter_vec!(
                    "conduit_cache_misses_total",
                    "Number of proxy cache misses (upstream was contacted)",
                    &["route"]
                )
                .expect("register conduit_cache_misses_total");

                Arc::new(Self {
                    requests_total,
                    request_duration_seconds,
                    cache_hits_total,
                    cache_misses_total,
                })
            })
            .clone()
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

pub struct AppState {
    pub config: Arc<ArcSwap<AppConfig>>,
    pub inflight: Arc<AtomicUsize>,
    /// Per-route round-robin counters shared across all request threads.
    pub round_robin: Arc<DashMap<String, AtomicUsize>>,
    /// Token-bucket rate-limiter state, keyed by client IP or header value.
    pub rate_limiter: Arc<RateLimiter>,
    /// Prometheus metrics counters and histograms.
    pub metrics: Arc<ConduitMetrics>,
    /// Access-log writer — shared across all worker threads.
    pub log_writer: Arc<LogWriter>,
    /// Per-upstream health state and least-conn inflight counts.
    pub upstream_health: Arc<UpstreamRegistry>,
    /// Path to the config file — used by `POST /reload` to re-read and hot-swap.
    pub config_path: PathBuf,
    /// Active ACME HTTP-01 challenge tokens: `token → key_authorization`.
    ///
    /// Populated by the ACME flow during certificate procurement/renewal;
    /// served to the CA via the `/.well-known/acme-challenge/{token}` handler.
    pub acme_challenges: Arc<DashMap<String, String>>,
    /// Loopback address of the Axum upload server, or `None` when no site
    /// has an `upload` block.  Populated before Pingora starts so the router
    /// can forward matching requests without a config look-up.
    pub upload_addr: Option<SocketAddr>,
    /// Broadcast channel for hot-reload browser signals.
    ///
    /// When the file watcher detects a change in a watched directory, it sends
    /// `()` on this channel.  All active SSE connections (`/__hot-reload__`)
    /// subscribe and forward a `data: reload` event to the browser.
    pub hot_reload_tx: tokio::sync::broadcast::Sender<()>,
    /// Redis-backed rate limiter, instantiated when any site configures
    /// `rateLimit.store: "redis://..."`.  `None` when no site uses Redis
    /// rate limiting.
    pub redis_rate_limiter: Option<Arc<RedisRateLimiter>>,
}

impl AppState {
    pub fn new(config: AppConfig, config_path: PathBuf, upload_addr: Option<SocketAddr>) -> Self {
        Self::new_with_redis(config, config_path, upload_addr, None)
    }

    pub fn new_with_redis(
        config: AppConfig,
        config_path: PathBuf,
        upload_addr: Option<SocketAddr>,
        redis_rate_limiter: Option<Arc<RedisRateLimiter>>,
    ) -> Self {
        let (hot_reload_tx, _) = tokio::sync::broadcast::channel(16);
        Self {
            config: Arc::new(ArcSwap::new(Arc::new(config))),
            inflight: Arc::new(AtomicUsize::new(0)),
            round_robin: Arc::new(DashMap::new()),
            rate_limiter: Arc::new(DashMap::new()),
            metrics: ConduitMetrics::global(),
            log_writer: Arc::new(LogWriter::new()),
            upstream_health: Arc::new(UpstreamRegistry::new()),
            config_path,
            acme_challenges: Arc::new(DashMap::new()),
            upload_addr,
            hot_reload_tx,
            redis_rate_limiter,
        }
    }
}

// ── ConduitProxy ──────────────────────────────────────────────────────────────

pub struct ConduitProxy {
    pub state: Arc<AppState>,
}

impl ConduitProxy {
    async fn do_request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Option<RequestCtx>,
    ) -> Result<bool> {
        self.state.inflight.fetch_add(1, Ordering::Relaxed);

        // ── Gather request metadata before borrowing the config ───────────────
        let request_origin = cors::request_origin(session);
        let is_cors_preflight = cors::is_preflight(session);

        // ── Load config once — extract all per-site filters ───────────────────
        let (
            mut req_ctx,
            ip_cfg,
            limits_cfg,
            rate_limit_cfg,
            basic_auth_cfg,
            api_key_cfg,
            cors_cfg,
            security_cfg,
            redirect_result,
            custom_headers,
            middleware,
            script_method,
            script_path_str,
            script_query,
            script_headers,
        ) = {
            let config = self.state.config.load();
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

            // Collect request headers for Rhai scripts (lower-cased keys).
            let req_headers_for_script: std::collections::HashMap<String, String> = session
                .req_header()
                .headers
                .iter()
                .filter_map(|(k, v)| {
                    v.to_str()
                        .ok()
                        .map(|vs| (k.as_str().to_ascii_lowercase(), vs.to_owned()))
                })
                .collect();

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

            (
                req_ctx,
                ip_cfg,
                limits_cfg,
                rate_limit_cfg,
                basic_auth_cfg,
                api_key_cfg,
                cors_cfg,
                security_cfg,
                redirect_result,
                custom_headers,
                middleware,
                method,
                path,
                query.unwrap_or_default(),
                req_headers_for_script,
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
        let guards = GuardCtx {
            ip_cfg,
            limits_cfg,
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
        };
        if self.run_guard_filters(session, guards).await? {
            return Ok(true);
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
    async fn run_guard_filters(&self, session: &mut Session, guards: GuardCtx) -> Result<bool> {
        // 1. IP filter — applied to every request, including health and metrics.
        if let Some(ref cfg) = guards.ip_cfg {
            if !ip_filter::is_allowed(cfg, session) {
                response::write_response(
                    session,
                    403,
                    "text/plain",
                    Bytes::from_static(b"Forbidden"),
                    &guards.extra_headers,
                )
                .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        // 2. CORS preflight — before auth; browsers send OPTIONS without credentials.
        if guards.is_preflight {
            if let Some(ref cfg) = guards.cors_cfg {
                let origin = guards.origin.as_deref().unwrap_or("");
                cors::handle_preflight(session, cfg, origin, &guards.sec_only).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        // Health, ACME challenge, and hot-reload endpoints bypass all remaining filters.
        if matches!(
            guards.handler_kind,
            HandlerKind::Health
                | HandlerKind::AcmeChallenge
                | HandlerKind::HotReloadSse
                | HandlerKind::HotReloadJs
        ) {
            return Ok(false);
        }

        // 3–5. Size limits, rate limiting, and authentication.
        if self
            .check_non_health_guards(
                session,
                &guards.limits_cfg,
                &guards.rate_limit_cfg,
                &guards.basic_auth_cfg,
                &guards.api_key_cfg,
                &guards.extra_headers,
            )
            .await?
        {
            return Ok(true);
        }

        // 6. Redirects.
        if let Some((location, status)) = guards.redirect_result {
            response::write_redirect(session, status, &location, &guards.extra_headers).await?;
            self.state.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(true);
        }

        // 7. Rhai script middleware — type: "script" entries from site.middleware.
        for entry in &guards.middleware {
            if entry.r#type != "script" {
                continue;
            }
            let Some(ref script_path) = entry.path else {
                continue;
            };
            match script::run_script(
                script_path,
                &guards.script_path,
                &guards.script_method,
                &guards.script_query,
                guards.script_headers.clone(),
            ) {
                script::ScriptOutcome::Continue => {}
                script::ScriptOutcome::Abort {
                    status,
                    body,
                    extra_headers,
                } => {
                    // Merge the script's extra headers with the standard extra
                    // headers (CORS, security, custom) so both sets are sent.
                    let mut all_headers = guards.extra_headers.clone();
                    all_headers.extend(extra_headers);
                    response::write_response(
                        session,
                        status,
                        "text/plain",
                        Bytes::from(body),
                        &all_headers,
                    )
                    .await?;
                    self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Check size limits, rate limiting, and authentication for non-health requests.
    ///
    /// Returns `Ok(true)` when the request was rejected (response written,
    /// inflight decremented).
    async fn check_non_health_guards(
        &self,
        session: &mut Session,
        limits_cfg: &Option<LimitsConfig>,
        rate_limit_cfg: &Option<RateLimitConfig>,
        basic_auth_cfg: &Option<BasicAuthConfig>,
        api_key_cfg: &Option<ApiKeyConfig>,
        extra_headers: &[(String, String)],
    ) -> Result<bool> {
        // 3. Request size / header limits.
        if let Some(ref cfg) = limits_cfg {
            if let Some((status, body)) = limits_rejection(limits::check(cfg, session)) {
                response::write_response(session, status, "text/plain", body, extra_headers)
                    .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        // 4. Token-bucket rate limiting.
        if let Some(ref rl_cfg) = rate_limit_cfg {
            // Determine whether to use Redis or the in-memory limiter.
            let allowed = match rl_cfg
                .store
                .as_deref()
                .filter(|s| s.starts_with("redis://"))
            {
                Some(_) => {
                    // Use Redis if the rate limiter was initialised at startup.
                    // Fall through to memory if it is absent (startup failure).
                    if let Some(ref rrl) = self.state.redis_rate_limiter {
                        let key = rate_limit::extract_client_key(rl_cfg, session);
                        rrl.check(&key, rl_cfg.limit, rl_cfg.window_secs).await
                    } else {
                        // Redis connection failed at startup — use memory.
                        rate_limit::check(rl_cfg, session, &self.state.rate_limiter)
                    }
                }
                None => rate_limit::check(rl_cfg, session, &self.state.rate_limiter),
            };
            if !allowed {
                response::write_response(
                    session,
                    429,
                    "text/plain",
                    Bytes::from_static(b"Too Many Requests"),
                    extra_headers,
                )
                .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        // 5a. Basic Auth.
        if let Some(ref auth_cfg) = basic_auth_cfg {
            if let auth::BasicAuthResult::Denied { challenge, realm } =
                auth::check_basic_auth(auth_cfg, session)
            {
                let www_auth = challenge.then(|| format!("Basic realm=\"{realm}\""));
                response::write_denied(session, www_auth.as_deref(), extra_headers).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        // 5b. API key auth.
        if let Some(ref key_cfg) = api_key_cfg {
            if !auth::check_api_key(key_cfg, session) {
                response::write_denied(session, None, extra_headers).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Dispatch a request to the appropriate local handler.
    ///
    /// Returns `Ok(true)` for local handlers (response fully written) or
    /// `Ok(false)` for proxy/upload targets (Pingora continues the pipeline).
    async fn dispatch_local(
        &self,
        session: &mut Session,
        ctx: &mut Option<RequestCtx>,
        handler_kind: HandlerKind,
    ) -> Result<bool> {
        // Append X-Response-Time to extra_headers if configured for this site.
        // Done once here so all local handler arms automatically include it.
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

        match handler_kind {
            HandlerKind::AcmeChallenge => {
                let token = if let Some(RequestCtx {
                    upstream: UpstreamTarget::Local(LocalHandler::AcmeChallenge { token }),
                    ..
                }) = ctx.as_ref()
                {
                    token.clone()
                } else {
                    unreachable!()
                };
                acme_handler::handle_acme_challenge(session, &token, &self.state.acme_challenges)
                    .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::Health => {
                let (extra, upstream_pairs) = {
                    let req_ctx = ctx.as_ref().unwrap();
                    let extra = req_ctx.extra_headers.clone();

                    // Collect upstream statuses when healthCheck.includeUpstreams is set.
                    let upstream_pairs: Vec<(String, bool)> = {
                        let config = self.state.config.load();
                        let site = config.sites.get(req_ctx.site_idx);
                        let include = site
                            .and_then(|s| s.health_check.as_ref())
                            .and_then(|hc| match hc {
                                HealthCheckConfig::Options(opts) => opts.include_upstreams,
                                _ => None,
                            })
                            .unwrap_or(false);

                        if include {
                            use crate::proxy::upstream as us;
                            if let Some(s) = site {
                                let mut urls: Vec<String> = Vec::new();
                                // Top-level proxy field.
                                if let Some(proxy) = &s.proxy {
                                    urls.extend(us::target_urls_from_proxy(proxy));
                                }
                                // routes array (Phase 3.6).
                                if let Some(routes) = &s.routes {
                                    for rc in routes {
                                        if let Some(rt) = &rc.proxy {
                                            urls.extend(us::target_urls(rt));
                                        }
                                    }
                                }
                                urls.into_iter()
                                    .map(|url| {
                                        let healthy = self.state.upstream_health.is_healthy(&url);
                                        (url, healthy)
                                    })
                                    .collect()
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    };
                    (extra, upstream_pairs)
                };

                let pairs_ref: Vec<(&str, bool)> = upstream_pairs
                    .iter()
                    .map(|(u, h)| (u.as_str(), *h))
                    .collect();
                health::handle_health(session, &pairs_ref, &extra).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::Metrics => {
                let (token, extra) = if let Some(RequestCtx {
                    upstream: UpstreamTarget::Local(LocalHandler::Metrics { token }),
                    extra_headers,
                    ..
                }) = ctx.as_ref()
                {
                    (
                        token.as_deref().map(str::to_owned),
                        extra_headers.as_slice().to_vec(),
                    )
                } else {
                    unreachable!()
                };
                metrics_handler::handle_metrics(session, token.as_deref(), &extra).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::StaticFile => {
                // Load compression options for this site before pattern-matching ctx.
                let compress_opts = {
                    let config = self.state.config.load();
                    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                    config
                        .sites
                        .get(site_idx)
                        .and_then(|s| s.compression.as_ref())
                        .and_then(compression::effective)
                };
                let accept_enc = ctx
                    .as_ref()
                    .map(|c| c.accept_enc.clone())
                    .unwrap_or_default();

                let (roots, options, strip_prefix, extra) = if let Some(RequestCtx {
                    upstream:
                        UpstreamTarget::Local(LocalHandler::StaticFile {
                            roots,
                            options,
                            strip_prefix,
                        }),
                    extra_headers,
                    ..
                }) = ctx.as_ref()
                {
                    (
                        roots.clone(),
                        options.clone(),
                        strip_prefix.clone(),
                        extra_headers.clone(),
                    )
                } else {
                    unreachable!()
                };
                let found = static_files::handle_static(
                    session,
                    &roots,
                    &options,
                    strip_prefix.as_deref(),
                    &extra,
                    compress_opts.as_ref(),
                    &accept_enc,
                )
                .await?;

                if !found {
                    // File not found — delegate to the site's fallback handler
                    // (e.g. SPA index.html for HTML requests, JSON 404 for API).
                    let config = self.state.config.load();
                    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                    let site = config.sites.get(site_idx);
                    fallback::handle_fallback(session, site, &extra).await?;
                }

                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::Fallback => {
                let config = self.state.config.load();
                let (site_idx, extra) = ctx
                    .as_ref()
                    .map(|c| (c.site_idx, c.extra_headers.clone()))
                    .unwrap_or((0, vec![]));
                let site = config.sites.get(site_idx);
                fallback::handle_fallback(session, site, &extra).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::HotReloadJs => {
                let extra = ctx
                    .as_ref()
                    .map(|c| c.extra_headers.clone())
                    .unwrap_or_default();
                hot_reload_handler::handle_client_js(session, &extra).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::HotReloadSse => {
                let extra = ctx
                    .as_ref()
                    .map(|c| c.extra_headers.clone())
                    .unwrap_or_default();
                let rx = self.state.hot_reload_tx.subscribe();
                // inflight is decremented after the SSE stream ends (inside handle_sse).
                hot_reload_handler::handle_sse(session, rx, &extra).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::Proxy => Ok(false),
        }
    }
}

// ── ProxyHttp trait implementation ────────────────────────────────────────────

#[async_trait]
impl ProxyHttp for ConduitProxy {
    type CTX = Option<RequestCtx>;

    fn new_ctx(&self) -> Self::CTX {
        None
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        self.do_request_filter(session, ctx).await
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>>
    where
        Self::CTX: Send + Sync,
    {
        let req_ctx = ctx.as_mut().expect("ctx set in request_filter");

        if let Some(ref retry) = req_ctx.retry {
            apply_backoff(retry).await;
        }

        let (addr_str, tls, sni) = resolve_peer_addr(req_ctx)?;

        let socket_addr: SocketAddr = addr_str.parse().map_err(|_| {
            pingora_core::Error::explain(
                pingora_core::ErrorType::ConnectProxyFailure,
                format!("invalid upstream address: {addr_str}"),
            )
        })?;
        let mut peer = HttpPeer::new(socket_addr, tls, sni);

        // Negotiate HTTP/2 with the upstream when the route sets `http2: true`.
        if req_ctx.proxy_http2 {
            peer.options.alpn = pingora_core::upstreams::peer::ALPN::H2H1;
        }

        // Derive fallback timeout from `limits.timeoutSecs` on the matched site.
        let limits_timeout_secs = {
            let cfg = self.state.config.load();
            cfg.sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.timeout_secs)
        };

        apply_peer_options(
            &mut peer,
            req_ctx.proxy_timeout.as_ref(),
            req_ctx.proxy_pool.as_ref(),
            limits_timeout_secs,
        );

        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        append_forwarded_headers(session, upstream_request, &self.state, ctx)?;
        apply_upstream_path_transforms(upstream_request, ctx)?;
        Ok(())
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        if let Some(req_ctx) = ctx.as_ref() {
            // Inject CORS + security headers into proxy responses.
            for (name, value) in &req_ctx.extra_headers {
                upstream_response.insert_header(name.clone(), value.clone())?;
            }

            // X-Response-Time for proxy responses.
            {
                let config = self.state.config.load();
                let site = config.sites.get(req_ctx.site_idx);
                let rt_cfg = site.and_then(|s| s.response_time.as_ref());
                if response_time::is_enabled(rt_cfg) {
                    let digits = response_time::decimal_digits(rt_cfg);
                    let elapsed = req_ctx.start_time.elapsed();
                    let value = response_time::format_elapsed(elapsed, digits);
                    upstream_response.insert_header("x-response-time", value)?;
                }
            }

            // 5xx retry logic.
            if let Some(retry) = &req_ctx.retry {
                let status = upstream_response.status.as_u16();
                if status >= 500 && retry.has_attempts_left() && retry.has_condition("5xx") {
                    return Err(pingora_core::Error::explain(
                        pingora_core::ErrorType::Custom("5xx_retry"),
                        format!("upstream returned HTTP {status}; will retry"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Enable the cache for proxy routes that carry a `cache` configuration.
    ///
    /// Called by Pingora after `request_filter`; only reached for upstream-bound
    /// requests (local handlers return `Ok(true)` in `request_filter`).
    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()>
    where
        Self::CTX: Send + Sync,
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
        } else if let Some(url) = cfg
            .store
            .strip_prefix("redis://")
            .map(|_| cfg.store.as_str())
        {
            cache_redis::get_or_create(url)
        } else if let Some(dir) = cfg.store.strip_prefix("disk:") {
            cache_disk::get_or_create(dir)
        } else {
            tracing::warn!(
                store = %cfg.store,
                "unsupported cache store — caching disabled for this route"
            );
            return Ok(());
        };

        // Check request-side policy (method, cookies, skip-paths).
        let method = session.req_header().method.as_str();
        let path = session.req_header().uri.path();
        let has_cookie = session.req_header().headers.contains_key("cookie");

        if !proxy_cache::should_cache_request(cfg, method, has_cookie, path) {
            return Ok(());
        }

        session.cache.enable(storage, None, None, None, None);
        Ok(())
    }

    /// Build a deterministic cache key: namespace = Host header, primary = scheme:path[?query].
    fn cache_key_callback(&self, session: &Session, ctx: &mut Self::CTX) -> Result<CacheKey>
    where
        Self::CTX: Send + Sync,
    {
        // Use extract_host() so the port suffix is stripped (e.g. "example.com:8080" → "example.com").
        // This keeps the cache key consistent with how the router matches virtual hosts.
        let host_str = extract_host(session);
        let host = host_str.as_str();

        // Derive scheme from whether the matched site has TLS configured.
        let scheme = {
            let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
            let config = self.state.config.load();
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
            let config = self.state.config.load();
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

    /// Decide whether an upstream response is cacheable.
    ///
    /// Returns [`RespCacheable::Cacheable`] for `200 OK` responses when the
    /// route has a non-zero `ttl_secs`.  Everything else is uncacheable.
    fn response_cache_filter(
        &self,
        _session: &Session,
        resp: &ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<RespCacheable>
    where
        Self::CTX: Send + Sync,
    {
        let cacheable = ctx
            .as_ref()
            .and_then(|c| c.proxy_cache_cfg.as_ref())
            .map(|cfg| proxy_cache::response_cacheable(cfg, resp))
            .unwrap_or(RespCacheable::Uncacheable(NoCacheReason::Custom(
                "no-cache-cfg",
            )));
        Ok(cacheable)
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut e: Box<pingora_core::Error>,
    ) -> Box<pingora_core::Error> {
        use pingora_core::ErrorType::*;

        if let Some(req_ctx) = ctx.as_ref() {
            if let Some(retry) = &req_ctx.retry {
                if retry.has_attempts_left() {
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
                    if (is_conn_err && retry.has_condition("connection_error"))
                        || (is_timeout && retry.has_condition("timeout"))
                    {
                        e.set_retry(true);
                    }
                }
            }
        }
        e
    }

    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<pingora_core::Error>,
        ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<pingora_core::Error> {
        use pingora_core::ErrorType::*;

        let mut e = e.more_context(format!("Peer: {peer}"));
        e.retry
            .decide_reuse(client_reused && !session.as_ref().retry_buffer_truncated());

        if let Some(req_ctx) = ctx.as_ref() {
            if let Some(retry) = &req_ctx.retry {
                if retry.has_attempts_left() {
                    let is_timeout = matches!(e.etype(), ReadTimedout | WriteTimedout);
                    let is_5xx_retry = matches!(e.etype(), Custom("5xx_retry"));
                    if (is_timeout && retry.has_condition("timeout"))
                        || (is_5xx_retry && retry.has_condition("5xx"))
                    {
                        e.set_retry(true);
                    }
                }
            }
        }
        e
    }

    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        // Decrement inflight for proxy requests (local handlers decrement inline).
        if let Some(req_ctx) = ctx.as_ref() {
            if !matches!(req_ctx.upstream, UpstreamTarget::Local(_)) {
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                // For least-conn routes, release the per-upstream slot.
                if let Some(ref url) = req_ctx.proxy_upstream_url {
                    self.state.upstream_health.conn_dec(url);
                }
            }
        }

        // Write access log entry.
        let start_time = ctx
            .as_ref()
            .map(|c| c.start_time)
            .unwrap_or_else(std::time::Instant::now);
        {
            let config = self.state.config.load();
            let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
            let site = config.sites.get(site_idx);
            logging::write_access_log(session, start_time, site, &self.state.log_writer);
        }

        // Record Prometheus metrics.
        let method = session.req_header().method.as_str().to_owned();
        let status = session
            .response_written()
            .map(|h| h.status.as_u16().to_string())
            .unwrap_or_else(|| "0".to_owned());
        let elapsed = ctx
            .as_ref()
            .map(|c| c.start_time.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        self.state
            .metrics
            .requests_total
            .with_label_values(&[&method, &status])
            .inc();
        self.state
            .metrics
            .request_duration_seconds
            .with_label_values(&[&method, &status])
            .observe(elapsed);

        // Cache hit / miss counters (only for proxy requests with caching enabled).
        if ctx
            .as_ref()
            .and_then(|c| c.proxy_cache_cfg.as_ref())
            .is_some()
        {
            use pingora_cache::CachePhase;
            let route = session.req_header().uri.path().to_owned();
            match session.cache.phase() {
                CachePhase::Hit => {
                    self.state
                        .metrics
                        .cache_hits_total
                        .with_label_values(&[&route])
                        .inc();
                }
                CachePhase::Miss | CachePhase::Expired => {
                    self.state
                        .metrics
                        .cache_misses_total
                        .with_label_values(&[&route])
                        .inc();
                }
                _ => {}
            }
        }
    }
}

// ── upstream_peer helpers ─────────────────────────────────────────────────────

/// Sleep for the configured backoff duration when this is a retry attempt (not the first try).
async fn apply_backoff(retry: &RetryState) {
    if retry.attempt > 0 {
        if let Some(ms) = retry.backoff_ms {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }
}

/// Resolve the upstream `(addr, tls, sni)` from the request context.
///
/// On a retry the address rotates through the URL list and the attempt counter
/// is incremented.  On the first attempt the values come from `ctx.upstream`.
fn resolve_peer_addr(req_ctx: &mut RequestCtx) -> pingora_core::Result<(String, bool, String)> {
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

/// Apply per-route timeout, connection-pool settings, and global limits to an
/// `HttpPeer`.
///
/// Priority (highest → lowest):
/// 1. `proxy.*.timeout.*` — per-route fine-grained timeouts
/// 2. `limits.timeoutSecs` — site-wide fallback timeout
///
/// `limits.timeout_secs` is applied to all three timeout fields only when
/// the corresponding per-route field is absent.
fn apply_peer_options(
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

    if let Some(p) = pool {
        if let Some(secs) = p.idle_timeout_secs {
            peer.options.idle_timeout = Some(Duration::from_secs(secs));
        }
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
}

/// All per-request guard data bundled into one value to keep `run_guard_filters`
/// within clippy's argument-count limit (7).
struct GuardCtx {
    ip_cfg: Option<IpFilterConfig>,
    limits_cfg: Option<LimitsConfig>,
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
    /// Request info forwarded to Rhai scripts (method, path, query, headers).
    script_method: String,
    script_path: String,
    script_query: String,
    script_headers: std::collections::HashMap<String, String>,
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
        UpstreamTarget::Local(_) => HandlerKind::Fallback,
        _ => HandlerKind::Proxy,
    }
}

/// Map a `limits::CheckResult` to the HTTP rejection status + body, or `None`
/// when the request is within the configured limits.
fn limits_rejection(result: limits::CheckResult) -> Option<(u16, Bytes)> {
    match result {
        limits::CheckResult::BodyTooLarge => {
            Some((413, Bytes::from_static(b"Request Entity Too Large")))
        }
        limits::CheckResult::HeaderTooLarge => {
            Some((431, Bytes::from_static(b"Request Header Fields Too Large")))
        }
        limits::CheckResult::Ok => None,
    }
}

fn extract_host(session: &Session) -> String {
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_owned())
        .unwrap_or_default()
}

/// Append `X-Forwarded-For` and `X-Forwarded-Proto` headers to the upstream request.
fn append_forwarded_headers(
    session: &Session,
    upstream_request: &mut RequestHeader,
    state: &AppState,
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
    Ok(())
}

/// Apply strip-prefix and path-rewrite transforms for proxy and upload targets.
fn apply_upstream_path_transforms(
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
            let mut path = upstream_request.uri.path().to_owned();

            // 1. Strip prefix.
            if let Some(pfx) = strip_prefix {
                let stripped = path.strip_prefix(pfx.as_str()).unwrap_or("/");
                path = if stripped.is_empty() {
                    "/".to_owned()
                } else {
                    stripped.to_owned()
                };
            }

            // 2. Apply rewrite rules (first match wins).
            if let Some(rules) = rewrite {
                for rule in rules {
                    match get_rewrite_regex(&rule.from) {
                        Some(re) if re.is_match(&path) => {
                            path = re.replacen(&path, 1, rule.to.as_str()).into_owned();
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
            }

            if path != upstream_request.uri.path() {
                let new_uri = rebuild_uri(&upstream_request.uri, &path)?;
                upstream_request.set_uri(new_uri);
            }
        }
        UpstreamTarget::Upload { .. } => {
            upstream_request.insert_header("x-conduit-site-idx", ctx_ref.site_idx.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

/// Return a compiled [`regex::Regex`] for `pattern`, using a process-wide cache
/// to avoid recompiling the same pattern on every request.
///
/// Rewrite patterns are plain (un-anchored) regexes so that `replacen` can
/// match anywhere in the path.  Invalid patterns are not stored; the caller
/// should log the error and skip the rule.
fn get_rewrite_regex(pattern: &str) -> Option<regex::Regex> {
    static CACHE: OnceLock<DashMap<String, regex::Regex>> = OnceLock::new();
    let cache = CACHE.get_or_init(DashMap::new);
    if let Some(re) = cache.get(pattern) {
        return Some(re.clone());
    }
    let re = regex::Regex::new(pattern).ok()?;
    cache.insert(pattern.to_owned(), re.clone());
    Some(re)
}

fn rebuild_uri(original: &http::Uri, new_path: &str) -> Result<http::Uri> {
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
