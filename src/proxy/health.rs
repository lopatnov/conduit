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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the current Unix timestamp as whole seconds.
///
/// Used throughout the health module for ejection/recovery time comparisons.
/// Centralised here to avoid repeating the same `SystemTime::now()…` chain.
#[inline]
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Return the current Unix timestamp as fractional seconds (for EWMA decay).
#[inline]
fn now_secs_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

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
    /// Peak Exponentially-Weighted Moving Average latency in microseconds.
    ///
    /// Updated on every completed proxy request.  Uses a **time-based decay**
    /// with a 10-second time constant (matching linkerd2-proxy's default):
    ///
    ///   new_ewma = old_ewma × e^(-Δt / 10s)  +  α × sample
    ///
    /// where Δt is the wall-clock time since the last sample.  This means
    /// stale measurements naturally decay between traffic bursts rather than
    /// persisting until the next request arrives.
    ///
    /// Default initial value: 30 000 µs (30 ms) — a realistic RPC latency,
    /// matching linkerd's `DEFAULT_RTT = 30 ms`.  New upstreams therefore
    /// receive fair load sooner than the previous 1-second conservative default.
    ///
    /// Used by `LeastResponseTime` and `P2c` to select the fastest backend.
    pub ewma_latency_us: f64,
    /// Wall-clock time (seconds since UNIX epoch) of the last EWMA sample.
    ///
    /// Used for time-based decay: when a new sample arrives, the old EWMA is
    /// multiplied by `e^(-elapsed / DECAY_SECS)` before mixing in the new value.
    pub ewma_last_sample_secs: f64,
    /// Consecutive 5xx responses from actual proxy traffic (passive health check).
    /// Resets to 0 on any non-5xx response.
    pub consecutive_5xx: u32,
    /// Unix timestamp (secs) until which this upstream is temporarily ejected.
    /// `None` = not ejected.  Set by Outlier Detection.
    pub ejected_until_secs: Option<u64>,
    /// Number of times this upstream has been ejected.  Used for backoff:
    /// ejection duration = `base_time × 2^ejection_count`.
    pub ejection_count: u32,
    /// Half-open circuit breaker state.
    ///
    /// When an ejection period expires, the first request that checks
    /// `is_healthy()` sets `half_open = true` and is allowed through as a
    /// "probe" request.  All subsequent requests are blocked (`is_healthy`
    /// returns `false`) until the probe completes:
    ///
    /// - Probe succeeds (non-5xx): `half_open` is cleared, ejection is lifted,
    ///   `ejection_count` is reset — full traffic resumes immediately.
    /// - Probe fails (5xx): `half_open` is cleared, upstream is re-ejected with
    ///   the next exponential-backoff duration.
    pub half_open: bool,
}

impl Default for UpstreamEntry {
    fn default() -> Self {
        Self {
            healthy: true,
            consecutive_failures: 0,
            consecutive_successes: 0,
            latency_ms: None,
            recovery_time_secs: None,
            ewma_latency_us: 30_000.0, // default: 30 ms (matches linkerd DEFAULT_RTT)
            ewma_last_sample_secs: 0.0, // will be set on first sample
            consecutive_5xx: 0,
            ejected_until_secs: None,
            ejection_count: 0,
            half_open: false,
        }
    }
}

/// EWMA time-decay constant: 10 seconds (matches linkerd2-proxy default).
///
/// Measurements older than this contribute less than `1/e ≈ 37%` of their
/// original weight.  Prevents stale latency readings from biasing load decisions
/// after a traffic pause.
const EWMA_DECAY_SECS: f64 = 10.0;

/// EWMA mixing factor for new samples (α = 0.1 → ~10-sample effective window).
const EWMA_ALPHA: f64 = 0.1;

