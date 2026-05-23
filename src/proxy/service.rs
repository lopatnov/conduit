use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::RequestHeader;
use pingora_proxy::{ProxyHttp, Session};

use crate::config::schema::AppConfig;
use crate::filter::{ip_filter, limits};
use crate::handler::{fallback, health, response, static_files};
use crate::proxy::ctx::{LocalHandler, RequestCtx, UpstreamTarget};
use crate::proxy::router;

pub struct AppState {
    pub config: Arc<ArcSwap<AppConfig>>,
    pub inflight: Arc<AtomicUsize>,
    /// Per-route round-robin counters shared across all request threads.
    pub round_robin: Arc<DashMap<String, AtomicUsize>>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config: Arc::new(ArcSwap::new(Arc::new(config))),
            inflight: Arc::new(AtomicUsize::new(0)),
            round_robin: Arc::new(DashMap::new()),
        }
    }
}

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

        let (req_ctx, ip_cfg, limits_cfg) = {
            let config = self.state.config.load();
            let host = extract_host(session);
            let path = session.req_header().uri.path().to_owned();
            let req_ctx = router::route_request(&config, &host, &path, &self.state.round_robin);
            let site = config.sites.get(req_ctx.site_idx);
            let ip_cfg = site.and_then(|s| s.ip_filter.clone());
            let limits_cfg = site.and_then(|s| s.limits.clone());
            (req_ctx, ip_cfg, limits_cfg)
        };

        if let Some(ref ip_cfg) = ip_cfg {
            if !ip_filter::is_allowed(ip_cfg, session) {
                response::write_response(
                    session,
                    403,
                    "text/plain",
                    Bytes::from_static(b"Forbidden"),
                )
                .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(true);
            }
        }

        let handler_kind = match &req_ctx.upstream {
            UpstreamTarget::Local(LocalHandler::Health) => HandlerKind::Health,
            UpstreamTarget::Local(LocalHandler::StaticFile { .. }) => HandlerKind::StaticFile,
            UpstreamTarget::Local(_) => HandlerKind::Fallback,
            _ => HandlerKind::Proxy,
        };

        if !matches!(handler_kind, HandlerKind::Health) {
            if let Some(ref limits_cfg) = limits_cfg {
                match limits::check(limits_cfg, session) {
                    limits::CheckResult::BodyTooLarge => {
                        response::write_response(
                            session,
                            413,
                            "text/plain",
                            Bytes::from_static(b"Request Entity Too Large"),
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
                        )
                        .await?;
                        self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                        return Ok(true);
                    }
                    limits::CheckResult::Ok => {}
                }
            }
        }

        *ctx = Some(req_ctx);

        match handler_kind {
            HandlerKind::Health => {
                health::handle_health(session).await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::StaticFile => {
                let (roots, options, strip_prefix) =
                    if let Some(RequestCtx {
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
                static_files::handle_static(session, &roots, &options, strip_prefix.as_deref())
                    .await?;
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(true)
            }
            HandlerKind::Fallback => {
                let config = self.state.config.load();
                let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
                let site = config.sites.get(site_idx);
                fallback::handle_fallback(session, site).await?;
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
        let ctx = ctx.as_ref().expect("ctx set in request_filter");
        match &ctx.upstream {
            UpstreamTarget::Proxy { addr, tls, sni, .. } => {
                let socket_addr: SocketAddr = addr.parse().map_err(|_| {
                    pingora_core::Error::explain(
                        pingora_core::ErrorType::ConnectProxyFailure,
                        format!("invalid upstream address: {addr}"),
                    )
                })?;
                Ok(Box::new(HttpPeer::new(socket_addr, *tls, sni.clone())))
            }
            _ => Err(pingora_core::Error::explain(
                pingora_core::ErrorType::InternalError,
                "upstream_peer called for local handler",
            )),
        }
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
        // Add X-Forwarded-For
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

        // X-Forwarded-Proto (TLS support added in Phase 1.9)
        upstream_request.insert_header("x-forwarded-proto", "http")?;

        // Strip prefix from request path if configured
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

    async fn logging(
        &self,
        _session: &mut Session,
        _e: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        if let Some(req_ctx) = ctx {
            if !matches!(req_ctx.upstream, UpstreamTarget::Local(_)) {
                self.state.inflight.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

enum HandlerKind {
    Health,
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
