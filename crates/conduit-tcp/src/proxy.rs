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
//! ```
//!
//! A connect error or timeout is not the end of the connection: the remaining targets are tried once each, in
//! order from the one picked, and a target that just failed is left out of the pick for [`DOWN_FOR`] so that a dead
//! one does not cost a connect timeout per connection (#725; nginx `proxy_next_upstream` + `fail_timeout`).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pingora_core::apps::ServerApp;
use pingora_core::protocols::Stream;
use pingora_core::server::ShutdownWatch;
use tokio::net::TcpStream;

use crate::config::TcpConfig;

/// How long a target that failed to connect is passed over while another one is available.
const DOWN_FOR: Duration = Duration::from_secs(5);

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
        }
    }

    fn now_ms(&self) -> u64 {
        // +1 so that a mark made at the very first millisecond is not the "not down" value 0.
        self.epoch.elapsed().as_millis() as u64 + 1
    }

    fn is_down(&self, idx: usize) -> bool {
        self.down_until_ms[idx].load(Ordering::Relaxed) > self.now_ms()
    }

    fn mark_down(&self, idx: usize) {
        let until = self.now_ms() + DOWN_FOR.as_millis() as u64;
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
        let Some((mut upstream, target)) = self.connect_any().await else {
            tracing::warn!("TCP proxy: no upstream target could be reached");
            return None;
        };

        tracing::debug!(target, "TCP proxy: relaying connection");

        match tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await {
            Ok((down_bytes, up_bytes)) => {
                tracing::debug!(
                    target,
                    bytes_client_to_upstream = down_bytes,
                    bytes_upstream_to_client = up_bytes,
                    "TCP proxy: connection closed"
                );
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
            connect_timeout_ms: None,
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
            strategy: None,
            connect_timeout_ms: Some(2000),
        });
        assert_eq!(proxy.connect_timeout, Duration::from_secs(2));
    }
}
