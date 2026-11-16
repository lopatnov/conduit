//! Raw TCP proxy — forwards bytes bidirectionally between client and upstream.
//!
//! Implements Pingora's [`ServerApp`] trait: for every accepted TCP connection,
//! the proxy picks an upstream target, connects, and relays bytes in both
//! directions until either side closes.
//!
//! Use cases: MySQL, PostgreSQL, Redis, SMTP, and any other non-HTTP protocol.
//!
//! # Configuration
//!
//! ```yaml
//! sites:
//!   - port: 3306
//!     tcp:
//!       targets: ["mysql-primary:3306", "mysql-replica:3306"]
//!       strategy: round-robin     # optional; default
//!       connectTimeoutMs: 5000    # optional
//!       maxConnections: 1000      # optional; none by default
//!       maxConnectionsPerIp: 20   # optional; none by default
//!       idleTimeoutSecs: 3600     # optional; default 3600, 0 = never
//!       sendProxyProtocol: v2     # optional; write a PROXY protocol header to the upstream
//!     proxyProtocol:              # optional; read one from the client (a load balancer)
//!       trustedSources: ["10.0.0.0/8"]
//! ```
//!
//! A connection over `maxConnections` or `maxConnectionsPerIp` is closed at once, before an upstream is chosen (#757;
//! see [`crate::limits`]), and one that has been quiet in both directions for `idleTimeoutSecs` is closed (see
//! [`crate::relay`]).
//!
//! With `proxyProtocol` the connection must start with a PROXY protocol header from a trusted address (see
//! [`crate::proxy_protocol`]); the client address it names is the one the caps count, and `sendProxyProtocol` passes it
//! on to the upstream. Anything else is closed before a byte of it reaches the upstream (#757).
//!
//! A connect error or timeout is not the end of the connection: the remaining targets are tried once each, in
//! order from the one picked, and a target that just failed is left out of the pick for [`DOWN_FOR`] so that a dead
//! one does not cost a connect timeout per connection (#725; nginx `proxy_next_upstream` + `fail_timeout`).

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pingora_core::apps::ServerApp;
use pingora_core::protocols::Stream;
use pingora_core::server::ShutdownWatch;
use tokio::net::TcpStream;

use crate::config::{ProxyProtocolConfig, TcpConfig};
use crate::limits::{Limits, Refusal};
use crate::proxy_protocol::{self as pp, Header, ReadError};
use crate::relay::{relay, Outcome};
use crate::throttle::Throttle;
use tokio::io::AsyncWriteExt;

/// How long a target that failed to connect is passed over while another one is available.
const DOWN_FOR: Duration = Duration::from_secs(5);

/// The idle timeout of a connection when `idleTimeoutSecs` is not set (Envoy's `tcp_proxy.idle_timeout`; nginx
/// `proxy_timeout` is 10 minutes). Without any, a client that goes away without closing holds a slot for ever.
const DEFAULT_IDLE_SECS: u64 = 3600;

/// Raw TCP proxy application.
///
/// Implements [`ServerApp`] so it can be registered directly with a Pingora
/// `listening::Service`.  Each connection is handled in a dedicated tokio task
/// spawned by Pingora.
pub struct TcpProxy {
    /// Upstream host:port addresses.
    targets: Vec<String>,
    /// Round-robin cursor.
    counter: AtomicUsize,
    /// Whether to use random selection instead of round-robin.
    random: bool,
    /// Upstream connect timeout.
    connect_timeout: Duration,
    /// Per target: milliseconds after `epoch` until which it is passed over (0 = not marked down).
    down_until_ms: Vec<AtomicU64>,
    epoch: Instant,
    /// Connection caps of the site.
    limits: Arc<Limits>,
    /// Quiet time after which a connection is closed (`None` = never).
    idle_timeout: Option<Duration>,
    /// Names the site in log lines (its port); 0 until [`TcpProxy::with_port`].
    port: u16,
    /// `proxyProtocol`: read the header of the client's connection (`None` = the socket address is the client).
    accept: Option<Accept>,
    /// `sendProxyProtocol`: the header to write to the upstream.
    send: Option<pp::Version>,
}

/// The accept side of the PROXY protocol on one site.
struct Accept {
    cfg: ProxyProtocolConfig,
    /// Which connections that did not pass are logged (a client can cause them at will).
    refused: Throttle,
}

