#![cfg(feature = "tcp")]
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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pingora_core::apps::ServerApp;
use pingora_core::protocols::Stream;
use pingora_core::server::ShutdownWatch;
use tokio::net::TcpStream;

use crate::config::schema::TcpConfig;

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
        }
    }

    /// Pick a target using round-robin or random selection.
    fn pick_target(&self) -> Option<&str> {
        let n = self.targets.len();
        if n == 0 {
            return None;
        }
        let idx = if self.random {
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
        };
        Some(&self.targets[idx])
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
        let target = match self.pick_target() {
            Some(t) => t.to_owned(),
            None => {
                tracing::warn!("TCP proxy: no upstream targets configured");
                return None;
            }
        };

        let upstream_result =
            tokio::time::timeout(self.connect_timeout, TcpStream::connect(&target)).await;

        let mut upstream = match upstream_result {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                tracing::warn!(target, "TCP proxy: upstream connect error: {e}");
                return None;
            }
            Err(_) => {
                tracing::warn!(
                    target,
                    timeout_ms = self.connect_timeout.as_millis(),
                    "TCP proxy: upstream connect timed out"
                );
                return None;
            }
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

    #[test]
    fn custom_connect_timeout() {
        use crate::config::schema::TcpConfig;
        let proxy = TcpProxy::new(&TcpConfig {
            targets: vec!["x:1".to_owned()],
            strategy: None,
            connect_timeout_ms: Some(2000),
        });
        assert_eq!(proxy.connect_timeout, Duration::from_secs(2));
    }
}
