//! The PROXY protocol on the HTTPS listeners (issue #757).
//!
//! Pingora runs one callback per listener collection after a connection is accepted and before the TLS handshake
//! ([`PreTlsProcess`]). Conduit serves every HTTP(S) site from one service, so the callback looks at the local port of the
//! connection to find the `proxyProtocol` block of the sites on it, and does nothing on a port that has none.
//!
//! For a port that has one the header is required and is believed only from `trustedSources`: any other connection is
//! dropped before the handshake. The client address in the header then replaces the socket's peer address in the socket
//! digest, which is what `session.client_addr()` reads: `ipFilter`, rate limits, `X-Forwarded-For` and the access log all
//! follow with no change of their own. Bytes read after the header are put back for the TLS handshake.
//!
//! Pingora logs a connection dropped here as a downstream handshake error (one line per connection, at `error`); the
//! text names the reason.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use conduit_tcp::proxy_protocol::{read_header, Header, ReadError};
use conduit_tcp::ProxyProtocolConfig;
use pingora_core::listeners::PreTlsProcess;
use pingora_core::protocols::l4::stream::Stream as L4Stream;
use pingora_core::protocols::{GetSocketDigest, SocketDigest};
use pingora_core::{Error, ErrorType, Result};

use crate::config::schema::SiteConfig;

/// The `proxyProtocol` block of each HTTPS port that has one, found by the port a connection arrived on.
pub(super) struct PreTlsProxyProtocol {
    ports: HashMap<u16, ProxyProtocolConfig>,
}

impl PreTlsProxyProtocol {
    /// The callback for the HTTPS ports of `sites`, or `None` when no site asks for the PROXY protocol on one.
    ///
    /// `has_tls_listener` says which ports really get a TLS listener. A site whose certificate is missing (or whose ACME
    /// order failed) falls back to plain HTTP, where this hook never runs: serving it that way would accept connections
    /// without the header the operator required, so start-up is refused instead.
    pub(super) fn for_sites(
        sites: &[SiteConfig],
        has_tls_listener: impl Fn(u16) -> bool,
    ) -> anyhow::Result<Option<Arc<Self>>> {
        let mut ports = HashMap::new();
        for (i, site) in sites.iter().enumerate().filter(|(_, s)| s.tcp.is_none()) {
            let Some(pp) = &site.proxy_protocol else {
                continue;
            };
            let port = site
                .port
                .unwrap_or(if site.tls.is_some() { 443 } else { 80 });
            anyhow::ensure!(
                has_tls_listener(port),
                "sites[{i}] has proxyProtocol, but port {port} has no TLS listener (its certificate is missing or its \
                 ACME order failed): not serving it without the header it requires"
            );
            // sites that share a port must agree (validated), so the first one is the port's
            ports.entry(port).or_insert_with(|| pp.clone());
        }
        Ok((!ports.is_empty()).then(|| Arc::new(Self { ports })))
    }

    /// The ports with a `proxyProtocol`, sorted (for the start-up log).
    pub(super) fn ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self.ports.keys().copied().collect();
        ports.sort_unstable();
        ports
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
        let peer = inet(digest.peer_addr()).ok_or_else(|| {
            refuse("the peer has no IP address, so it cannot be a trusted source")
        })?;
        if !cfg.trusts(&peer.ip()) {
            return Err(refuse(format!(
                "{} is not in proxyProtocol.trustedSources of port {}",
                peer.ip(),
                local.port()
            )));
        }
        let read = read_header(stream, cfg.header_cap(), cfg.timeout()).await;
        let (header, rest) = read.map_err(|e| {
            refuse(match e {
                ReadError::Refused(why) => why.to_owned(),
                ReadError::Timeout => {
                    "no complete PROXY protocol header within timeoutMs".to_owned()
                }
                ReadError::Io(e) => format!("PROXY protocol header: {e}"),
            })
        })?;
        // The bytes after the header belong to the TLS handshake.
        stream.rewind(&rest);
        if let Header::Proxy { source, .. } = header {
            replace_peer(stream, &digest, source);
        }
        Ok(())
    }
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A connection to a loopback listener: the client end, and the server end the way Pingora hands it to the
    /// callback (a layer-4 stream whose socket digest carries the peer address). Also returns the listener's port.
    async fn pair() -> (tokio::net::TcpStream, L4Stream, u16) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let (server, peer) = listener.accept().await.unwrap();
        let mut stream = L4Stream::from(server);
        let digest = SocketDigest::from_raw_fd(std::os::fd::AsRawFd::as_raw_fd(&stream));
        digest.peer_addr.set(Some(peer.into())).expect("new cell");
        stream.set_socket_digest(digest);
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

    fn site(json: &str) -> SiteConfig {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_hook_is_built_only_for_ports_that_have_a_tls_listener() {
        let with = site(
            r#"{ "port": 8443, "tls": { "cert": "c", "key": "k" },
                 "proxyProtocol": { "trustedSources": ["10.0.0.0/8"] } }"#,
        );
        let without = site(r#"{ "port": 9000 }"#);
        let hook = PreTlsProxyProtocol::for_sites(&[without.clone(), with.clone()], |p| p == 8443)
            .unwrap()
            .expect("one port has it");
        assert_eq!(hook.ports(), vec![8443]);
        // no site asks for it: no hook
        assert!(PreTlsProxyProtocol::for_sites(&[without], |_| true)
            .unwrap()
            .is_none());
        // a site that asks for it but ends up on a plain listener stops the start
        let err = PreTlsProxyProtocol::for_sites(&[with], |_| false)
            .err()
            .expect("refused");
        assert!(
            err.to_string().contains("port 8443 has no TLS listener"),
            "{err}"
        );
    }
}
