use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_proxy::{ProxyHttp, Session};

use crate::config::schema::AppConfig;
use crate::handler::{fallback, health, static_files};
use crate::proxy::ctx::{LocalHandler, RequestCtx, UpstreamTarget};
use crate::proxy::router;

pub struct AppState {
    pub config: Arc<ArcSwap<AppConfig>>,
    pub inflight: Arc<AtomicUsize>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config: Arc::new(ArcSwap::new(Arc::new(config))),
            inflight: Arc::new(AtomicUsize::new(0)),
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

        let req_ctx = {
            let config = self.state.config.load();
            let host = extract_host(session);
            let path = session.req_header().uri.path().to_owned();
            router::route_request(&config, &host, &path)
        };

        let handler_kind = match &req_ctx.upstream {
            UpstreamTarget::Local(LocalHandler::Health) => HandlerKind::Health,
            UpstreamTarget::Local(LocalHandler::StaticFile { .. }) => HandlerKind::StaticFile,
            UpstreamTarget::Local(_) => HandlerKind::Fallback,
            _ => HandlerKind::Proxy,
        };

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
            UpstreamTarget::Proxy { addr, .. } => {
                Ok(Box::new(HttpPeer::new(*addr, false, String::new())))
            }
            _ => Err(pingora_core::Error::explain(
                pingora_core::ErrorType::InternalError,
                "upstream_peer called for local handler",
            )),
        }
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
