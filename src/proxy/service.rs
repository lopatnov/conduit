use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_cache::{CacheKey, NoCacheReason, RespCacheable};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use prometheus::{CounterVec, HistogramVec};

use crate::config::schema::{
    ApiKeyConfig, AppConfig, BasicAuthConfig, ConnectionPoolConfig, CorsConfig, HealthCheckConfig,
    IpFilterConfig, LimitsConfig, ProxyTimeout, RateLimitConfig,
};
use crate::filter::rate_limit::RateLimiter;
use crate::filter::{
    auth, compression, cors, ip_filter, limits, logging, rate_limit, redirects, response_time,
    security_headers,
};
use crate::handler::{
    acme_challenge as acme_handler, fallback, health, metrics as metrics_handler, response,
    static_files,
};
use crate::proxy::cache as proxy_cache;
use crate::proxy::ctx::{AcceptEncoding, LocalHandler, RequestCtx, RetryState, UpstreamTarget};
use crate::proxy::health::UpstreamRegistry;
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
}

impl AppState {
    pub fn new(config: AppConfig, config_path: PathBuf, upload_addr: Option<SocketAddr>) -> Self {
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

            let req_ctx = router::route_request(
                &config,
                &host,
                &path,
                &client_ip,
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

        // ── Guard filters (ip, cors, limits, auth, redirects) ─────────────────
        let guards = GuardCtx {
            ip_cfg,
            limits_cfg,
            rate_limit_cfg,
            basic_auth_cfg,
            api_key_cfg,
            cors_cfg,
            redirect_result,
            handler_kind: handler_kind.clone(),
            is_preflight: is_cors_preflight,
            sec_only,
            origin: request_origin,
            extra_headers: req_ctx.extra_headers.clone(),
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

        // Health and ACME challenge endpoints bypass all remaining filters.
        if matches!(
            guards.handler_kind,
            HandlerKind::Health | HandlerKind::AcmeChallenge
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
            if !rate_limit::check(rl_cfg, session, &self.state.rate_limiter) {
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
                            site.and_then(|s| s.proxy.as_ref())
                                .map(|proxy| {
                                    us::target_urls_from_proxy(proxy)
                                        .into_iter()
                                        .map(|url| {
                                            let healthy =
                                                self.state.upstream_health.is_healthy(&url);
                                            (url, healthy)
                                        })
                                        .collect()
                                })
                                .unwrap_or_default()
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
                static_files::handle_static(
                    session,
                    &roots,
                    &options,
                    strip_prefix.as_deref(),
                    &extra,
                    compress_opts.as_ref(),
                    &accept_enc,
                )
                .await?;
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

        apply_peer_options(
            &mut peer,
            req_ctx.proxy_timeout.as_ref(),
            req_ctx.proxy_pool.as_ref(),
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
        let client_ip = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip().to_string());

        if let Some(ip) = client_ip {
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

        let proto = {
            // Determine the downstream protocol from the site's TLS configuration.
            let config = self.state.config.load();
            let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
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
        upstream_request.insert_header("x-forwarded-proto", proto)?;

        if let Some(ctx_ref) = ctx.as_ref() {
            match &ctx_ref.upstream {
                UpstreamTarget::Proxy {
                    strip_prefix: Some(pfx),
                    ..
                } => {
                    let old_path = upstream_request.uri.path().to_owned();
                    let new_path = old_path.strip_prefix(pfx.as_str()).unwrap_or("/");
                    let new_path = if new_path.is_empty() { "/" } else { new_path };
                    if new_path != old_path {
                        let new_uri = rebuild_uri(&upstream_request.uri, new_path)?;
                        upstream_request.set_uri(new_uri);
                    }
                }
                UpstreamTarget::Upload { .. } => {
                    // Tell the upload server which site's config to apply.
                    upstream_request
                        .insert_header("x-conduit-site-idx", ctx_ref.site_idx.to_string())?;
                }
                _ => {}
            }
        }

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

        // Only "memory" store is supported in Phase 2.6.
        if cfg.store != "memory" {
            tracing::warn!(
                store = %cfg.store,
                "unsupported cache store — caching disabled for this route"
            );
            return Ok(());
        }

        // Check request-side policy (method, cookies, skip-paths).
        let method = session.req_header().method.as_str();
        let path = session.req_header().uri.path();
        let has_cookie = session.req_header().headers.contains_key("cookie");

        if !proxy_cache::should_cache_request(cfg, method, has_cookie, path) {
            return Ok(());
        }

        session
            .cache
            .enable(proxy_cache::cache_storage(), None, None, None, None);
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

        Ok(proxy_cache::build_cache_key(host, scheme, path, query))
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

/// Apply per-route timeout and connection-pool settings to an `HttpPeer`.
fn apply_peer_options(
    peer: &mut HttpPeer,
    timeout: Option<&ProxyTimeout>,
    pool: Option<&ConnectionPoolConfig>,
) {
    if let Some(t) = timeout {
        if let Some(ms) = t.connect_ms {
            peer.options.connection_timeout = Some(Duration::from_millis(ms));
        }
        if let Some(ms) = t.read_ms {
            peer.options.read_timeout = Some(Duration::from_millis(ms));
        }
        if let Some(ms) = t.send_ms {
            peer.options.write_timeout = Some(Duration::from_millis(ms));
        }
    }
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
    handler_kind: HandlerKind,
    is_preflight: bool,
    sec_only: Vec<(String, String)>,
    origin: Option<String>,
    extra_headers: Vec<(String, String)>,
}

/// Classify a request's upstream target into a `HandlerKind` for filter routing.
fn handler_kind_of(upstream: &UpstreamTarget) -> HandlerKind {
    match upstream {
        UpstreamTarget::Local(LocalHandler::Health) => HandlerKind::Health,
        UpstreamTarget::Local(LocalHandler::AcmeChallenge { .. }) => HandlerKind::AcmeChallenge,
        UpstreamTarget::Local(LocalHandler::Metrics { .. }) => HandlerKind::Metrics,
        UpstreamTarget::Local(LocalHandler::StaticFile { .. }) => HandlerKind::StaticFile,
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
