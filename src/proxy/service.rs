use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use prometheus::{CounterVec, HistogramVec};

use crate::config::schema::AppConfig;
use crate::filter::rate_limit::RateLimiter;
use crate::filter::{auth, cors, ip_filter, limits, rate_limit, redirects, security_headers};
use crate::handler::{fallback, health, metrics as metrics_handler, response, static_files};
use crate::proxy::ctx::{LocalHandler, RequestCtx, UpstreamTarget};
use crate::proxy::{router, upstream};

// ── Prometheus metrics (registered once per process) ─────────────────────────

static METRICS: OnceLock<Arc<ConduitMetrics>> = OnceLock::new();

pub struct ConduitMetrics {
    pub requests_total: CounterVec,
    pub request_duration_seconds: HistogramVec,
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

                Arc::new(Self {
                    requests_total,
                    request_duration_seconds,
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
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config: Arc::new(ArcSwap::new(Arc::new(config))),
            inflight: Arc::new(AtomicUsize::new(0)),
            round_robin: Arc::new(DashMap::new()),
            rate_limiter: Arc::new(DashMap::new()),
            metrics: ConduitMetrics::global(),
        }
    }
}

// ── ConduitProxy ──────────────────────────────────────────────────────────────

pub struct ConduitProxy {
    pub state: Arc<AppState>,
}

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

            let req_ctx = router::route_request(&config, &host, &path, &self.state.round_robin);
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
            )
        };

        // Compute security headers once; reused both in the full extra_headers set
        // and in the preflight path that injects security headers without CORS headers.
        let sec_only: Vec<(String, String)> = security_cfg
            .as_ref()
            .map(security_headers::header_entries)
            .unwrap_or_default();

        // ── Build planned response headers (CORS + security) ──────────────────
        // These are injected into every response written for this request.
        {
            let cors_hdrs = cors_cfg
                .as_ref()
                .map(|c| cors::response_headers(c, request_origin.as_deref()))
                .unwrap_or_default();
            req_ctx.extra_headers = cors_hdrs
                .into_iter()
                .chain(sec_only.iter().cloned())
                .collect();
        }

        // Determine handler kind so we can skip filters for the health endpoint.
        let handler_kind = match &req_ctx.upstream {
            UpstreamTarget::Local(LocalHandler::Health) => HandlerKind::Health,
            UpstreamTarget::Local(LocalHandler::Metrics { .. }) => HandlerKind::Metrics,
            UpstreamTarget::Local(LocalHandler::StaticFile { .. }) => HandlerKind::StaticFile,
            UpstreamTarget::Local(_) => HandlerKind::Fallback,
            _ => HandlerKind::Proxy,
        };

        // ── 1. IP filter (all requests, including health) ─────────────────────
        if let Some(ref ip_cfg) = ip_cfg {
            if !ip_filter::is_allowed(ip_cfg, session) {
                response::write_response(
                    session,
                    403,
                    "text/plain",
                    Bytes::from_static(b"Forbidden"),
                    &req_ctx.extra_headers,
                )
                .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        // ── 2. CORS preflight ─────────────────────────────────────────────────
        // Handled after ip_filter but before any auth — OPTIONS preflights must
        // not be blocked by authentication (browsers send them without credentials).
        if is_cors_preflight {
            if let Some(ref cfg) = cors_cfg {
                cors::handle_preflight(
                    session,
                    cfg,
                    request_origin.as_deref().unwrap_or(""),
                    &sec_only,
                )
                .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        // ── 3–5. Limits, rate-limit, and auth are skipped for health ──────────
        if !matches!(handler_kind, HandlerKind::Health) {
            // 3. Request size / header limits.
            if let Some(ref limits_cfg) = limits_cfg {
                match limits::check(limits_cfg, session) {
                    limits::CheckResult::BodyTooLarge => {
                        response::write_response(
                            session,
                            413,
                            "text/plain",
                            Bytes::from_static(b"Request Entity Too Large"),
                            &req_ctx.extra_headers,
                        )
                        .await?;
                        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                        return Ok(true);
                    }
                    limits::CheckResult::HeaderTooLarge => {
                        response::write_response(
                            session,
                            431,
                            "text/plain",
                            Bytes::from_static(b"Request Header Fields Too Large"),
                            &req_ctx.extra_headers,
                        )
                        .await?;
                        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                        return Ok(true);
                    }
                    limits::CheckResult::Ok => {}
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
                        &req_ctx.extra_headers,
                    )
                    .await?;
                    self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                    return Ok(true);
                }
            }

            // 5a. Basic Auth.
            if let Some(ref auth_cfg) = basic_auth_cfg {
                match auth::check_basic_auth(auth_cfg, session) {
                    auth::BasicAuthResult::Allowed => {}
                    auth::BasicAuthResult::Denied { challenge, realm } => {
                        let www_auth = if challenge {
                            Some(format!("Basic realm=\"{realm}\""))
                        } else {
                            None
                        };
                        response::write_denied(
                            session,
                            www_auth.as_deref(),
                            &req_ctx.extra_headers,
                        )
                        .await?;
                        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                        return Ok(true);
                    }
                }
            }

            // 5b. API key auth.
            if let Some(ref key_cfg) = api_key_cfg {
                if !auth::check_api_key(key_cfg, session) {
                    response::write_denied(session, None, &req_ctx.extra_headers).await?;
                    self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                    return Ok(true);
                }
            }
        }

        // ── 6. Redirects ──────────────────────────────────────────────────────
        if let Some((location, status)) = redirect_result {
            response::write_redirect(session, status, &location, &req_ctx.extra_headers).await?;
            self.state.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(true);
        }

        // ── 7. Dispatch ───────────────────────────────────────────────────────
        *ctx = Some(req_ctx);

        match handler_kind {
            HandlerKind::Health => {
                let extra = ctx.as_ref().unwrap().extra_headers.as_slice();
                health::handle_health(session, extra).await?;
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
            if retry.attempt > 0 {
                if let Some(ms) = retry.backoff_ms {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
            }
        }

        let (addr_str, tls, sni) = if let Some(ref mut retry) = req_ctx.retry {
            let url = &retry.urls[retry.attempt % retry.urls.len()];
            let addr_str = upstream::url_to_host_port(url).ok_or_else(|| {
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
            (addr_str, tls, sni)
        } else {
            match &req_ctx.upstream {
                UpstreamTarget::Proxy { addr, tls, sni, .. } => (addr.clone(), *tls, sni.clone()),
                _ => {
                    return Err(pingora_core::Error::explain(
                        pingora_core::ErrorType::InternalError,
                        "upstream_peer called for local handler",
                    ))
                }
            }
        };

        let socket_addr: SocketAddr = addr_str.parse().map_err(|_| {
            pingora_core::Error::explain(
                pingora_core::ErrorType::ConnectProxyFailure,
                format!("invalid upstream address: {addr_str}"),
            )
        })?;
        Ok(Box::new(HttpPeer::new(socket_addr, tls, sni)))
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
            if let UpstreamTarget::Proxy {
                strip_prefix: Some(pfx),
                ..
            } = &ctx_ref.upstream
            {
                let old_path = upstream_request.uri.path().to_owned();
                let new_path = old_path.strip_prefix(pfx.as_str()).unwrap_or("/");
                let new_path = if new_path.is_empty() { "/" } else { new_path };
                if new_path != old_path {
                    let new_uri = rebuild_uri(&upstream_request.uri, new_path)?;
                    upstream_request.set_uri(new_uri);
                }
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
        // Inject CORS + security headers into proxy responses.
        if let Some(req_ctx) = ctx.as_ref() {
            for (name, value) in &req_ctx.extra_headers {
                upstream_response.insert_header(name.clone(), value.clone())?;
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
            }
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
    }
}

enum HandlerKind {
    Health,
    Metrics,
    StaticFile,
    Fallback,
    Proxy,
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
