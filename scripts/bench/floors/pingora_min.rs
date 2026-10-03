// Floor for issue #475: the smallest Pingora HTTP proxy — same runtime, connector and keep-alive pool as Conduit, no Conduit code.
// 127.0.0.1:8080 -> 127.0.0.1:4000, ServerConf defaults (threads = 1).
use async_trait::async_trait;
use pingora_core::server::Server;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_proxy::{http_proxy_service, ProxyHttp, Session};

struct Passthrough;

#[async_trait]
impl ProxyHttp for Passthrough {
    type CTX = ();
    fn new_ctx(&self) -> Self::CTX {}
    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut Self::CTX) -> Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new("127.0.0.1:4000", false, String::new())))
    }
}

fn main() {
    let mut server = Server::new(None).unwrap();
    server.bootstrap();
    let mut service = http_proxy_service(&server.configuration, Passthrough);
    service.add_tcp("127.0.0.1:8080");
    server.add_service(service);
    server.run_forever();
}