/// Where a connection came from and where it was going, as far as is known.
struct Endpoints {
    client: Option<SocketAddr>,
    destination: Option<SocketAddr>,
}

impl TcpProxy {
    pub fn new(cfg: &TcpConfig) -> Self {
        let random = cfg
            .strategy
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("random"))
            .unwrap_or(false);
        let connect_timeout = Duration::from_millis(cfg.connect_timeout_ms.unwrap_or(5_000));
        Self {
            targets: cfg.targets.clone(),
            counter: AtomicUsize::new(0),
            random,
            connect_timeout,
            down_until_ms: cfg.targets.iter().map(|_| AtomicU64::new(0)).collect(),
            epoch: Instant::now(),
            limits: Limits::new(cfg.max_connections, cfg.max_connections_per_ip),
            idle_timeout: match cfg.idle_timeout_secs.unwrap_or(DEFAULT_IDLE_SECS) {
                0 => None,
                secs => Some(Duration::from_secs(secs)),
            },
            port: 0,
            accept: None,
            send: cfg.send_proxy_protocol,
        }
    }

    /// Require a PROXY protocol header on every connection, from the addresses `cfg` trusts.
    pub fn with_proxy_protocol(mut self, cfg: &ProxyProtocolConfig) -> Self {
        self.accept = Some(Accept {
            cfg: cfg.clone(),
            refused: Throttle::default(),
        });
        self
    }

    /// The port the site listens on, named in the log line about refused connections.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    fn now_ms(&self) -> u64 {
        // +1 so that a mark made at the very first millisecond is not the "not down" value 0.
        crate::relay::millis(self.epoch.elapsed()).saturating_add(1)
    }

    fn is_down(&self, idx: usize) -> bool {
        self.down_until_ms[idx].load(Ordering::Relaxed) > self.now_ms()
    }

    fn mark_down(&self, idx: usize) {
        let until = self.now_ms().saturating_add(crate::relay::millis(DOWN_FOR));
        self.down_until_ms[idx].store(until, Ordering::Relaxed);
    }

    /// The order in which to try the targets for one connection: every target once, starting at the picked one;
    /// those marked down come last, so they are tried only when nothing else is left.
    fn try_order(&self) -> Vec<usize> {
        let n = self.targets.len();
        if n == 0 {
            return Vec::new();
        }
        let start = self.pick_index();
        let cyclic = (0..n).map(|k| (start + k) % n);
        let (up, down): (Vec<usize>, Vec<usize>) = cyclic.partition(|&i| !self.is_down(i));
        up.into_iter().chain(down).collect()
    }

    /// Connect to the first target that answers, marking each one that does not as down.
    async fn connect_any(&self) -> Option<(TcpStream, &str)> {
        for idx in self.try_order() {
            let target = self.targets[idx].as_str();
            match tokio::time::timeout(self.connect_timeout, TcpStream::connect(target)).await {
                Ok(Ok(stream)) => return Some((stream, target)),
                Ok(Err(e)) => tracing::warn!(target, "TCP proxy: upstream connect error: {e}"),
                Err(_) => tracing::warn!(
                    target,
                    timeout_ms = self.connect_timeout.as_millis(),
                    "TCP proxy: upstream connect timed out"
                ),
            }
            self.mark_down(idx);
        }
        None
    }

    /// Read the PROXY protocol header of a connection (the site has `proxyProtocol`) and put what it says into `ends`.
    /// `None` when the connection is refused: its peer is not trusted, or it did not send a valid header in time.
    /// Returns the payload bytes that came in after the header.
    async fn read_proxy_header(
        &self,
        accept: &Accept,
        downstream: &mut Stream,
        ends: &mut Endpoints,
    ) -> Option<Vec<u8>> {
        let peer = ends.client?; // a client with no IP address (a unix socket) cannot be trusted
        if !accept.cfg.trusts(&peer.ip()) {
            if let Some(n) = accept.refused.note() {
                tracing::warn!(
                    port = self.port,
                    peer = %peer.ip(),
                    refused = n,
                    "TCP proxy: connection from an address not in proxyProtocol.trustedSources closed"
                );
            }
            return None;
        }
        // A trusted peer that has not sent its header yet holds a connection, so the site cap applies while it waits.
        let waiting = match self.limits.admit(None) {
            Ok(permit) => permit,
            Err(_) => {
                if let Some(n) = self.limits.note_refusal() {
                    tracing::warn!(
                        port = self.port,
                        refusal = "maxConnections",
                        refused = n,
                        "TCP proxy: connections refused because a limit is reached"
                    );
                }
                return None;
            }
        };
        let read = pp::read_header(downstream, accept.cfg.header_cap(), accept.cfg.timeout()).await;
        drop(waiting);
        match read {
            Ok((
                Header::Proxy {
                    source,
                    destination,
                },
                rest,
            )) => {
                ends.client = Some(source);
                ends.destination = Some(destination);
                Some(rest)
            }
            // A health check of the load balancer itself: the socket address stays the client's.
            Ok((Header::Local, rest)) => Some(rest),
            Err(e) => {
                if let Some(n) = accept.refused.note() {
                    let why = match &e {
                        ReadError::Refused(why) => (*why).to_owned(),
                        ReadError::Timeout => "no complete header within timeoutMs".to_owned(),
                        ReadError::Io(e) => e.to_string(),
                    };
                    tracing::warn!(
                        port = self.port,
                        peer = %peer.ip(),
                        refused = n,
                        "TCP proxy: connection closed, no valid PROXY protocol header: {why}"
                    );
                }
                None
            }
        }
    }

    /// What the upstream gets before the relay starts: the PROXY protocol header (`sendProxyProtocol`), then the
    /// payload bytes that were read together with the client's header.
    async fn write_preamble(
        &self,
        upstream: &mut TcpStream,
        ends: &Endpoints,
        first_bytes: &[u8],
    ) -> std::io::Result<()> {
        let mut out = Vec::new();
        if let Some(version) = self.send {
            out = match (ends.client, ends.destination) {
                (Some(client), Some(destination)) => pp::encode(version, client, destination),
                // Without both ends the header says "no client known" (the receiver keeps the socket address).
                (Some(client), None) => {
                    pp::encode(version, client, unspecified_like(client.ip(), 0))
                }
                _ => pp::encode_local(version),
            };
        }
        out.extend_from_slice(first_bytes);
        if out.is_empty() {
            return Ok(());
        }
        tokio::time::timeout(self.connect_timeout, upstream.write_all(&out))
            .await
            .unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the upstream did not take the first bytes in time",
                ))
            })
    }

    /// Pick a target index using round-robin or random selection.
    fn pick_index(&self) -> usize {
        let n = self.targets.len();
        if self.random {
            // splitmix64 from nanoseconds — same RNG used elsewhere in Conduit.
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as u64;
            let mut x = seed.wrapping_mul(0x9e3779b97f4a7c15);
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            x ^= x >> 31;
            (x as usize) % n
        } else {
            self.counter.fetch_add(1, Ordering::Relaxed) % n
        }
    }

    /// Pick a target using round-robin or random selection.
    #[cfg(test)]
    fn pick_target(&self) -> Option<&str> {
        if self.targets.is_empty() {
            return None;
        }
        Some(&self.targets[self.pick_index()])
    }
}

