//! Upstream health tracking and background health check tasks.
//!
//! The [`UpstreamRegistry`] lives in [`AppState`] and is shared by the router
//! (to filter unhealthy upstreams and to count inflight connections for
//! least-conn balancing) and the admin API (to report status).
//!
//! Background health check tasks are spawned once at server start inside
//! [`AdminApiService::start()`] via [`spawn_health_checks`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::schema::{AppConfig, ProxyConfig, ProxyRouteTarget};
use crate::proxy::upstream;

// ── Per-upstream health state ─────────────────────────────────────────────────

/// Live health state for a single upstream URL.
///
/// Updated atomically by the background probe task; read lock-free by the router.
#[derive(Debug, Clone)]
pub struct UpstreamEntry {
    /// `true` while the upstream is considered available.
    ///
    /// Defaults to `true` until the first probe runs so that new upstreams
    /// receive traffic immediately without a warm-up delay.
    pub healthy: bool,
    /// Consecutive probe failures without a success in between.
    pub consecutive_failures: u32,
    /// Consecutive probe successes without a failure in between.
    pub consecutive_successes: u32,
    /// Round-trip latency of the most recent successful probe, in milliseconds.
    pub latency_ms: Option<u64>,
}

impl Default for UpstreamEntry {
    fn default() -> Self {
        Self {
            healthy: true, // optimistic: assume healthy until a probe says otherwise
            consecutive_failures: 0,
            consecutive_successes: 0,
            latency_ms: None,
        }
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Central store for upstream health state and per-upstream connection counts.
pub struct UpstreamRegistry {
    /// Per-upstream health state, keyed by the full upstream URL string.
    pub statuses: DashMap<String, UpstreamEntry>,
    /// Per-upstream inflight connection count, used by the `least-conn` strategy.
    ///
    /// Incremented in the router when a URL is selected; decremented in the
    /// `logging()` hook after the response is sent.
    pub conn_count: DashMap<String, AtomicUsize>,
}

impl Default for UpstreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl UpstreamRegistry {
    pub fn new() -> Self {
        Self {
            statuses: DashMap::new(),
            conn_count: DashMap::new(),
        }
    }

    // ── Least-conn helpers ────────────────────────────────────────────────────

    /// Return the current inflight count for `url` (0 when unknown).
    pub fn conn_load(&self, url: &str) -> usize {
        self.conn_count
            .get(url)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Increment the inflight count for `url` and return the new value.
    pub fn conn_inc(&self, url: &str) {
        self.conn_count
            .entry(url.to_owned())
            .or_insert_with(|| AtomicUsize::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the inflight count for `url`, saturating at 0.
    pub fn conn_dec(&self, url: &str) {
        if let Some(c) = self.conn_count.get(url) {
            // fetch_update lets us implement saturating decrement atomically.
            let _ = c.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
        }
    }

    /// Pick the URL from `urls` with the lowest current inflight count and
    /// increment its count.
    ///
    /// Falls back to the first URL when the slice is empty.
    pub fn pick_least_conn(&self, urls: &[String]) -> Option<String> {
        let chosen = urls.iter().min_by_key(|url| self.conn_load(url))?.clone();
        self.conn_inc(&chosen);
        Some(chosen)
    }

    // ── Health helpers ────────────────────────────────────────────────────────

    /// Return `true` if `url` is currently considered healthy.
    ///
    /// Defaults to `true` when the URL has not been seen by a probe yet
    /// (optimistic assumption so traffic flows before the first check).
    pub fn is_healthy(&self, url: &str) -> bool {
        self.statuses.get(url).map(|e| e.healthy).unwrap_or(true)
    }

    /// Filter `urls` to only healthy ones.
    ///
    /// If all upstreams are down the original list is returned unchanged so
    /// the proxy continues to try rather than hard-failing.
    pub fn filter_healthy<'a>(&self, urls: &'a [String]) -> Vec<&'a String> {
        let healthy: Vec<&String> = urls.iter().filter(|u| self.is_healthy(u)).collect();
        if healthy.is_empty() {
            urls.iter().collect()
        } else {
            healthy
        }
    }
}

// ── Background health check tasks ─────────────────────────────────────────────

/// Spawn a Tokio background task for every proxy route that has
/// `healthCheck` configured.
///
/// Call once after the Tokio runtime is available (e.g. from
/// `AdminApiService::start()`).  Tasks are fire-and-forget; they run until
/// the process exits.
pub fn spawn_health_checks(registry: Arc<UpstreamRegistry>, config: &AppConfig) {
    for site in &config.sites {
        let Some(proxy) = &site.proxy else {
            continue;
        };
        let routes = match proxy {
            ProxyConfig::Routes(r) => r,
            ProxyConfig::Single(_) => continue,
        };

        for route_target in routes.values() {
            let ProxyRouteTarget::Full(cfg) = route_target else {
                continue;
            };
            let Some(hc) = &cfg.health_check else {
                continue;
            };

            let urls = upstream::target_urls(route_target);
            if urls.is_empty() {
                continue;
            }

            let path = hc.path.clone().unwrap_or_else(|| "/".to_string());
            let interval_secs = hc.interval_secs.unwrap_or(10).max(1);
            let unhealthy_threshold = hc.unhealthy_threshold.unwrap_or(3);
            let healthy_threshold = hc.healthy_threshold.unwrap_or(1);
            let reg = registry.clone();

            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    ticker.tick().await;
                    for url in &urls {
                        let Some(host_port) = upstream::url_to_host_port(url) else {
                            continue;
                        };
                        let (ok, latency_ms) = probe_http(&host_port, &path).await;

                        let mut entry = reg.statuses.entry(url.clone()).or_default();

                        if ok {
                            entry.consecutive_failures = 0;
                            entry.consecutive_successes =
                                entry.consecutive_successes.saturating_add(1);
                            entry.latency_ms = Some(latency_ms);
                            if entry.consecutive_successes >= healthy_threshold {
                                if !entry.healthy {
                                    tracing::info!(url, "upstream recovered");
                                }
                                entry.healthy = true;
                            }
                        } else {
                            entry.consecutive_successes = 0;
                            entry.consecutive_failures =
                                entry.consecutive_failures.saturating_add(1);
                            if entry.consecutive_failures >= unhealthy_threshold {
                                if entry.healthy {
                                    tracing::warn!(
                                        url,
                                        failures = entry.consecutive_failures,
                                        "upstream marked unhealthy"
                                    );
                                }
                                entry.healthy = false;
                            }
                        }
                    }
                }
            });
        }
    }
}

