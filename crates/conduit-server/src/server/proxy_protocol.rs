//! The PROXY protocol on the HTTP and HTTPS listeners (issues #42 and #757).
//!
//! Pingora's [`PreTlsProcess`] callback reads the header before a TLS handshake. For plain HTTP, Conduit wraps Pingora's
//! [`ServerApp`] and reads the header before delegating to the HTTP parser. Both look at the local port of the connection to
//! find the `proxyProtocol` block, and do nothing on a port that has none.
//!
//! For a port that has one the header is required and is believed only from `trustedSources`: any other connection is
//! dropped before the handshake or HTTP parsing. The client address in the header then replaces the socket's peer address in the socket
//! digest, which is what `session.client_addr()` reads: `ipFilter`, rate limits, `X-Forwarded-For` and the access log all
//! follow with no change of their own. Bytes read after the header are put back for the TLS handshake or HTTP parser.
//!
//! Pingora logs TLS connections dropped by the callback as downstream handshake errors (`error`); the HTTP wrapper logs
//! refused connections at `debug` (one line per scanner connection would flood the log). Both messages include the reason.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use conduit_tcp::proxy_protocol::{read_header, Header, ReadError};
use conduit_tcp::ProxyProtocolConfig;
use pingora_core::apps::ServerApp;
use pingora_core::listeners::PreTlsProcess;
use pingora_core::protocols::l4::stream::Stream as L4Stream;
use pingora_core::protocols::{GetSocketDigest, SocketDigest, Stream};
use pingora_core::server::ShutdownWatch;
use pingora_core::{Error, ErrorType, Result};

use crate::config::schema::SiteConfig;

pub(super) type SiteProxyProtocolHooks = (
    Option<Arc<PreTlsProxyProtocol>>,
    Option<Arc<PreHttpProxyProtocol>>,
);

/// The hooks for the HTTPS ports and for the plain HTTP ports of `sites`; each is `None` when no site asks for the PROXY
/// protocol on such a port.
///
/// `has_tls_listener` says which ports really get a TLS listener. A site whose certificate is missing (or whose ACME
/// order failed) falls back to plain HTTP, where this hook never runs: serving it that way would accept connections
/// without the header the operator required, so start-up is refused instead.
pub(super) fn hooks_for_sites(
    sites: &[SiteConfig],
    has_tls_listener: impl Fn(u16) -> bool,
    has_plain_http_listener: impl Fn(u16) -> bool,
) -> anyhow::Result<SiteProxyProtocolHooks> {
    let mut tls_ports = HashMap::new();
    let mut http_ports = HashMap::new();
    for (i, site) in sites.iter().enumerate().filter(|(_, s)| s.tcp.is_none()) {
        let Some(pp) = &site.proxy_protocol else {
            continue;
        };
        let port = site
            .port
            .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
        // Port 0 binds an OS-assigned port that no connection's local port could look up here: refused, as in `validate`.
        anyhow::ensure!(
            port != 0,
            "sites[{i}] has proxyProtocol on port 0, which binds a port the policy cannot be found by"
        );
        if has_tls_listener(port) {
            // Sites that share a port must agree (validated), so the first one is the port's policy.
            tls_ports.entry(port).or_insert_with(|| pp.clone());
        } else if has_plain_http_listener(port) && site.tls.is_none() {
            http_ports.entry(port).or_insert_with(|| pp.clone());
        } else {
            anyhow::bail!(
                "sites[{i}] has proxyProtocol, but port {port} has no matching HTTP or TLS listener (its \
                 certificate is missing or its ACME order failed): not serving it without the header it requires"
            );
        }
    }
    Ok((
        (!tls_ports.is_empty()).then(|| Arc::new(PreTlsProxyProtocol { ports: tls_ports })),
        (!http_ports.is_empty()).then(|| Arc::new(PreHttpProxyProtocol { ports: http_ports })),
    ))
}

/// The `proxyProtocol` block of each HTTPS port that has one, found by the port a connection arrived on.
pub(super) struct PreTlsProxyProtocol {
    ports: HashMap<u16, ProxyProtocolConfig>,
}