/// The unspecified address (`0.0.0.0` / `::`) of the family of `ip`, with `port`.
fn unspecified_like(ip: IpAddr, port: u16) -> SocketAddr {
    match ip {
        IpAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], port)),
        IpAddr::V6(_) => SocketAddr::from(([0u16; 8], port)),
    }
}

#[async_trait]
impl ServerApp for TcpProxy {
    /// Accept a downstream stream, connect to an upstream, and relay bytes
    /// bidirectionally until either side closes.
    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        if self.targets.is_empty() {
            tracing::warn!("TCP proxy: no upstream targets configured");
            return None;
        }
        let socket_peer = downstream
            .get_socket_digest()
            .and_then(|digest| digest.peer_addr().and_then(|a| a.as_inet().copied()));
        let socket_local = downstream
            .get_socket_digest()
            .and_then(|digest| digest.local_addr().and_then(|a| a.as_inet().copied()));
        let mut ends = Endpoints {
            client: socket_peer,
            destination: socket_local,
        };
        // Bytes the header read pulled in after the header: payload, sent on before anything else.
        let mut first_bytes = Vec::new();
        if let Some(accept) = &self.accept {
            first_bytes = self
                .read_proxy_header(accept, &mut downstream, &mut ends)
                .await?;
        }
        let client = ends.client.map(|a| a.ip());
        // Held until the relay ends, however it ends.
        let _permit = match self.limits.admit(client) {
            Ok(permit) => permit,
            Err(refusal) => {
                if let Some(n) = self.limits.note_refusal() {
                    tracing::warn!(
                        port = self.port,
                        refusal = match refusal {
                            Refusal::SiteFull => "maxConnections",
                            Refusal::ClientFull => "maxConnectionsPerIp",
                        },
                        refused = n,
                        "TCP proxy: connections refused because a limit is reached"
                    );
                }
                return None;
            }
        };
        let Some((upstream, target)) = self.connect_any().await else {
            tracing::warn!("TCP proxy: no upstream target could be reached");
            return None;
        };

