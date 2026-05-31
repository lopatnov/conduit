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
    /// Timestamp (seconds since UNIX epoch) when this upstream was last marked
    /// healthy after recovering from an unhealthy state.  `None` means the
    /// upstream has been healthy since it was first seen.
    ///
    /// Used by slow-start: during the ramp-up window, the upstream's effective
    /// weight is scaled from 0 to 100 % proportionally to elapsed time.
    pub recovery_time_secs: Option<u64>,
}

impl Default for UpstreamEntry {
    fn default() -> Self {
        Self {
            healthy: true, // optimistic: assume healthy until a probe says otherwise
            consecutive_failures: 0,
            consecutive_successes: 0,
            latency_ms: None,
            recovery_time_secs: None,
        }
    }
}

/// Compute the slow-start traffic fraction for an upstream.
///
/// Returns a value in `[0.0, 1.0]`:
/// - `1.0` when slow-start is disabled or the ramp window has elapsed.
/// - A value proportional to `elapsed / window_secs` during ramp-up.
///
/// Callers should multiply their selection probability by this value.
pub fn slow_start_fraction(entry: &UpstreamEntry, window_secs: u64) -> f64 {
    if window_secs == 0 {
        return 1.0;
    }
    let Some(recovery) = entry.recovery_time_secs else {
        return 1.0; // no recovery time recorded → fully ramped
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let elapsed = now.saturating_sub(recovery);
    if elapsed >= window_secs {
        1.0
    } else {
        elapsed as f64 / window_secs as f64
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
    /// Runtime upstream overrides: `route_path → [(url, weight)]`.
    ///
    /// When present for a route, these targets are used **instead of** the
    /// config-file targets.  An explicit empty vec means all targets have been
    /// removed.  `None` (key absent) means "use config" (no override).
    ///
    /// Mutated by `POST /upstreams/add|remove|weight` and cleared by
    /// `POST /reload` so that the config file is the single source of truth
    /// after a reload.
    pub overrides: DashMap<String, Vec<(String, u32)>>,
}

impl Default for UpstreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Override key helpers ──────────────────────────────────────────────────────

/// Build the DashMap key for a runtime override entry.
///
/// Format: `"{site}\0{route}"`.  The null byte is a safe separator because
/// neither host labels nor URL paths can contain `\0`.
/// Site `"*"` is the wildcard — applies to every site that serves the route.
fn override_key(site: &str, route: &str) -> String {
    format!("{site}\0{route}")
}

/// Format a site's host+port as the canonical site label used in override keys.
///
/// Mirrors the display logic in the admin API so that labels are consistent
/// across the router, the registry, and the CLI.
pub fn site_label(host: &Option<String>, port: Option<u16>) -> String {
    match (host, port) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.clone(),
        (None, Some(p)) => format!("*:{p}"),
        (None, None) => "*".to_string(),
    }
}

impl UpstreamRegistry {
    pub fn new() -> Self {
        Self {
            statuses: DashMap::new(),
            conn_count: DashMap::new(),
            overrides: DashMap::new(),
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
    /// Returns `None` when `urls` is empty.
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

    // ── Runtime upstream overrides (Phase 2.5c) ───────────────────────────────

    /// Return the runtime override target list for `(site_label, route)`.
    ///
    /// Lookup order:
    /// 1. Site-specific key — `"{site_label}\0{route}"` (set via `--site` flag).
    /// 2. Wildcard key — `"*\0{route}"` (set when no `--site` is specified).
    /// 3. `None` — fall back to the config-file targets.
    pub fn get_override_targets(
        &self,
        site_label: &str,
        route: &str,
    ) -> Option<Vec<(String, u32)>> {
        // Site-specific takes precedence.
        let specific = override_key(site_label, route);
        if let Some(v) = self.overrides.get(&specific) {
            return Some(v.value().clone());
        }
        // Wildcard — applies to all sites.
        self.overrides
            .get(&override_key("*", route))
            .map(|e| e.value().clone())
    }

    /// Add `url` with `weight` to the runtime override list for `(site, route)`.
    ///
    /// `site` is either a site label (e.g. `"app.example.com:443"`) for a
    /// site-specific override, or `"*"` to apply the override to every site
    /// that serves this route (wildcard, backward-compatible default).
    ///
    /// If `url` is already present its weight is updated.
    pub fn add_upstream(&self, site: &str, route: &str, url: &str, weight: u32) {
        let key = override_key(site, route);
        let mut entry = self.overrides.entry(key).or_default();
        if let Some(existing) = entry.iter_mut().find(|(u, _)| u == url) {
            existing.1 = weight;
        } else {
            entry.push((url.to_owned(), weight));
        }
    }

    /// Remove `url` from the runtime override list for `(site, route)`.
    ///
    /// Returns `true` if the URL was found and removed.
    pub fn remove_upstream(&self, site: &str, route: &str, url: &str) -> bool {
        let key = override_key(site, route);
        if let Some(mut entry) = self.overrides.get_mut(&key) {
            let before = entry.len();
            entry.retain(|(u, _)| u != url);
            return entry.len() < before;
        }
        false
    }

    /// Update the weight of `url` within the runtime override list for `(site, route)`.
    ///
    /// Returns `true` when the URL was found and its weight updated.
    pub fn set_weight(&self, site: &str, route: &str, url: &str, weight: u32) -> bool {
        let key = override_key(site, route);
        if let Some(mut entry) = self.overrides.get_mut(&key) {
            if let Some(existing) = entry.iter_mut().find(|(u, _)| u == url) {
                existing.1 = weight;
                return true;
            }
        }
        false
    }

    /// Drop all runtime overrides (called on `POST /reload`).
    pub fn clear_overrides(&self) {
        self.overrides.clear();
    }
}

// ── Health state update ───────────────────────────────────────────────────────

/// Apply a single probe result to an upstream's health entry.
///
/// Called from the background health-check task after each TCP probe.  Extracted
/// into a pure function so the threshold logic can be unit-tested without a live
/// network connection or a running Tokio runtime.
pub(crate) fn apply_probe_result(
    entry: &mut UpstreamEntry,
    ok: bool,
    latency_ms: u64,
    healthy_threshold: u32,
    unhealthy_threshold: u32,
) {
    if ok {
        entry.consecutive_failures = 0;
        entry.consecutive_successes = entry.consecutive_successes.saturating_add(1);
        entry.latency_ms = Some(latency_ms);
        if entry.consecutive_successes >= healthy_threshold {
            entry.healthy = true;
        }
    } else {
        entry.consecutive_successes = 0;
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        if entry.consecutive_failures >= unhealthy_threshold {
            entry.healthy = false;
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
        let Some(ProxyConfig::Routes(routes)) = &site.proxy else {
            continue;
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
            let raw_path = hc.path.clone().unwrap_or_else(|| "/".to_string());
            let path = if raw_path.starts_with('/') {
                raw_path
            } else {
                format!("/{raw_path}")
            };
            spawn_health_task(
                registry.clone(),
                urls,
                path,
                hc.interval_secs.unwrap_or(10).max(1),
                hc.healthy_threshold.unwrap_or(1),
                hc.unhealthy_threshold.unwrap_or(3),
            );
        }
    }
}

/// Spawn a single background health-check task for a set of upstream URLs.
fn spawn_health_task(
    registry: Arc<UpstreamRegistry>,
    urls: Vec<String>,
    path: String,
    interval_secs: u64,
    healthy_threshold: u32,
    unhealthy_threshold: u32,
) {
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
                let mut entry = registry.statuses.entry(url.clone()).or_default();
                let was_healthy = entry.healthy;
                apply_probe_result(
                    &mut entry,
                    ok,
                    latency_ms,
                    healthy_threshold,
                    unhealthy_threshold,
                );
                if ok && !was_healthy && entry.healthy {
                    // Record recovery time for slow-start weight ramping.
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    entry.recovery_time_secs = Some(now);
                    tracing::info!(url, "upstream recovered");
                } else if !ok && was_healthy && !entry.healthy {
                    tracing::warn!(
                        url,
                        failures = entry.consecutive_failures,
                        "upstream marked unhealthy"
                    );
                }
            }
        }
    });
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

    // ── apply_probe_result ────────────────────────────────────────────────────

    fn fresh() -> UpstreamEntry {
        UpstreamEntry::default()
    }

    #[test]
    fn probe_success_increments_successes_and_records_latency() {
        let mut e = fresh();
        apply_probe_result(&mut e, true, 42, 2, 3);
        assert_eq!(e.consecutive_successes, 1);
        assert_eq!(e.consecutive_failures, 0);
        assert_eq!(e.latency_ms, Some(42));
        assert!(e.healthy, "should stay healthy after first success");
    }

    #[test]
    fn probe_enough_successes_marks_healthy() {
        let mut e = UpstreamEntry {
            healthy: false,
            consecutive_failures: 5,
            consecutive_successes: 0,
            latency_ms: None,
            recovery_time_secs: None,
        };
        apply_probe_result(&mut e, true, 10, 2, 3);
        assert!(!e.healthy, "one success is not enough (threshold = 2)");
        apply_probe_result(&mut e, true, 10, 2, 3);
        assert!(e.healthy, "two successes should restore health");
        assert_eq!(e.consecutive_failures, 0);
    }

    #[test]
    fn probe_failure_increments_failures_and_clears_successes() {
        let mut e = fresh();
        apply_probe_result(&mut e, false, 0, 1, 3);
        assert_eq!(e.consecutive_failures, 1);
        assert_eq!(e.consecutive_successes, 0);
        assert!(e.healthy, "one failure is not enough (threshold = 3)");
    }

    #[test]
    fn probe_enough_failures_marks_unhealthy() {
        let mut e = fresh();
        apply_probe_result(&mut e, false, 0, 1, 3);
        apply_probe_result(&mut e, false, 0, 1, 3);
        assert!(e.healthy, "two failures, threshold = 3 → still healthy");
        apply_probe_result(&mut e, false, 0, 1, 3);
        assert!(!e.healthy, "three failures → unhealthy");
    }

    #[test]
    fn probe_success_after_failure_resets_failure_counter() {
        let mut e = UpstreamEntry {
            healthy: true,
            consecutive_failures: 2,
            consecutive_successes: 0,
            latency_ms: None,
            recovery_time_secs: None,
        };
        apply_probe_result(&mut e, true, 5, 1, 3);
        assert_eq!(e.consecutive_failures, 0);
        assert!(e.healthy);
    }

    // ── probe_http ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn probe_http_succeeds_on_200() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 256];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });

        let (ok, latency_ms) = probe_http(&format!("127.0.0.1:{port}"), "/").await;
        assert!(ok, "probe should succeed on HTTP 200");
        assert!(latency_ms < 5000);
    }

    #[tokio::test]
    async fn probe_http_fails_on_500() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 256];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
            }
        });

        let (ok, _) = probe_http(&format!("127.0.0.1:{port}"), "/").await;
        assert!(!ok, "probe should fail on HTTP 500");
    }

    #[tokio::test]
    async fn probe_http_fails_when_nothing_listening() {
        // Bind then drop so the port is released before we call probe_http.
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let (ok, latency_ms) = probe_http(&format!("127.0.0.1:{port}"), "/").await;
        assert!(!ok, "probe should fail when nothing is listening");
        assert_eq!(latency_ms, 0);
    }

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

    // ── override management ───────────────────────────────────────────────────

    #[test]
    fn add_upstream_creates_override_and_updates_weight() {
        let reg = UpstreamRegistry::new();

        // No override yet → None
        assert!(reg.get_override_targets("*", "/api").is_none());

        reg.add_upstream("*", "/api", "http://a:4000", 1);
        let ov = reg.get_override_targets("*", "/api").unwrap();
        assert_eq!(ov, vec![("http://a:4000".to_string(), 1)]);

        // Add a second target.
        reg.add_upstream("*", "/api", "http://b:4000", 2);
        let ov = reg.get_override_targets("*", "/api").unwrap();
        assert_eq!(ov.len(), 2);

        // Update weight of existing target.
        reg.add_upstream("*", "/api", "http://a:4000", 5);
        let ov = reg.get_override_targets("*", "/api").unwrap();
        let a_weight = ov
            .iter()
            .find(|(u, _)| u == "http://a:4000")
            .map(|(_, w)| *w);
        assert_eq!(a_weight, Some(5), "weight must be updated in place");
        assert_eq!(ov.len(), 2, "no duplicate entries on weight update");
    }

    #[test]
    fn remove_upstream_returns_true_when_found() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/api", "http://a:4000", 1);
        reg.add_upstream("*", "/api", "http://b:4000", 1);

        let removed = reg.remove_upstream("*", "/api", "http://a:4000");
        assert!(removed, "remove should return true when URL was present");

        let ov = reg.get_override_targets("*", "/api").unwrap();
        assert_eq!(ov.len(), 1);
        assert_eq!(ov[0].0, "http://b:4000");
    }

    #[test]
    fn remove_upstream_returns_false_when_missing() {
        let reg = UpstreamRegistry::new();
        // No override list at all.
        assert!(!reg.remove_upstream("*", "/api", "http://x:4000"));

        // Override list exists but URL not in it.
        reg.add_upstream("*", "/api", "http://a:4000", 1);
        assert!(!reg.remove_upstream("*", "/api", "http://x:4000"));
    }

    #[test]
    fn remove_all_leaves_empty_override_list() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/", "http://a:4000", 1);
        reg.remove_upstream("*", "/", "http://a:4000");

        // Key must still exist (empty list) so callers know there IS an override.
        let ov = reg.get_override_targets("*", "/");
        assert!(ov.is_some(), "empty override list must still be present");
        assert!(ov.unwrap().is_empty());
    }

    #[test]
    fn set_weight_updates_existing_target() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/api", "http://a:4000", 1);

        let updated = reg.set_weight("*", "/api", "http://a:4000", 10);
        assert!(updated, "set_weight must return true for known URL");

        let ov = reg.get_override_targets("*", "/api").unwrap();
        assert_eq!(ov[0].1, 10);
    }

    #[test]
    fn set_weight_returns_false_when_not_found() {
        let reg = UpstreamRegistry::new();
        // Route not in overrides at all.
        assert!(!reg.set_weight("*", "/api", "http://a:4000", 5));

        // Route exists but URL not in it.
        reg.add_upstream("*", "/api", "http://b:4000", 1);
        assert!(!reg.set_weight("*", "/api", "http://a:4000", 5));
    }

    #[test]
    fn clear_overrides_removes_all_routes() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/api", "http://a:4000", 1);
        reg.add_upstream("*", "/web", "http://b:4000", 1);
        assert_eq!(reg.overrides.len(), 2);

        reg.clear_overrides();
        assert!(reg.overrides.is_empty());
        assert!(reg.get_override_targets("*", "/api").is_none());
    }
}