impl PreTlsProxyProtocol {
    /// The callback for one `tcp` site that terminates TLS: the header comes before the handshake, so the relay (which
    /// only sees the decrypted bytes) cannot read it. The site is its own listener, so the callback has one port.
    #[cfg(feature = "tcp")]
    pub(super) fn for_tcp_site(port: u16, pp: &ProxyProtocolConfig) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            port != 0,
            "a tcp site has proxyProtocol on port 0, which binds a port the policy cannot be found by"
        );
        Ok(Arc::new(Self {
            ports: HashMap::from([(port, pp.clone())]),
        }))
    }

    /// The ports with a `proxyProtocol`, sorted (for the start-up log).
    pub(super) fn ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self.ports.keys().copied().collect();
        ports.sort_unstable();
        ports
    }
}

/// The `proxyProtocol` blocks on plain HTTP ports. The header is read before Pingora parses the request, and the stream is
/// handed to the HTTP proxy only after its client address has been replaced.
pub(super) struct PreHttpProxyProtocol {
    ports: HashMap<u16, ProxyProtocolConfig>,
}

impl PreHttpProxyProtocol {
    /// The HTTP ports with a `proxyProtocol`, sorted (for the start-up log).
    pub(super) fn ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self.ports.keys().copied().collect();
        ports.sort_unstable();
        ports
    }

    /// Read and remove the required header before the HTTP parser sees the stream.
    async fn process(&self, stream: Stream) -> Option<Stream> {
        let Some(digest) = stream.get_socket_digest() else {
            tracing::warn!(
                "plain HTTP connection has no socket digest; refusing PROXY protocol handling"
            );
            return None;
        };
        // Fail closed: without a local address the port, and so whether this connection must carry a header, is unknown.
        // Letting it through would skip the policy on a port that requires one.
        let Some(local) = inet(digest.local_addr()) else {
            tracing::warn!(
                "plain HTTP connection has no local address; refusing PROXY protocol handling"
            );
            return None;
        };
        let Some(cfg) = self.ports.get(&local.port()) else {
            return Some(stream);
        };

        let Ok(mut stream) = stream.into_any().downcast::<L4Stream>() else {
            tracing::warn!(
                port = local.port(),
                "plain HTTP stream is not a Pingora TCP stream"
            );
            return None;
        };
        if let Err(error) = read_proxy_header(&mut stream, cfg, local.port()).await {
            tracing::debug!(port = local.port(), error = %error, "plain HTTP connection refused by PROXY protocol");
            return None;
        }
        let stream: Stream = stream;
        Some(stream)
    }
}

/// The HTTP listener wrapper. `HttpProxy` normally starts parsing as soon as Pingora accepts a stream; this wrapper handles
/// PROXY protocol first on the configured plain HTTP ports, then delegates to the unchanged HTTP proxy.
pub(super) struct HttpProxyWithProxyProtocol<A> {
    inner: Arc<A>,
    plain_http: Option<Arc<PreHttpProxyProtocol>>,
}

impl<A> HttpProxyWithProxyProtocol<A> {
    pub(super) fn new(inner: A, plain_http: Option<Arc<PreHttpProxyProtocol>>) -> Self {
        Self {
            inner: Arc::new(inner),
            plain_http,
        }
    }
}