// ── HTTP health probe ─────────────────────────────────────────────────────────

/// Send an HTTP/1.1 HEAD request to `host_port` at `path`.
///
/// Returns `(success, latency_ms)`.  `success` is `true` when the server
/// responds with any HTTP status below 500.  Connection or I/O errors return
/// `(false, 0)`.
///
/// A 5-second timeout is applied to both the TCP connect and the read.
async fn probe_http(host_port: &str, path: &str) -> (bool, u64) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(5);

    let stream =
        match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(host_port)).await {
            Ok(Ok(s)) => s,
            _ => return (false, 0),
        };

    let req = format!("HEAD {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");

    let mut reader = BufReader::new(stream);

    let write_result =
        tokio::time::timeout(timeout, reader.get_mut().write_all(req.as_bytes())).await;
    if write_result.is_err() || write_result.unwrap().is_err() {
        return (false, 0);
    }

    let mut line = String::new();
    let read_result = tokio::time::timeout(timeout, reader.read_line(&mut line)).await;
    if read_result.is_err() || line.is_empty() {
        return (false, 0);
    }

    // Status line: "HTTP/1.1 200 OK" — parse the code.
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let latency_ms = start.elapsed().as_millis() as u64;
    (status > 0 && status < 500, latency_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conn_inc_dec_saturates() {
        let reg = UpstreamRegistry::new();
        let url = "http://localhost:4000";

        assert_eq!(reg.conn_load(url), 0);
        reg.conn_inc(url);
        reg.conn_inc(url);
        assert_eq!(reg.conn_load(url), 2);
        reg.conn_dec(url);
        assert_eq!(reg.conn_load(url), 1);
        reg.conn_dec(url);
        reg.conn_dec(url); // extra dec — must not underflow
        assert_eq!(reg.conn_load(url), 0);
    }

    #[test]
    fn pick_least_conn_selects_minimum() {
        let reg = UpstreamRegistry::new();
        let urls = vec![
            "http://a:4000".to_string(),
            "http://b:4000".to_string(),
            "http://c:4000".to_string(),
        ];

        // Pre-seed two of them.
        reg.conn_inc("http://a:4000");
        reg.conn_inc("http://a:4000");
        reg.conn_inc("http://b:4000");

        // c has 0 — should be chosen and its count incremented.
        let chosen = reg.pick_least_conn(&urls).unwrap();
        assert_eq!(chosen, "http://c:4000");
        assert_eq!(reg.conn_load("http://c:4000"), 1);
    }

    #[test]
    fn filter_healthy_returns_all_when_all_down() {
        let reg = UpstreamRegistry::new();
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];

        // Mark both as unhealthy.
        reg.statuses.insert(
            "http://a:4000".to_string(),
            UpstreamEntry {
                healthy: false,
                ..Default::default()
            },
        );
        reg.statuses.insert(
            "http://b:4000".to_string(),
            UpstreamEntry {
                healthy: false,
                ..Default::default()
            },
        );

        // filter_healthy must return all when all are down (fail-open).
        let result = reg.filter_healthy(&urls);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_healthy_excludes_down_upstreams() {
        let reg = UpstreamRegistry::new();
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];

        reg.statuses.insert(
            "http://a:4000".to_string(),
            UpstreamEntry {
                healthy: false,
                ..Default::default()
            },
        );
        // b is unknown — defaults to healthy

        let result = reg.filter_healthy(&urls);
        assert_eq!(result.len(), 1);
        assert_eq!(*result[0], "http://b:4000");
    }
}