        tracing::debug!(target, "TCP proxy: relaying connection");

        let mut upstream = upstream;
        if let Err(e) = self
            .write_preamble(&mut upstream, &ends, &first_bytes)
            .await
        {
            tracing::warn!(target, "TCP proxy: could not write to the upstream: {e}");
            return None;
        }

        match relay(downstream, upstream, self.idle_timeout).await {
            Ok(Outcome::Closed(down_bytes, up_bytes)) => {
                tracing::debug!(
                    target,
                    bytes_client_to_upstream = down_bytes,
                    bytes_upstream_to_client = up_bytes,
                    "TCP proxy: connection closed"
                );
            }
            Ok(Outcome::Idle) => {
                tracing::debug!(target, "TCP proxy: idle connection closed");
            }
            Err(e) => {
                // Broken pipe and connection reset are normal — don't warn.
                use std::io::ErrorKind::{BrokenPipe, ConnectionReset};
                if e.kind() != BrokenPipe && e.kind() != ConnectionReset {
                    tracing::warn!(target, "TCP proxy: relay error: {e}");
                }
            }
        }

        None // TCP connections are never reused
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_proxy(targets: &[&str], strategy: Option<&str>) -> TcpProxy {
        TcpProxy::new(&TcpConfig {
            targets: targets.iter().map(|s| s.to_string()).collect(),
            strategy: strategy.map(str::to_owned),
            ..Default::default()
        })
    }

    #[test]
    fn round_robin_cycles_through_targets() {
        let proxy = make_proxy(&["a:1", "b:2", "c:3"], None);
        let picks: Vec<_> = (0..6)
            .map(|_| proxy.pick_target().unwrap().to_owned())
            .collect();
        // Should cycle: a, b, c, a, b, c
        assert_eq!(picks[0], picks[3]);
        assert_eq!(picks[1], picks[4]);
        assert_eq!(picks[2], picks[5]);
        // All three targets are picked.
        assert!(picks[..3].contains(&"a:1".to_owned()));
        assert!(picks[..3].contains(&"b:2".to_owned()));
        assert!(picks[..3].contains(&"c:3".to_owned()));
    }

    // ── failover (#725) ──────────────────────────────────────────────────────