/// Update the Peak EWMA latency for `url` after completing a proxy request.
///
/// Uses **time-based exponential decay** before mixing in the new sample:
///
///   decayed  = old_ewma × e^(-Δt / DECAY_SECS)
///   new_ewma = decayed + α × sample
///
/// This matches linkerd2-proxy's `PeakEwma` approach: measurements automatically
/// decay between traffic bursts so old slow periods don't bias future routing.
pub fn record_request_latency(
    registry: &UpstreamRegistry,
    url: &str,
    elapsed_us: u64,
    status: u16,
) {
    let ts_f64 = now_secs_f64();

    let mut entry = registry.statuses.entry(url.to_owned()).or_default();

    // Apply time-based decay since the last sample.
    let delta = if entry.ewma_last_sample_secs > 0.0 {
        (ts_f64 - entry.ewma_last_sample_secs).max(0.0)
    } else {
        0.0 // first sample — no decay
    };
    let decay = (-delta / EWMA_DECAY_SECS).exp();
    let decayed = entry.ewma_latency_us * decay;

    // Mix in the new sample.
    entry.ewma_latency_us = decayed + EWMA_ALPHA * elapsed_us as f64;
    entry.ewma_last_sample_secs = ts_f64;

    // Passive health: track consecutive 5xx responses.
    if status >= 500 {
        entry.consecutive_5xx = entry.consecutive_5xx.saturating_add(1);
    } else {
        entry.consecutive_5xx = 0;
    }

    // Half-open circuit breaker resolution.
    if entry.half_open {
        entry.half_open = false;
        if status < 500 && status != 0 {
            // Probe succeeded: lift the ejection and reset the counter so the
            // next ejection cycle starts fresh.
            entry.ejected_until_secs = None;
            entry.ejection_count = 0;
            tracing::info!(url, "half-open probe succeeded — upstream fully recovered");
        } else {
            // Probe failed: re-eject with the next exponential-backoff level.
            const BASE_SECS: u64 = 30;
            const MAX_SECS: u64 = 300;
            let duration = (BASE_SECS * (1u64 << entry.ejection_count.min(10))).min(MAX_SECS);
            let now = now_secs();
            entry.ejected_until_secs = Some(now + duration);
            entry.ejection_count = entry.ejection_count.saturating_add(1);
            tracing::warn!(
                url,
                ejected_for_secs = duration,
                "half-open probe failed — re-ejecting upstream"
            );
        }
    }
}