#[async_trait]
impl<A> ServerApp for HttpProxyWithProxyProtocol<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    async fn process_new(
        self: &Arc<Self>,
        stream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let stream = match &self.plain_http {
            Some(hook) => hook.process(stream).await?,
            None => stream,
        };
        self.inner.process_new(stream, shutdown).await
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

fn refuse(why: impl Into<String>) -> Box<Error> {
    Error::explain(ErrorType::Custom("ProxyProtocol"), why.into())
}

fn inet(addr: Option<&pingora_core::protocols::l4::socket::SocketAddr>) -> Option<SocketAddr> {
    addr.and_then(|a| a.as_inet().copied())
}

#[async_trait]
impl PreTlsProcess for PreTlsProxyProtocol {
    async fn process(&self, stream: &mut L4Stream) -> Result<()> {
        let Some(digest) = stream.get_socket_digest() else {
            return Err(refuse("the connection has no socket digest"));
        };
        let Some(local) = inet(digest.local_addr()) else {
            return Err(refuse("the local address of the connection is not known"));
        };
        let Some(cfg) = self.ports.get(&local.port()) else {
            return Ok(()); // a port without proxyProtocol
        };
        read_proxy_header(stream, cfg, local.port()).await
    }
}

/// Require a trusted peer to send one valid header, restore bytes read past it, and replace the socket peer when the header
/// contains a PROXY address. Used before TLS and before plain HTTP parsing.
async fn read_proxy_header(
    stream: &mut L4Stream,
    cfg: &ProxyProtocolConfig,
    port: u16,
) -> Result<()> {
    let Some(digest) = stream.get_socket_digest() else {
        return Err(refuse("the connection has no socket digest"));
    };
    let peer = inet(digest.peer_addr())
        .ok_or_else(|| refuse("the peer has no IP address, so it cannot be a trusted source"))?;
    if !cfg.trusts(&peer.ip()) {
        return Err(refuse(format!(
            "{} is not in proxyProtocol.trustedSources of port {}",
            peer.ip(),
            port
        )));
    }
    let (header, rest) = read_header(stream, cfg.header_cap(), cfg.timeout())
        .await
        .map_err(|e| {
            refuse(match e {
                ReadError::Refused(why) => why.to_owned(),
                ReadError::Timeout => {
                    "no complete PROXY protocol header within timeoutMs".to_owned()
                }
                ReadError::Io(e) => format!("PROXY protocol header: {e}"),
            })
        })?;
    // Bytes after the header belong to the TLS handshake or HTTP parser.
    stream.rewind(&rest);
    if let Header::Proxy { source, .. } = header {
        replace_peer(stream, &digest, source);
    }
    Ok(())
}

/// Give `stream` a socket digest whose peer address is `peer` (the cell of a digest cannot be changed once it is set).
fn replace_peer(stream: &mut L4Stream, old: &SocketDigest, peer: SocketAddr) {
    #[cfg(unix)]
    let new = SocketDigest::from_raw_fd(std::os::fd::AsRawFd::as_raw_fd(stream));
    #[cfg(windows)]
    let new =
        SocketDigest::from_raw_socket(std::os::windows::io::AsRawSocket::as_raw_socket(stream));
    let _ = new.peer_addr.set(Some(peer.into()));
    if let Some(local) = old.local_addr() {
        let _ = new.local_addr.set(Some(local.clone()));
    }
    stream.set_socket_digest(new);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A connection to a loopback listener: the client end, and the server end the way Pingora hands it to the
    /// callback (a layer-4 stream whose socket digest carries the peer address). Also returns the listener's port.
    async fn raw_pair() -> (tokio::net::TcpStream, L4Stream, u16, SocketAddr) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let (server, peer) = listener.accept().await.unwrap();
        (client, L4Stream::from(server), port, peer)
    }

    async fn pair() -> (tokio::net::TcpStream, L4Stream, u16) {
        let (client, mut stream, port, peer) = raw_pair().await;
        let digest = SocketDigest::from_raw_fd(std::os::fd::AsRawFd::as_raw_fd(&stream));
        digest.peer_addr.set(Some(peer.into())).expect("new cell");
        stream.set_socket_digest(digest);
        (client, stream, port)
    }

    async fn pair_without_digest() -> (tokio::net::TcpStream, L4Stream, u16) {
        let (client, stream, port, _) = raw_pair().await;
        (client, stream, port)
    }

    fn hook(port: u16, sources: &[&str]) -> PreTlsProxyProtocol {
        PreTlsProxyProtocol {
            ports: HashMap::from([(
                port,
                ProxyProtocolConfig {
                    trusted_sources: sources.iter().map(|s| (*s).to_owned()).collect(),
                    timeout_ms: Some(300),
                    ..Default::default()
                },
            )]),
        }
    }

    fn http_hook(port: u16, sources: &[&str]) -> PreHttpProxyProtocol {
        PreHttpProxyProtocol {
            ports: HashMap::from([(
                port,
                ProxyProtocolConfig {
                    trusted_sources: sources.iter().map(|s| (*s).to_owned()).collect(),
                    timeout_ms: Some(300),
                    ..Default::default()
                },
            )]),
        }
    }

    #[derive(Clone)]
    struct TestServerApp {
        process_calls: Arc<AtomicUsize>,
        cleanup_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ServerApp for TestServerApp {
        async fn process_new(
            self: &Arc<Self>,
            stream: Stream,
            _shutdown: &ShutdownWatch,
        ) -> Option<Stream> {
            self.process_calls.fetch_add(1, Ordering::Relaxed);
            Some(stream)
        }

        async fn cleanup(&self) {
            self.cleanup_calls.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn peer_of(stream: &L4Stream) -> SocketAddr {
        inet(stream.get_socket_digest().unwrap().peer_addr()).unwrap()
    }

    #[tokio::test]
    async fn a_trusted_header_replaces_the_peer_and_hands_back_what_follows() {
        let (mut client, mut stream, port) = pair().await;
        let real = peer_of(&stream);
        client
            .write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\n\x16\x03\x01tls")
            .await
            .unwrap();
        hook(port, &["127.0.0.0/8"])
            .process(&mut stream)
            .await
            .unwrap();
        assert_eq!(peer_of(&stream), "203.0.113.9:51000".parse().unwrap());
        assert_ne!(peer_of(&stream), real);
        // the local address of the socket is still the real one
        let local = inet(stream.get_socket_digest().unwrap().local_addr()).unwrap();
        assert_eq!(local.port(), port);
        // what came after the header is what the TLS handshake will read
        let mut after = [0u8; 6];
        stream.read_exact(&mut after).await.unwrap();
        assert_eq!(&after, b"\x16\x03\x01tls");
    }

    #[tokio::test]
    async fn a_local_header_keeps_the_socket_peer() {
        let (mut client, mut stream, port) = pair().await;
        let real = peer_of(&stream);
        client.write_all(b"PROXY UNKNOWN\r\nrest").await.unwrap();
        hook(port, &["127.0.0.1"])
            .process(&mut stream)
            .await
            .unwrap();
        assert_eq!(peer_of(&stream), real);
        let mut after = [0u8; 4];
        stream.read_exact(&mut after).await.unwrap();
        assert_eq!(&after, b"rest");
    }

    #[tokio::test]
    async fn an_untrusted_peer_is_dropped_without_reading_anything() {
        let (mut client, mut stream, port) = pair().await;
        client
            .write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\n")
            .await
            .unwrap();
        let err = hook(port, &["192.0.2.0/24"])
            .process(&mut stream)
            .await
            .expect_err("not a trusted source");
        assert!(err.to_string().contains("trustedSources"), "{err}");
        // the header was not consumed, and the claimed address was not adopted
        assert_ne!(peer_of(&stream), "203.0.113.9:51000".parse().unwrap());
    }

    #[tokio::test]
    async fn a_trusted_peer_without_a_header_is_dropped() {
        let (mut client, mut stream, port) = pair().await;
        client.write_all(b"\x16\x03\x01\x02\x00").await.unwrap();
        let err = hook(port, &["127.0.0.1"])
            .process(&mut stream)
            .await
            .expect_err("a TLS hello is not a PROXY header");
        assert!(err.to_string().contains("PROXY protocol"), "{err}");
    }

    #[tokio::test]
    async fn a_silent_trusted_peer_is_dropped_at_the_deadline() {
        let (_client, mut stream, port) = pair().await;
        let started = std::time::Instant::now();
        let err = hook(port, &["127.0.0.1"])
            .process(&mut stream)
            .await
            .expect_err("no header");
        assert!(err.to_string().contains("timeoutMs"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_port_without_proxy_protocol_is_left_alone() {
        let (mut client, mut stream, port) = pair().await;
        let real = peer_of(&stream);
        client.write_all(b"hello").await.unwrap();
        // the block is for another port
        hook(port.wrapping_add(1), &["127.0.0.1"])
            .process(&mut stream)
            .await
            .unwrap();
        assert_eq!(peer_of(&stream), real);
        let mut data = [0u8; 5];
        stream.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"hello", "nothing was consumed");
    }

    #[tokio::test]
    async fn a_plain_http_header_replaces_the_peer_and_hands_back_the_request_bytes() {
        let (mut client, stream, port) = pair().await;
        let request = b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n";
        client
            .write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 80\r\n")
            .await
            .unwrap();
        client.write_all(request).await.unwrap();

        let hook = PreHttpProxyProtocol {
            ports: HashMap::from([(
                port,
                ProxyProtocolConfig {
                    trusted_sources: vec!["127.0.0.0/8".to_owned()],
                    timeout_ms: Some(300),
                    ..Default::default()
                },
            )]),
        };
        let stream: Stream = Box::new(stream);
        let mut stream = hook.process(stream).await.expect("a trusted HTTP header");
        let peer = inet(stream.get_socket_digest().unwrap().peer_addr()).unwrap();
        assert_eq!(peer, "203.0.113.9:51000".parse().unwrap());
        let mut after = vec![0; request.len()];
        tokio::io::AsyncReadExt::read_exact(&mut stream, &mut after)
            .await
            .unwrap();
        assert_eq!(
            after, request,
            "the HTTP parser receives every byte after the header"
        );
    }

    #[tokio::test]
    async fn a_plain_http_header_from_an_untrusted_peer_is_dropped() {
        let (mut client, stream, port) = pair().await;
        client
            .write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 80\r\n")
            .await
            .unwrap();
        let hook = PreHttpProxyProtocol {
            ports: HashMap::from([(
                port,
                ProxyProtocolConfig {
                    trusted_sources: vec!["192.0.2.0/24".to_owned()],
                    timeout_ms: Some(300),
                    ..Default::default()
                },
            )]),
        };
        let stream: Stream = Box::new(stream);
        assert!(hook.process(stream).await.is_none());
    }

    #[tokio::test]
    async fn a_plain_http_connection_without_a_socket_digest_is_dropped() {
        let (_client, stream, port) = pair_without_digest().await;
        let stream: Stream = Box::new(stream);

        assert!(http_hook(port, &["127.0.0.0/8"])
            .process(stream)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn a_plain_http_connection_without_a_local_address_is_dropped() {
        let (_client, stream, port) = pair().await;
        stream
            .get_socket_digest()
            .unwrap()
            .local_addr
            .set(None)
            .expect("the local address has not been read yet");
        let stream: Stream = Box::new(stream);

        assert!(http_hook(port, &["127.0.0.0/8"])
            .process(stream)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn an_http_port_without_a_proxy_protocol_policy_passes_the_stream_unchanged() {
        let (mut client, stream, port) = pair().await;
        client.write_all(b"hello").await.unwrap();
        let hook = PreHttpProxyProtocol {
            ports: HashMap::from([(port.wrapping_add(1), ProxyProtocolConfig::default())]),
        };
        let stream: Stream = Box::new(stream);
        let mut stream = hook.process(stream).await.expect("unconfigured HTTP port");
        let mut received = [0; 5];
        stream.read_exact(&mut received).await.unwrap();

        assert_eq!(&received, b"hello");
    }

    #[tokio::test]
    async fn the_http_proxy_wrapper_processes_the_header_then_delegates_and_cleans_up() {
        let (mut client, stream, port) = pair().await;
        client
            .write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 80\r\n")
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let cleanups = Arc::new(AtomicUsize::new(0));
        let wrapper = Arc::new(HttpProxyWithProxyProtocol::new(
            TestServerApp {
                process_calls: calls.clone(),
                cleanup_calls: cleanups.clone(),
            },
            Some(Arc::new(http_hook(port, &["127.0.0.0/8"]))),
        ));
        let (_shutdown_tx, shutdown) = tokio::sync::watch::channel(false);

        let stream = wrapper
            .process_new(Box::new(stream), &shutdown)
            .await
            .expect("trusted PROXY header is delegated");
        let peer = inet(stream.get_socket_digest().unwrap().peer_addr()).unwrap();
        assert_eq!(peer, "203.0.113.9:51000".parse().unwrap());
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        wrapper.cleanup().await;
        assert_eq!(cleanups.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn the_http_proxy_wrapper_delegates_unchanged_when_no_hook_is_configured() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cleanups = Arc::new(AtomicUsize::new(0));
        let wrapper = Arc::new(HttpProxyWithProxyProtocol::new(
            TestServerApp {
                process_calls: calls.clone(),
                cleanup_calls: cleanups.clone(),
            },
            None,
        ));
        let (_shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        let (stream, _peer) = tokio::io::duplex(8);

        assert!(wrapper
            .process_new(Box::new(stream), &shutdown)
            .await
            .is_some());
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        wrapper.cleanup().await;
        assert_eq!(cleanups.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn read_proxy_header_refuses_a_stream_without_a_socket_digest() {
        let (_client, mut stream, port) = pair_without_digest().await;
        let err = read_proxy_header(&mut stream, &ProxyProtocolConfig::default(), port)
            .await
            .expect_err("a connection without a digest cannot be trusted");

        assert!(err.to_string().contains("socket digest"), "{err}");
    }

    #[tokio::test]
    async fn read_proxy_header_refuses_a_peer_without_an_ip_address() {
        let (_client, mut stream, port, _) = raw_pair().await;
        let digest = SocketDigest::from_raw_fd(std::os::fd::AsRawFd::as_raw_fd(&stream));
        digest.peer_addr.set(None).expect("new cell");
        stream.set_socket_digest(digest);
        let err = read_proxy_header(&mut stream, &ProxyProtocolConfig::default(), port)
            .await
            .expect_err("a peer without an IP address cannot be trusted");

        assert!(err.to_string().contains("peer has no IP address"), "{err}");
    }

    fn site(json: &str) -> SiteConfig {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_hooks_are_built_for_the_actual_http_or_tls_listener() {
        let with = site(
            r#"{ "port": 8443, "tls": { "cert": "c", "key": "k" },
                 "proxyProtocol": { "trustedSources": ["10.0.0.0/8"] } }"#,
        );
        let plain =
            site(r#"{ "port": 8080, "proxyProtocol": { "trustedSources": ["10.0.0.0/8"] } }"#);
        let without = site(r#"{ "port": 9000 }"#);
        let (tls_hook, http_hook) = hooks_for_sites(
            &[without.clone(), with.clone(), plain.clone()],
            |p| p == 8443,
            |p| p == 8080,
        )
        .unwrap();
        assert_eq!(tls_hook.expect("one TLS port has it").ports(), vec![8443]);
        assert_eq!(http_hook.expect("one HTTP port has it").ports(), vec![8080]);
        // no site asks for it: no hook
        assert!(hooks_for_sites(&[without], |_| true, |_| true)
            .unwrap()
            .0
            .is_none());
        // A TLS-configured site that falls back to a plain listener still refuses to start.
        let err = hooks_for_sites(&[with], |_| false, |_| true)
            .err()
            .expect("refused");
        assert!(
            err.to_string()
                .contains("port 8443 has no matching HTTP or TLS listener"),
            "{err}"
        );
    }

    #[test]
    fn a_tls_site_and_a_plain_site_on_one_port_share_the_tls_policy() {
        // Sites on one port must agree on proxyProtocol (validated), and the port is a TLS port when any site has a
        // certificate: the policy then belongs to the TLS hook only, never to the plain HTTP hook.
        let pp = r#""proxyProtocol": { "trustedSources": ["10.0.0.0/8"] }"#;
        let tls = site(&format!(
            r#"{{ "port": 8443, "tls": {{ "cert": "c", "key": "k" }}, {pp} }}"#
        ));
        let plain = site(&format!(r#"{{ "port": 8443, {pp} }}"#));
        let (tls_hook, http_hook) =
            hooks_for_sites(&[tls, plain], |p| p == 8443, |_| false).unwrap();
        assert_eq!(tls_hook.expect("the TLS port has it").ports(), vec![8443]);
        assert!(http_hook.is_none(), "no plain HTTP listener on that port");
    }
}