    /// An address nothing listens on: bind to port 0, note the port, close the listener.
    fn dead_addr() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap().to_string();
        drop(l);
        a
    }

    #[test]
    fn a_target_marked_down_is_tried_last() {
        let proxy = make_proxy(&["a:1", "b:2", "c:3"], None);
        proxy.mark_down(0);
        for _ in 0..6 {
            let order = proxy.try_order();
            assert_eq!(order.len(), 3, "every target once: {order:?}");
            assert_eq!(
                *order.last().unwrap(),
                0,
                "the down target is last: {order:?}"
            );
        }
        // all down: still every target once, none dropped
        proxy.mark_down(1);
        proxy.mark_down(2);
        let mut order = proxy.try_order();
        order.sort_unstable();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn the_down_mark_expires() {
        let proxy = make_proxy(&["a:1", "b:2"], None);
        proxy.down_until_ms[0].store(proxy.now_ms() + 1, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(5));
        assert!(!proxy.is_down(0));
        proxy.mark_down(1);
        assert!(proxy.is_down(1));
    }

    #[tokio::test]
    async fn a_connect_failure_goes_on_to_the_next_target() {
        let live = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live_addr = live.local_addr().unwrap().to_string();
        let dead = dead_addr();
        let proxy = make_proxy(&[dead.as_str(), live_addr.as_str()], None);
        // Six connections, round-robin over a dead and a live target: all reach the live one (before #725, 3 of 6).
        for k in 0..6 {
            let (_stream, target) = proxy.connect_any().await.expect("a target answers");
            assert_eq!(target, live_addr, "connection {k}");
        }
        assert!(
            proxy.is_down(0),
            "the dead target is passed over for a while"
        );
    }

    #[tokio::test]
    async fn no_target_answering_ends_the_connection_attempt() {
        let proxy = make_proxy(&[dead_addr().as_str(), dead_addr().as_str()], None);
        assert!(proxy.connect_any().await.is_none());
    }

    #[test]
    fn single_target_always_picked() {
        let proxy = make_proxy(&["only:9999"], None);
        for _ in 0..5 {
            assert_eq!(proxy.pick_target(), Some("only:9999"));
        }
    }

    #[test]
    fn no_targets_returns_none() {
        let proxy = make_proxy(&[], None);
        assert_eq!(proxy.pick_target(), None);
    }

    #[test]
    fn random_strategy_picks_from_list() {
        let proxy = make_proxy(&["a:1", "b:2", "c:3"], Some("random"));
        let targets = ["a:1", "b:2", "c:3"];
        for _ in 0..10 {
            let picked = proxy.pick_target().unwrap();
            assert!(targets.contains(&picked), "picked {picked} not in targets");
        }
    }

    #[test]
    fn connect_timeout_defaults_to_5s() {
        let proxy = make_proxy(&["x:1"], None);
        assert_eq!(proxy.connect_timeout, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn process_new_with_no_targets_returns_none_without_connecting() {
        let proxy = Arc::new(make_proxy(&[], None));
        let (_client_side, server_side) = tokio::io::duplex(64);
        let downstream: Stream = Box::new(server_side);
        let (_tx, shutdown) = tokio::sync::watch::channel(false);

        let result = proxy.process_new(downstream, &shutdown).await;
        assert!(result.is_none(), "no targets configured must return None");
    }

    #[test]
    fn custom_connect_timeout() {
        use crate::config::TcpConfig;
        let proxy = TcpProxy::new(&TcpConfig {
            targets: vec!["x:1".to_owned()],
            connect_timeout_ms: Some(2000),
            ..Default::default()
        });
        assert_eq!(proxy.connect_timeout, Duration::from_secs(2));
    }

    // ── limits and idle timeout, through real sockets (#757) ─────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// An echo upstream; returns its address.
    async fn echo_upstream() -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    /// The stream Pingora's listener hands to a `ServerApp`: the socket digest carries the peer address.
    #[cfg(unix)]
    fn accepted_stream(s: tokio::net::TcpStream, peer: std::net::SocketAddr) -> Stream {
        use pingora_core::protocols::{GetSocketDigest, SocketDigest};
        use std::os::fd::AsRawFd;
        let mut stream = pingora_core::protocols::l4::stream::Stream::from(s);
        let digest = SocketDigest::from_raw_fd(stream.as_raw_fd());
        digest.peer_addr.set(Some(peer.into())).expect("new cell");
        stream.set_socket_digest(digest);
        Box::new(stream)
    }

    #[cfg(not(unix))]
    fn accepted_stream(s: tokio::net::TcpStream, _peer: std::net::SocketAddr) -> Stream {
        Box::new(pingora_core::protocols::l4::stream::Stream::from(s))
    }

    /// The proxy on a loopback listener, serving every accepted connection the way Pingora's listening service does.
    async fn serve(cfg: TcpConfig) -> std::net::SocketAddr {
        serve_proxy(TcpProxy::new(&cfg)).await
    }

    async fn serve_proxy(proxy: TcpProxy) -> std::net::SocketAddr {
        let proxy = Arc::new(proxy);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (_tx, shutdown) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _tx = _tx;
            loop {
                let Ok((s, peer)) = l.accept().await else {
                    return;
                };
                let (proxy, shutdown) = (Arc::clone(&proxy), shutdown.clone());
                tokio::spawn(async move {
                    let stream = accepted_stream(s, peer);
                    proxy.process_new(stream, &shutdown).await;
                });
            }
        });
        addr
    }

    /// `true` when the proxy relays a round trip on a new connection (kept open in `held`).
    async fn round_trip(addr: std::net::SocketAddr, held: &mut Vec<tokio::net::TcpStream>) -> bool {
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        let _ = c.write_all(b"hi").await;
        let mut buf = [0u8; 2];
        let ok = matches!(
            tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut buf)).await,
            Ok(Ok(_))
        ) && &buf == b"hi";
        if ok {
            held.push(c);
        }
        ok
    }

    #[tokio::test]
    async fn a_connection_over_max_connections_is_closed_and_a_freed_slot_is_reused() {
        let up = echo_upstream().await;
        let addr = serve(TcpConfig {
            targets: vec![up],
            max_connections: Some(2),
            ..Default::default()
        })
        .await;
        let mut held = Vec::new();
        assert!(round_trip(addr, &mut held).await, "first");
        assert!(round_trip(addr, &mut held).await, "second");
        assert!(
            !round_trip(addr, &mut held).await,
            "the third is over the cap and is closed without being relayed"
        );
        held.pop(); // a client goes away: its slot is free again
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(round_trip(addr, &mut held).await, "after a slot is freed");
    }

    // Needs the peer address in the socket digest, which the helper above sets on Unix only.
    #[cfg(unix)]
    #[tokio::test]
    async fn max_connections_per_ip_counts_the_client_address() {
        let up = echo_upstream().await;
        let addr = serve(TcpConfig {
            targets: vec![up],
            max_connections_per_ip: Some(1),
            ..Default::default()
        })
        .await;
        let mut held = Vec::new();
        assert!(round_trip(addr, &mut held).await, "first from 127.0.0.1");
        assert!(
            !round_trip(addr, &mut held).await,
            "a second from the same address is refused"
        );
        held.clear();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(round_trip(addr, &mut held).await, "after the first closed");
    }

    #[tokio::test]
    async fn a_quiet_connection_is_closed_after_idle_timeout_secs() {
        let up = echo_upstream().await;
        let addr = serve(TcpConfig {
            targets: vec![up],
            idle_timeout_secs: Some(1),
            ..Default::default()
        })
        .await;
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        c.read_exact(&mut buf).await.unwrap();
        let started = Instant::now();
        let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
            .await
            .expect("the proxy closes the quiet connection")
            .unwrap_or(0);
        assert_eq!(n, 0, "closed, not data");
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(800) && waited < Duration::from_secs(3),
            "closed after about 1 s, not {waited:?}"
        );
    }

    #[test]
    fn idle_timeout_defaults_to_an_hour_and_zero_means_never() {
        let t = |secs: Option<u64>| {
            TcpProxy::new(&TcpConfig {
                targets: vec!["x:1".to_owned()],
                idle_timeout_secs: secs,
                ..Default::default()
            })
            .idle_timeout
        };
        assert_eq!(t(None), Some(Duration::from_secs(3600)));
        assert_eq!(t(Some(90)), Some(Duration::from_secs(90)));
        assert_eq!(t(Some(0)), None);
    }

    // ── PROXY protocol, through real sockets (#757) ──────────────────────────

    fn pp_cfg(sources: &[&str]) -> ProxyProtocolConfig {
        ProxyProtocolConfig {
            trusted_sources: sources.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        }
    }

    /// Everything the client reads until the proxy closes the connection (EOF, or a reset because it closed with our bytes
    /// unread). A connection still open after `wait` fails the test: a timeout is not a refusal.
    async fn read_all(c: &mut tokio::net::TcpStream, wait: Duration) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match tokio::time::timeout(wait, c.read(&mut buf)).await {
                Err(_) => {
                    panic!("the connection was left open for {wait:?} instead of being closed")
                }
                Ok(Ok(0) | Err(_)) => return out,
                Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_trusted_peers_header_is_consumed_and_the_payload_after_it_is_relayed() {
        let up = echo_upstream().await;
        let addr = serve_proxy(
            TcpProxy::new(&TcpConfig {
                targets: vec![up],
                ..Default::default()
            })
            .with_proxy_protocol(&pp_cfg(&["127.0.0.0/8"])),
        )
        .await;
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Header and payload in one write: the bytes after the header must not be lost.
        c.write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\nhi")
            .await
            .unwrap();
        let mut buf = [0u8; 2];
        tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut buf))
            .await
            .expect("echo")
            .unwrap();
        assert_eq!(&buf, b"hi", "the upstream got the payload, not the header");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn send_proxy_protocol_passes_the_client_named_by_the_header_to_the_upstream() {
        let up = echo_upstream().await;
        let addr = serve_proxy(
            TcpProxy::new(&TcpConfig {
                targets: vec![up],
                send_proxy_protocol: Some(pp::Version::V1),
                ..Default::default()
            })
            .with_proxy_protocol(&pp_cfg(&["127.0.0.1"])),
        )
        .await;
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\nhi")
            .await
            .unwrap();
        let want = b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\nhi";
        let mut got = vec![0u8; want.len()];
        tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut got))
            .await
            .expect("echo")
            .unwrap();
        assert_eq!(got, want, "the echo upstream shows what it was sent");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn send_proxy_protocol_without_accept_names_the_socket_peer() {
        let up = echo_upstream().await;
        let addr = serve(TcpConfig {
            targets: vec![up],
            send_proxy_protocol: Some(pp::Version::V2),
            ..Default::default()
        })
        .await;
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        let me = c.local_addr().unwrap();
        c.write_all(b"hi").await.unwrap();
        let mut got = vec![0u8; 28 + 2];
        tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut got))
            .await
            .expect("echo")
            .unwrap();
        let parsed = pp::parse(&got, 4096).expect("a v2 header first");
        let Header::Proxy {
            source,
            destination,
        } = parsed.header
        else {
            panic!("expected a PROXY header, got {:?}", parsed.header);
        };
        assert_eq!(source, me, "the client is the socket peer");
        assert_eq!(destination, addr, "the destination is the listener");
        assert_eq!(&got[parsed.len..], b"hi");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_untrusted_peer_and_a_trusted_peer_without_a_header_are_closed() {
        let up = echo_upstream().await;
        for sources in [&["192.0.2.0/24"][..], &["127.0.0.0/8"][..]] {
            let addr = serve_proxy(
                TcpProxy::new(&TcpConfig {
                    targets: vec![up.clone()],
                    ..Default::default()
                })
                .with_proxy_protocol(&pp_cfg(sources)),
            )
            .await;
            let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
            let _ = c.write_all(b"GET / HTTP/1.1\r\n\r\n").await;
            let got = read_all(&mut c, Duration::from_secs(2)).await;
            assert!(
                got.is_empty(),
                "{sources:?}: nothing is relayed, got {got:?}"
            );
        }
        // a trusted peer that sends a header with the real thing after it is fine
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_trusted_peer_that_never_finishes_the_header_is_closed_and_frees_its_slot() {
        let up = echo_upstream().await;
        let addr = serve_proxy(
            TcpProxy::new(&TcpConfig {
                targets: vec![up],
                max_connections: Some(1),
                ..Default::default()
            })
            .with_proxy_protocol(&ProxyProtocolConfig {
                trusted_sources: vec!["127.0.0.0/8".into()],
                timeout_ms: Some(300),
                ..Default::default()
            }),
        )
        .await;
        let mut slow = tokio::net::TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"PROXY TCP4 1.2.3.4 ").await.unwrap(); // and then nothing
        let started = Instant::now();
        let got = read_all(&mut slow, Duration::from_secs(3)).await;
        assert!(got.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "closed at the deadline, not {:?}",
            started.elapsed()
        );
        // the single slot is free again
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(b"PROXY UNKNOWN\r\nhi").await.unwrap();
        let mut buf = [0u8; 2];
        tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut buf))
            .await
            .expect("echo")
            .unwrap();
        assert_eq!(&buf, b"hi");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn max_connections_per_ip_counts_the_client_the_header_names() {
        let up = echo_upstream().await;
        let addr = serve_proxy(
            TcpProxy::new(&TcpConfig {
                targets: vec![up],
                max_connections_per_ip: Some(1),
                ..Default::default()
            })
            .with_proxy_protocol(&pp_cfg(&["127.0.0.0/8"])),
        )
        .await;
        let open = |client: &'static str| async move {
            let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
            c.write_all(format!("PROXY TCP4 {client} 192.0.2.1 1000 443\r\nhi").as_bytes())
                .await
                .unwrap();
            let mut buf = [0u8; 2];
            let ok = matches!(
                tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut buf)).await,
                Ok(Ok(_))
            );
            (ok, c)
        };
        // All three arrive from 127.0.0.1; only the named client counts.
        let (a, _a) = open("203.0.113.1").await;
        let (a2, _a2) = open("203.0.113.1").await;
        let (b, _b) = open("203.0.113.2").await;
        assert!(a, "first from 203.0.113.1");
        assert!(!a2, "a second from the same named client is over the cap");
        assert!(b, "another named client is not affected");
    }
}