/// Compute the slow-start traffic fraction for an upstream.
///
/// Check whether `url` should be ejected based on its consecutive 5xx count.
///
/// If the threshold is reached the upstream is ejected for
/// `base_time × 2^ejection_count` seconds, capped at `max_ejection_time_secs`.
/// At most `max_ejection_percent` % of the tracked upstreams may be ejected at once.
pub fn maybe_eject(
    registry: &UpstreamRegistry,
    url: &str,
    cfg: &crate::config::schema::OutlierDetectionConfig,
) {
    let threshold = cfg.consecutive_5xx.unwrap_or(5);
    let base_secs = cfg.base_ejection_time_secs.unwrap_or(30);
    let max_secs = cfg.max_ejection_time_secs.unwrap_or(300);
    let max_pct = cfg.max_ejection_percent.unwrap_or(10) as usize;

    let should_eject = registry
        .statuses
        .get(url)
        .is_some_and(|e| e.consecutive_5xx >= threshold && e.ejected_until_secs.is_none());

    if !should_eject {
        return;
    }

    // Count currently-ejected upstreams to enforce max_ejection_percent.
    let total = registry.statuses.len().max(1);
    let max_ejected = (total * max_pct / 100).max(1);
    let now = now_secs();
    let currently_ejected = registry
        .statuses
        .iter()
        .filter(|e| e.ejected_until_secs.is_some_and(|u| u > now))
        .count();
    if currently_ejected >= max_ejected {
        tracing::debug!(
            url,
            "outlier ejection skipped: max_ejection_percent reached"
        );
        return;
    }

    // Compute backoff duration.
    let count = registry
        .statuses
        .get(url)
        .map(|e| e.ejection_count)
        .unwrap_or(0);
    let duration = (base_secs * (1u64 << count.min(10))).min(max_secs);
    let until = now + duration;

    registry.statuses.entry(url.to_owned()).and_modify(|e| {
        e.ejected_until_secs = Some(until);
        e.ejection_count = e.ejection_count.saturating_add(1);
        e.consecutive_5xx = 0;
    });

    tracing::warn!(
        url,
        ejected_for_secs = duration,
        count,
        "upstream ejected by outlier detection"
    );
}

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
    let now = now_secs();
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

    /// Atomically increment the inflight count for `url` **only if** the current
    /// count is strictly less than `max`.
    ///
    /// Returns `true` when the slot was acquired, `false` when the upstream is
    /// already at or over the limit (no increment performed).
    pub fn conn_inc_if_below(&self, url: &str, max: u64) -> bool {
        use std::sync::atomic::Ordering::{Relaxed, SeqCst};
        let counter = self
            .conn_count
            .entry(url.to_owned())
            .or_insert_with(|| AtomicUsize::new(0));
        // Spin CAS: try to increment only while value < max.
        loop {
            let current = counter.load(Relaxed);
            if current >= max as usize {
                return false;
            }
            if counter
                .compare_exchange(current, current + 1, SeqCst, Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Like [`pick_least_conn`] but respects a per-upstream connection limit.
    ///
    /// Picks the URL with the lowest load that is **below `max_conns`** and
    /// increments its counter.  Returns `None` when ALL URLs are at or above
    /// `max_conns` (circuit breaker: caller should return 503).
    pub fn pick_least_conn_with_max(&self, urls: &[String], max_conns: u64) -> Option<String> {
        // Find the candidate: minimum load among URLs below max.
        let chosen = urls
            .iter()
            .filter(|url| self.conn_load(url) < max_conns as usize)
            .min_by_key(|url| self.conn_load(url))?
            .clone();
        // Atomically acquire the slot — may fail if a concurrent request raced us.
        if self.conn_inc_if_below(&chosen, max_conns) {
            Some(chosen)
        } else {
            // Race: the slot filled up; retry with remaining candidates.
            let remaining: Vec<String> = urls
                .iter()
                .filter(|u| u.as_str() != chosen.as_str())
                .cloned()
                .collect();
            self.pick_least_conn_with_max(&remaining, max_conns)
        }
    }

    // ── Health helpers ────────────────────────────────────────────────────────

    /// Return `true` if `url` is currently considered healthy.
    ///
    /// Defaults to `true` when the URL has not been seen by a probe yet
    /// (optimistic assumption so traffic flows before the first check).
    ///
    /// Implements half-open circuit breaker: when an ejection period expires,
    /// the *first* call transitions the upstream to half-open and returns `true`
    /// (the "probe" request).  Subsequent calls return `false` until the probe
    /// resolves via [`record_request_latency`].
    pub fn is_healthy(&self, url: &str) -> bool {
        // Fast path: entry not yet in registry → optimistic.
        let Some(entry) = self.statuses.get(url) else {
            return true;
        };
        if !entry.healthy {
            return false;
        }
        let Some(until) = entry.ejected_until_secs else {
            return true;
        };
        let now = now_secs();

        if now < until {
            return false; // still within ejection period
        }

        // Ejection period expired.
        if entry.half_open {
            // A probe is already in flight — block this request.
            return false;
        }

        // First request after ejection expires: promote to half-open probe.
        // Drop the shared ref and acquire exclusive access.
        drop(entry);
        let mut e = self.statuses.entry(url.to_owned()).or_default();
        // Re-check after acquiring write lock (another thread may have raced us).
        let now2 = now_secs();
        if let Some(until2) = e.ejected_until_secs {
            if now2 < until2 {
                return false; // re-ejected by racing call
            }
        }
        if e.half_open {
            return false; // another thread beat us to the probe
        }
        // We are the probe request.
        e.half_open = true;
        tracing::info!(
            url,
            "ejection period expired — half-open probe request dispatched"
        );
        true
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

/// Spawn background connection warmup tasks for routes that have
/// `healthCheck.prewarmConnections` configured.
///
/// For each such route, sends `n` sequential HEAD requests to the configured
/// health-check path immediately after startup, populating Pingora's connection
/// pool so the first real user requests don't pay the TCP-handshake cost.
pub fn spawn_connection_warmup(config: &AppConfig) {
    for site in &config.sites {
        let Some(crate::config::schema::ProxyConfig::Routes(routes)) = &site.proxy else {
            continue;
        };
        for route_target in routes.values() {
            let crate::config::schema::ProxyRouteTarget::Full(cfg) = route_target else {
                continue;
            };
            let Some(hc) = &cfg.health_check else {
                continue;
            };
            let n = match hc.prewarm_connections {
                Some(0) | None => continue,
                Some(n) => n.min(8) as usize, // clamp to 8
            };
            let path = hc.path.clone().unwrap_or_else(|| "/".to_string());
            let urls = crate::proxy::upstream::target_urls(route_target);
            for url in urls {
                let path = path.clone();
                tokio::spawn(async move {
                    for i in 0..n {
                        let target = format!("{url}{path}");
                        match reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(5))
                            .build()
                        {
                            Ok(client) => match client.head(&target).send().await {
                                Ok(_) => tracing::debug!(
                                    url,
                                    attempt = i + 1,
                                    total = n,
                                    "connection warmup request succeeded"
                                ),
                                Err(e) => tracing::debug!(
                                    url,
                                    attempt = i + 1,
                                    total = n,
                                    "connection warmup request failed: {e}"
                                ),
                            },
                            Err(e) => tracing::warn!("connection warmup client build error: {e}"),
                        }
                    }
                    tracing::info!(url, n, "connection warmup complete");
                });
            }
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
                    let now = now_secs();
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

    // ── site_label ───────────────────────────────────────────────────────────

    #[test]
    fn site_label_with_host_and_port() {
        assert_eq!(
            site_label(&Some("api.example.com".to_owned()), Some(443)),
            "api.example.com:443"
        );
    }

    #[test]
    fn site_label_host_only() {
        assert_eq!(
            site_label(&Some("example.com".to_owned()), None),
            "example.com"
        );
    }

    #[test]
    fn site_label_port_only() {
        assert_eq!(site_label(&None, Some(8080)), "*:8080");
    }

    #[test]
    fn site_label_no_host_no_port() {
        assert_eq!(site_label(&None, None), "*");
    }

    // ── slow_start_fraction ──────────────────────────────────────────────────

    #[test]
    fn slow_start_zero_window_returns_one() {
        let e = UpstreamEntry::default();
        assert_eq!(slow_start_fraction(&e, 0), 1.0);
    }

    #[test]
    fn slow_start_no_recovery_time_returns_one() {
        let mut e = UpstreamEntry::default();
        e.recovery_time_secs = None;
        assert_eq!(slow_start_fraction(&e, 30), 1.0);
    }

    #[test]
    fn slow_start_already_elapsed_returns_one() {
        let mut e = UpstreamEntry::default();
        // Set recovery time far in the past so elapsed >= window.
        e.recovery_time_secs = Some(now_secs().saturating_sub(60));
        assert_eq!(slow_start_fraction(&e, 30), 1.0);
    }

    #[test]
    fn slow_start_halfway_returns_roughly_half() {
        let mut e = UpstreamEntry::default();
        // Set recovery 15 seconds ago with a 30-second window → ~0.5
        e.recovery_time_secs = Some(now_secs().saturating_sub(15));
        let fraction = slow_start_fraction(&e, 30);
        assert!(
            fraction > 0.3 && fraction < 0.7,
            "expected ~0.5 got {fraction}"
        );
    }

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
            ..Default::default()
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
            ..Default::default()
        };
        apply_probe_result(&mut e, true, 5, 1, 3);
        assert_eq!(e.consecutive_failures, 0);
        assert!(e.healthy);
    }

    // ── Peak EWMA + Outlier Detection ────────────────────────────────────────

    #[test]
    fn ewma_latency_updates_toward_sample() {
        let reg = UpstreamRegistry::new();
        // Default EWMA is 30_000 µs (30 ms).  The first sample has no time-based
        // decay (ewma_last_sample_secs == 0.0 → decay = e^0 = 1.0).
        // After first sample: 1.0 * 30_000 + 0.1 * 100_000 = 30_000 + 10_000 = 40_000
        record_request_latency(&reg, "http://u:4000", 100_000, 200); // 100 ms
        let v = reg.statuses.get("http://u:4000").unwrap().ewma_latency_us;
        // Allow small floating-point tolerance.
        assert!((v - 40_000.0).abs() < 1.0, "expected ≈40_000, got {v}");
    }

    #[test]
    fn ewma_default_is_30ms() {
        // New upstreams start at 30 ms = 30_000 µs (matches linkerd DEFAULT_RTT).
        let entry = super::UpstreamEntry::default();
        assert!((entry.ewma_latency_us - 30_000.0).abs() < 1.0);
    }

    #[test]
    fn consecutive_5xx_increments_on_error() {
        let reg = UpstreamRegistry::new();
        record_request_latency(&reg, "http://u:4000", 10_000, 503);
        record_request_latency(&reg, "http://u:4000", 10_000, 503);
        let c5 = reg.statuses.get("http://u:4000").unwrap().consecutive_5xx;
        assert_eq!(c5, 2);
    }

    #[test]
    fn consecutive_5xx_resets_on_success() {
        let reg = UpstreamRegistry::new();
        record_request_latency(&reg, "http://u:4000", 10_000, 503);
        record_request_latency(&reg, "http://u:4000", 10_000, 200);
        let c5 = reg.statuses.get("http://u:4000").unwrap().consecutive_5xx;
        assert_eq!(c5, 0);
    }

    #[test]
    fn outlier_ejection_after_threshold() {
        use crate::config::schema::OutlierDetectionConfig;
        let reg = UpstreamRegistry::new();
        // Drive consecutive_5xx to threshold.
        for _ in 0..5 {
            record_request_latency(&reg, "http://u:4000", 10_000, 503);
        }
        let cfg = OutlierDetectionConfig {
            consecutive_5xx: Some(5),
            base_ejection_time_secs: Some(30),
            max_ejection_time_secs: Some(300),
            max_ejection_percent: Some(100),
        };
        maybe_eject(&reg, "http://u:4000", &cfg);
        assert!(
            !reg.is_healthy("http://u:4000"),
            "must be ejected after 5 consecutive 5xx"
        );
    }

    #[test]
    fn outlier_no_ejection_below_threshold() {
        use crate::config::schema::OutlierDetectionConfig;
        let reg = UpstreamRegistry::new();
        record_request_latency(&reg, "http://u:4000", 10_000, 503);
        let cfg = OutlierDetectionConfig {
            consecutive_5xx: Some(5),
            base_ejection_time_secs: Some(30),
            max_ejection_time_secs: Some(300),
            max_ejection_percent: Some(100),
        };
        maybe_eject(&reg, "http://u:4000", &cfg);
        assert!(
            reg.is_healthy("http://u:4000"),
            "one 5xx must not trigger ejection"
        );
    }

    #[test]
    fn outlier_skipped_when_max_ejection_percent_reached() {
        use crate::config::schema::OutlierDetectionConfig;
        let reg = UpstreamRegistry::new();

        // Two upstreams: b is already ejected, a hits the threshold.
        // With max_ejection_percent=50 and 2 total upstreams → max 1 ejected.
        // Since b is already ejected, a cannot be ejected.
        for _ in 0..5 {
            record_request_latency(&reg, "http://a:4000", 10_000, 503);
        }
        // Mark b as already ejected (far future).
        {
            let mut e = reg.statuses.entry("http://b:4000".to_owned()).or_default();
            e.ejected_until_secs = Some(u64::MAX);
            e.ejection_count = 1;
        }
        let cfg = OutlierDetectionConfig {
            consecutive_5xx: Some(5),
            base_ejection_time_secs: Some(30),
            max_ejection_time_secs: Some(300),
            max_ejection_percent: Some(50),
        };
        // Try to eject a — should be skipped because b is already ejected (50% of 2).
        maybe_eject(&reg, "http://a:4000", &cfg);
        assert!(
            reg.is_healthy("http://a:4000"),
            "ejection must be skipped when max_ejection_percent is reached"
        );
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

    // ── half-open circuit breaker ─────────────────────────────────────────────
    // Tests use the module-level now_secs() helper directly.

    #[test]
    fn ejected_upstream_is_not_healthy() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .ejected_until_secs = Some(now_secs() + 300);
        assert!(!reg.is_healthy(url), "ejected upstream must not be healthy");
    }

    #[test]
    fn first_call_after_ejection_expires_enters_half_open() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        // Set an expired ejection (in the past).
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .ejected_until_secs = Some(now_secs().saturating_sub(1));
        // First call should return true (probe allowed) and set half_open.
        assert!(
            reg.is_healthy(url),
            "first call after ejection must be allowed as probe"
        );
        assert!(
            reg.statuses.get(url).unwrap().half_open,
            "half_open must be set after probe dispatch"
        );
    }

    #[test]
    fn second_call_while_half_open_is_blocked() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .ejected_until_secs = Some(now_secs().saturating_sub(1));
        // First call = probe.
        let _ = reg.is_healthy(url);
        // Second call = must be blocked.
        assert!(
            !reg.is_healthy(url),
            "second call while half_open must be blocked"
        );
    }

    #[test]
    fn successful_probe_clears_ejection_and_half_open() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        {
            let mut e = reg.statuses.entry(url.to_owned()).or_default();
            e.ejected_until_secs = Some(now_secs().saturating_sub(1));
            e.ejection_count = 2;
        }
        // Dispatch the probe.
        assert!(reg.is_healthy(url));
        // Simulate a successful probe response (status 200).
        record_request_latency(&reg, url, 10_000, 200);
        let e = reg.statuses.get(url).unwrap();
        assert!(!e.half_open, "half_open must be cleared after success");
        assert!(
            e.ejected_until_secs.is_none(),
            "ejection must be lifted after success"
        );
        assert_eq!(
            e.ejection_count, 0,
            "ejection_count must be reset after success"
        );
    }

    #[test]
    fn failed_probe_re_ejects_with_backoff() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        {
            let mut e = reg.statuses.entry(url.to_owned()).or_default();
            e.ejected_until_secs = Some(now_secs().saturating_sub(1));
            e.ejection_count = 1;
        }
        assert!(reg.is_healthy(url));
        // Simulate a failed probe response (status 500).
        record_request_latency(&reg, url, 10_000, 500);
        let e = reg.statuses.get(url).unwrap();
        assert!(!e.half_open, "half_open must be cleared after failure");
        assert!(
            e.ejected_until_secs.is_some(),
            "upstream must be re-ejected after probe failure"
        );
        assert_eq!(e.ejection_count, 2, "ejection_count must be incremented");
    }

    // ── override_key ──────────────────────────────────────────────────────────

    #[test]
    fn override_key_format_uses_nul_separator() {
        let key = override_key("mysite:8080", "/api");
        assert_eq!(key, "mysite:8080\0/api");
    }

    #[test]
    fn override_key_wildcard_site() {
        let key = override_key("*", "/api");
        assert_eq!(key, "*\0/api");
    }

    // ── UpstreamEntry default values ──────────────────────────────────────────

    #[test]
    fn upstream_entry_default_is_healthy_and_not_ejected() {
        let e = UpstreamEntry::default();
        assert!(e.healthy, "new entry must be healthy by default");
        assert!(e.ejected_until_secs.is_none());
        assert!(!e.half_open);
        assert_eq!(e.ejection_count, 0);
        assert_eq!(e.consecutive_5xx, 0);
        // Default EWMA latency matches linkerd's DEFAULT_RTT (30ms = 30000 µs)
        assert_eq!(e.ewma_latency_us, 30_000.0);
    }

    // ── spawn_health_checks and spawn_connection_warmup ───────────────────────

    #[test]
    fn spawn_health_checks_empty_config_is_noop() {
        // With no sites having health checks, spawn_health_checks must not panic.
        let reg = std::sync::Arc::new(UpstreamRegistry::new());
        let config = crate::config::schema::AppConfig::default();
        // No tokio::spawn will be called since no health checks are configured.
        spawn_health_checks(reg, &config);
    }

    #[test]
    fn spawn_connection_warmup_empty_config_is_noop() {
        let config = crate::config::schema::AppConfig::default();
        spawn_connection_warmup(&config);
    }

    // ── conn_load / conn_inc / conn_dec ───────────────────────────────────────

    #[test]
    fn conn_load_returns_zero_for_unknown_url() {
        let reg = UpstreamRegistry::new();
        assert_eq!(reg.conn_load("http://unknown:4000"), 0);
    }

    #[test]
    fn conn_inc_increments_counter() {
        let reg = UpstreamRegistry::new();
        let url = "http://a:4000";
        reg.conn_inc(url);
        assert_eq!(reg.conn_load(url), 1);
        reg.conn_inc(url);
        assert_eq!(reg.conn_load(url), 2);
    }

    #[test]
    fn conn_dec_decrements_counter() {
        let reg = UpstreamRegistry::new();
        let url = "http://a:4000";
        reg.conn_inc(url);
        reg.conn_inc(url);
        reg.conn_dec(url);
        assert_eq!(reg.conn_load(url), 1);
    }

    #[test]
    fn conn_dec_saturates_at_zero() {
        let reg = UpstreamRegistry::new();
        let url = "http://a:4000";
        // No increment → dec should not underflow.
        reg.conn_dec(url);
        assert_eq!(reg.conn_load(url), 0);
    }

    #[test]
    fn conn_dec_unknown_url_is_noop() {
        let reg = UpstreamRegistry::new();
        // Decrementing an unknown URL must not panic.
        reg.conn_dec("http://never-seen:4000");
    }

    // ── conn_inc_if_below ─────────────────────────────────────────────────────

    #[test]
    fn conn_inc_if_below_acquires_slot_when_below_max() {
        let reg = UpstreamRegistry::new();
        let url = "http://a:4000";
        assert!(reg.conn_inc_if_below(url, 3), "first slot must be acquired");
        assert_eq!(reg.conn_load(url), 1);
    }

    #[test]
    fn conn_inc_if_below_rejects_when_at_max() {
        let reg = UpstreamRegistry::new();
        let url = "http://a:4000";
        reg.conn_inc(url); // count = 1
        reg.conn_inc(url); // count = 2
        reg.conn_inc(url); // count = 3
        assert!(
            !reg.conn_inc_if_below(url, 3),
            "at max (3) must be rejected"
        );
        assert_eq!(reg.conn_load(url), 3, "count must not change");
    }

    // ── pick_least_conn ───────────────────────────────────────────────────────

    #[test]
    fn pick_least_conn_returns_none_for_empty_list() {
        let reg = UpstreamRegistry::new();
        assert!(reg.pick_least_conn(&[]).is_none());
    }

    #[test]
    fn pick_least_conn_picks_lowest_load() {
        let reg = UpstreamRegistry::new();
        let a = "http://a:4000".to_owned();
        let b = "http://b:4000".to_owned();
        reg.conn_inc(&a);
        reg.conn_inc(&a); // a = 2, b = 0
        let chosen = reg.pick_least_conn(&[a.clone(), b.clone()]);
        assert_eq!(chosen.as_deref(), Some(b.as_str()), "b has lower load");
    }

    // ── pick_least_conn_with_max ──────────────────────────────────────────────

    #[test]
    fn pick_least_conn_with_max_returns_none_when_all_at_max() {
        let reg = UpstreamRegistry::new();
        let url = "http://a:4000".to_owned();
        reg.conn_inc(&url); // = 1 = max
        let result = reg.pick_least_conn_with_max(&[url], 1);
        assert!(result.is_none(), "all at max → None (circuit breaker)");
    }

    #[test]
    fn pick_least_conn_with_max_picks_below_max_url() {
        let reg = UpstreamRegistry::new();
        let a = "http://a:4000".to_owned();
        let b = "http://b:4000".to_owned();
        reg.conn_inc(&a); // a = 1, b = 0; max = 2
        let chosen = reg.pick_least_conn_with_max(&[a.clone(), b.clone()], 2);
        assert_eq!(chosen.as_deref(), Some(b.as_str()), "b has lower load");
    }

    // ── filter_healthy ────────────────────────────────────────────────────────

    #[test]
    fn filter_healthy_returns_all_when_no_status_known() {
        let reg = UpstreamRegistry::new();
        let urls = vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()];
        // No health status set → all optimistically healthy.
        let healthy = reg.filter_healthy(&urls);
        assert_eq!(healthy.len(), 2);
    }

    #[test]
    fn filter_healthy_excludes_unhealthy() {
        let reg = UpstreamRegistry::new();
        let url_a = "http://a:4000".to_owned();
        let url_b = "http://b:4000".to_owned();
        reg.statuses.entry(url_a.clone()).or_default().healthy = false;
        let urls = vec![url_a, url_b.clone()];
        let healthy = reg.filter_healthy(&urls);
        assert_eq!(healthy.len(), 1);
        assert_eq!(*healthy[0], url_b);
    }

    // ── filter_healthy: all-unhealthy fail-open ───────────────────────────────

    #[test]
    fn filter_healthy_returns_all_when_all_unhealthy() {
        let reg = UpstreamRegistry::new();
        let url_a = "http://a:4000".to_owned();
        let url_b = "http://b:4000".to_owned();
        // Both explicitly marked unhealthy.
        reg.statuses.entry(url_a.clone()).or_default().healthy = false;
        reg.statuses.entry(url_b.clone()).or_default().healthy = false;
        let urls = vec![url_a.clone(), url_b.clone()];
        let result = reg.filter_healthy(&urls);
        // All unhealthy → fail-open: return all (try anyway).
        assert_eq!(
            result.len(),
            2,
            "fail-open: all must be returned when none healthy"
        );
    }

    // ── dynamic override API ──────────────────────────────────────────────────

    #[test]
    fn add_and_get_override_targets() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/api", "http://extra:4000", 2);
        let targets = reg
            .get_override_targets("*", "/api")
            .expect("must return Some after add");
        assert!(
            targets
                .iter()
                .any(|(url, w)| url == "http://extra:4000" && *w == 2),
            "added upstream must appear: {targets:?}"
        );
    }

    #[test]
    fn remove_upstream_removes_entry() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/api", "http://extra:4000", 1);
        let removed = reg.remove_upstream("*", "/api", "http://extra:4000");
        assert!(removed, "remove must return true");
        let targets = reg.get_override_targets("*", "/api").unwrap_or_default();
        assert!(
            !targets.iter().any(|(url, _)| url == "http://extra:4000"),
            "removed upstream must be gone"
        );
    }

    #[test]
    fn clear_overrides_removes_all() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/a", "http://a:4000", 1);
        reg.add_upstream("*", "/b", "http://b:4000", 1);
        reg.clear_overrides();
        let a = reg.get_override_targets("*", "/a").unwrap_or_default();
        let b = reg.get_override_targets("*", "/b").unwrap_or_default();
        assert!(
            a.is_empty() && b.is_empty(),
            "all overrides must be cleared"
        );
    }

    #[test]
    fn set_weight_updates_existing() {
        let reg = UpstreamRegistry::new();
        reg.add_upstream("*", "/api", "http://a:4000", 1);
        let ok = reg.set_weight("*", "/api", "http://a:4000", 5);
        assert!(ok, "set_weight must return true for existing entry");
        let targets = reg
            .get_override_targets("*", "/api")
            .expect("targets must exist");
        assert!(
            targets
                .iter()
                .any(|(url, w)| url == "http://a:4000" && *w == 5),
            "weight must be updated: {targets:?}"
        );
    }
}
