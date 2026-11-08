//! Upstream health tracking and background health check tasks.
//!
//! The [`UpstreamRegistry`] lives in `AppState` (root crate) and is shared by
//! the router (to filter unhealthy upstreams and to count inflight
//! connections for least-conn balancing) and the admin API (to report
//! status).
//!
//! Background health check tasks are spawned once at server start inside the
//! root crate's `AdminApiService::start()` via [`spawn_health_checks`].

use std::sync::atomic::AtomicUsize;

use dashmap::DashMap;

use crate::config::OutlierDetectionConfig;

#[cfg(feature = "proxy")]
pub use crate::probes::spawn_connection_warmup;
pub use crate::probes::spawn_health_checks;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the current Unix timestamp as whole seconds.
///
/// Used throughout the health module for ejection/recovery time comparisons.
/// Centralised here to avoid repeating the same `SystemTime::now()…` chain.
#[inline]
pub(crate) fn now_secs() -> u64 {
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
    /// Unix timestamp (secs) until which the half-open promotion waits for its probe request's result
    /// (`HALF_OPEN_PROBE_TIMEOUT_SECS` after the promotion). Past it, a promotion that nobody answered
    /// lapses and the upstream is offered again (issue #650). Kept apart from `ejected_until_secs` so that
    /// the admin view, the health endpoint and the ejection cap keep seeing an expired ejection as expired.
    pub half_open_until_secs: Option<u64>,
    /// Cumulative count of 2xx responses received from this upstream.
    pub responses_2xx: u64,
    /// Cumulative count of 4xx responses received from this upstream.
    pub responses_4xx: u64,
    /// Cumulative count of 5xx responses received from this upstream.
    pub responses_5xx: u64,
    /// Total number of times this upstream was selected to handle a request.
    pub selected_total: u64,
    /// Unix timestamp (seconds) of the last time this upstream was selected.
    /// Zero means it has never been selected since the process started.
    pub selected_last_secs: u64,
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
            half_open_until_secs: None,
            responses_2xx: 0,
            responses_4xx: 0,
            responses_5xx: 0,
            selected_total: 0,
            selected_last_secs: 0,
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

/// How long a half-open promotion holds the upstream closed while it waits for the probe request's
/// result. If no result arrives in this time (the strategy picked another upstream, or the request
/// never completed), the upstream is offered again (issue #650).
pub const HALF_OPEN_PROBE_TIMEOUT_SECS: u64 = 10;

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
        entry.half_open_until_secs = None;
        if status < 500 && status != 0 {
            // Probe succeeded: lift the ejection and reset the counter so the
            // next ejection cycle starts fresh.
            entry.ejected_until_secs = None;
            entry.ejection_count = 0;
            // Slow start (#157): a half-open probe succeeding is a genuine
            // recovery, same as an active health-check flipping the peer back
            // to healthy (see spawn_health_task) -- and on the routes[]/groups
            // paths it is the ONLY recovery signal available at all, since
            // active probes aren't spawned there. Without this, slow_start_fraction
            // never sees a recovery on those paths and slowStartSecs stays inert.
            entry.recovery_time_secs = Some(now_secs());
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

/// Record a completed request's HTTP status code for per-upstream statistics.
///
/// Increments the matching `responses_2xx`, `responses_4xx`, or `responses_5xx`
/// counter on the upstream entry.  Called from `logging()` after every proxied
/// request.
pub fn record_response_status(registry: &UpstreamRegistry, url: &str, status: u16) {
    let mut entry = registry.statuses.entry(url.to_owned()).or_default();
    match status {
        200..=299 => entry.responses_2xx = entry.responses_2xx.saturating_add(1),
        400..=499 => entry.responses_4xx = entry.responses_4xx.saturating_add(1),
        500..=599 => entry.responses_5xx = entry.responses_5xx.saturating_add(1),
        _ => {}
    }
}

/// Record that this upstream was selected to handle a request.
///
/// Increments `selected_total` and updates `selected_last_secs`.
/// Called from `upstream_request_filter()`.
pub fn record_upstream_selected(registry: &UpstreamRegistry, url: &str) {
    let mut entry = registry.statuses.entry(url.to_owned()).or_default();
    entry.selected_total = entry.selected_total.saturating_add(1);
    entry.selected_last_secs = now_secs();
}

/// Check whether `url` should be ejected based on its consecutive 5xx count.
///
/// If the threshold is reached the upstream is ejected for
/// `base_time × 2^ejection_count` seconds, capped at `max_ejection_time_secs`.
/// At most `max_ejection_percent` % of the tracked upstreams may be ejected at once.
pub fn maybe_eject(registry: &UpstreamRegistry, url: &str, cfg: &OutlierDetectionConfig) {
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

/// Compute the slow-start traffic fraction for an upstream.
///
/// Returns a value in `[0.0, 1.0]`:
/// - `1.0` when slow-start is disabled or the ramp window has elapsed.
/// - A value proportional to `elapsed / window_secs` during ramp-up.
///
/// Callers should multiply their selection probability by this value.
///
/// Consumed by [`crate::proxy::slow_start::Ramp`] (issue #157), the single
/// dispatch point every non-hash `LoadBalancingStrategy` gets ramp admission
/// from -- hash-based strategies and sticky sessions are deliberately exempt
/// (see that module's doc comment for why).
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
    /// Per-upstream traffic meters, used by the `least-bandwidth` strategy (issue #627). An entry is
    /// created by the first byte recorded for a URL; only routes that use that strategy record any.
    pub traffic: DashMap<String, std::sync::Arc<crate::traffic::TrafficMeter>>,
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
            traffic: DashMap::new(),
            overrides: DashMap::new(),
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
    /// resolves via [`record_request_latency`], or for [`HALF_OPEN_PROBE_TIMEOUT_SECS`]
    /// at most: this call only *offers* the upstream to the strategy, which may pick
    /// another one, and then no request is sent and nothing resolves the probe, so the
    /// promotion expires and the next call offers the upstream again (issue #650: it
    /// used to stay out for good).
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
        if entry.half_open && entry.half_open_until_secs.is_some_and(|h| now < h) {
            // A probe is already awaited: block this request.
            return false;
        }

        // Offer the upstream as the probe: promote to half-open. This is also the path of a
        // promotion that nobody answered (`half_open` with its deadline passed: the strategy
        // picked another upstream, or the request never completed): it is offered again.
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
        if e.half_open && e.half_open_until_secs.is_some_and(|h| now2 < h) {
            return false; // another thread beat us to the probe
        }
        // We are the probe request: the upstream is closed to others until the deadline.
        e.half_open = true;
        e.half_open_until_secs = Some(now2.saturating_add(HALF_OPEN_PROBE_TIMEOUT_SECS));
        tracing::info!(
            url,
            "ejection period expired — half-open probe request dispatched"
        );
        true
    }

    /// The same answer as [`Self::is_healthy`], without its side effect: an upstream whose ejection
    /// has expired counts as a candidate and is **not** promoted to half-open. Use it to *ask*
    /// ("is every upstream down?", "is the pinned one up?"); [`Self::is_healthy`] (through
    /// [`Self::filter_healthy`]) is for building the list a pick chooses from, because it hands out
    /// the single probe offer. Asking with `is_healthy` before the pick is built used up the offer on
    /// a query nobody acts on (issue #650).
    pub fn is_healthy_peek(&self, url: &str) -> bool {
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
            return false; // still within the ejection period
        }
        // Expired: a candidate, unless the probe offer is out and awaited.
        !(entry.half_open && entry.half_open_until_secs.is_some_and(|h| now < h))
    }

    /// Filter `urls` to only healthy ones.
    ///
    /// If all upstreams are down the original list is returned unchanged so
    /// the proxy continues to try rather than hard-failing. The second
    /// tuple element is `true` exactly in that fail-open case -- callers
    /// that need to tell "genuinely healthy" apart from "nothing is
    /// healthy, trying anyway" (issue #374) should check it rather than
    /// re-deriving health from the returned list, which can't make that
    /// distinction on its own. Deliberately does **not** offer a second,
    /// independent way to ask "is this URL really healthy" instead (e.g. a
    /// second call to [`Self::is_healthy`] on the same URL within one
    /// request) -- `is_healthy`'s half-open probe promotion is stateful and
    /// only fires once per ejection cycle, so a second call for the same
    /// URL in the same request would see the *first* call's side effect
    /// and silently disagree with it.
    pub fn filter_healthy<'a>(&self, urls: &'a [String]) -> (Vec<&'a String>, bool) {
        let healthy: Vec<&String> = urls.iter().filter(|u| self.is_healthy(u)).collect();
        if healthy.is_empty() {
            (urls.iter().collect(), true)
        } else {
            (healthy, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UpstreamHealthCheck;
    use crate::overrides::override_key;
    use crate::probes::{apply_probe_result, probe_http};

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
        // recovery_time_secs is already None by default.
        let e = UpstreamEntry::default();
        assert_eq!(slow_start_fraction(&e, 30), 1.0);
    }

    #[test]
    fn slow_start_already_elapsed_returns_one() {
        // Set recovery time far in the past so elapsed >= window.
        let e = UpstreamEntry {
            recovery_time_secs: Some(now_secs().saturating_sub(60)),
            ..Default::default()
        };
        assert_eq!(slow_start_fraction(&e, 30), 1.0);
    }

    #[test]
    fn slow_start_halfway_returns_roughly_half() {
        // Set recovery 15 seconds ago with a 30-second window → ~0.5
        let e = UpstreamEntry {
            recovery_time_secs: Some(now_secs().saturating_sub(15)),
            ..Default::default()
        };
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
        use crate::config::OutlierDetectionConfig;
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
        use crate::config::OutlierDetectionConfig;
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
        use crate::config::OutlierDetectionConfig;
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

        let (ok, latency_ms) = probe_http(
            &format!("127.0.0.1:{port}"),
            "/",
            std::time::Duration::from_secs(5),
        )
        .await;
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

        let (ok, _) = probe_http(
            &format!("127.0.0.1:{port}"),
            "/",
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(!ok, "probe should fail on HTTP 500");
    }

    #[tokio::test]
    async fn probe_http_fails_when_nothing_listening() {
        // Bind then drop so the port is released before we call probe_http.
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let (ok, latency_ms) = probe_http(
            &format!("127.0.0.1:{port}"),
            "/",
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(!ok, "probe should fail when nothing is listening");
        let _ = latency_ms;
    }

    /// An upstream that accepts the connection and never answers: the probe gives up after `timeoutMs`
    /// (issue #652), not after a fixed 5 seconds.
    #[tokio::test]
    async fn probe_http_gives_up_on_a_hanging_upstream_after_the_timeout() {
        use std::time::{Duration, Instant};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // keep the connection open and silent
            let _held = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let started = Instant::now();
        let (ok, latency_ms) = probe_http(
            &format!("127.0.0.1:{port}"),
            "/",
            Duration::from_millis(200),
        )
        .await;
        assert!(!ok, "a silent upstream is a failed probe");
        assert_eq!(latency_ms, 0, "a failed probe reports no latency");
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(200) && took < Duration::from_secs(3),
            "it must wait for the configured timeout, not the old fixed 5 s: {took:?}"
        );
    }

    /// The timeout is one deadline for the whole probe: a status line that arrives after it fails the probe
    /// even though connecting and sending were fast, and the same upstream passes with a longer timeout.
    #[tokio::test]
    async fn probe_http_timeout_covers_the_whole_probe() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // answers every connection, 400 ms after it has read the request
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    let _ = stream.read(&mut buf).await;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                });
            }
        });
        let addr = format!("127.0.0.1:{port}");

        let (slow_ok, _) = probe_http(&addr, "/", Duration::from_millis(150)).await;
        assert!(
            !slow_ok,
            "400 ms of silence is a failure under a 150 ms deadline"
        );
        let (patient_ok, latency_ms) = probe_http(&addr, "/", Duration::from_secs(5)).await;
        assert!(patient_ok, "and a pass under a 5 s deadline");
        assert!(
            latency_ms >= 400,
            "the latency includes the wait: {latency_ms}"
        );
    }

    #[test]
    fn probe_timeout_reads_the_option_and_defaults_to_five_seconds() {
        use crate::probes::probe_timeout;
        use std::time::Duration;

        assert_eq!(
            probe_timeout(&UpstreamHealthCheck::default()),
            Duration::from_secs(5)
        );
        let hc = UpstreamHealthCheck {
            timeout_ms: Some(750),
            ..Default::default()
        };
        assert_eq!(probe_timeout(&hc), Duration::from_millis(750));
        // 0 is refused by `validate`; if it ever gets here it is 1 ms, never "no timeout"
        let zero = UpstreamHealthCheck {
            timeout_ms: Some(0),
            ..Default::default()
        };
        assert_eq!(probe_timeout(&zero), Duration::from_millis(1));
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
        let chosen = reg.pick_least_conn(&urls, &[]).unwrap();
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

        // filter_healthy must return all when all are down (fail-open), and
        // report that it did so via the second tuple element (#374).
        let (result, fail_open) = reg.filter_healthy(&urls);
        assert_eq!(result.len(), 2);
        assert!(fail_open, "fail_open must be true when every peer is down");
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

        let (result, fail_open) = reg.filter_healthy(&urls);
        assert_eq!(result.len(), 1);
        assert_eq!(*result[0], "http://b:4000");
        assert!(
            !fail_open,
            "fail_open must be false when at least one peer is genuinely healthy"
        );
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

    /// Issue #650: the promotion is made by a query (`filter_healthy` asks about every upstream of
    /// the route before the strategy picks one). When the strategy picks another upstream no probe
    /// request is sent and nothing clears `half_open`; it used to stay `true`, and the upstream out,
    /// for good.
    #[test]
    fn a_half_open_promotion_without_a_probe_request_expires_and_the_upstream_is_offered_again() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .ejected_until_secs = Some(now_secs().saturating_sub(1));
        // The query that promotes it; no request follows.
        assert!(reg.is_healthy(url), "offered as the probe");
        assert!(!reg.is_healthy(url), "closed while the probe is awaited");
        let (ejected, until) = {
            let e = reg.statuses.get(url).unwrap();
            (
                e.ejected_until_secs.unwrap(),
                e.half_open_until_secs.unwrap(),
            )
        };
        assert!(
            until >= now_secs() + HALF_OPEN_PROBE_TIMEOUT_SECS - 1
                && until <= now_secs() + HALF_OPEN_PROBE_TIMEOUT_SECS,
            "the promotion holds the upstream for the probe timeout"
        );
        assert!(
            ejected <= now_secs(),
            "the ejection itself stays expired: the admin view, the health endpoint and the ejection cap must not see a probe as an ejection"
        );
        // The timeout passes with no result.
        reg.statuses.get_mut(url).unwrap().half_open_until_secs =
            Some(now_secs().saturating_sub(1));
        assert!(
            reg.is_healthy(url),
            "after the timeout the upstream is offered again (it used to stay out for good)"
        );
        assert!(
            !reg.is_healthy(url),
            "and is closed to others again while that probe is awaited"
        );
    }

    /// The probe that is answered still resolves the state as before (the timeout is only the fallback).
    #[test]
    fn an_answered_probe_after_a_re_offer_recovers_the_upstream() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .ejected_until_secs = Some(now_secs().saturating_sub(1));
        assert!(reg.is_healthy(url));
        reg.statuses.get_mut(url).unwrap().half_open_until_secs =
            Some(now_secs().saturating_sub(1));
        assert!(reg.is_healthy(url), "re-offered");
        record_request_latency(&reg, url, 10_000, 200);
        let e = reg.statuses.get(url).unwrap();
        assert!(!e.half_open && e.half_open_until_secs.is_none() && e.ejected_until_secs.is_none());
        drop(e);
        assert!(reg.is_healthy(url), "fully recovered");
    }

    /// `is_healthy_peek` answers like `is_healthy` but never promotes: asking before the pool is built
    /// must leave the probe offer for the pick that builds the candidate list (issue #650).
    #[test]
    fn peek_does_not_promote_and_agrees_with_is_healthy_otherwise() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        assert!(reg.is_healthy_peek(url), "unknown upstream: optimistic");
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .ejected_until_secs = Some(now_secs() + 300);
        assert!(!reg.is_healthy_peek(url), "within the ejection period");
        reg.statuses.get_mut(url).unwrap().ejected_until_secs = Some(now_secs().saturating_sub(1));
        assert!(reg.is_healthy_peek(url), "expired ejection: a candidate");
        assert!(reg.is_healthy_peek(url), "and asking again changes nothing");
        assert!(
            !reg.statuses.get(url).unwrap().half_open,
            "peek never promotes"
        );
        // The offer is still there for the query that builds the pool.
        assert!(
            reg.is_healthy(url),
            "the promoting query still gets the offer"
        );
        assert!(
            !reg.is_healthy_peek(url),
            "an awaited probe closes the upstream to peek as well"
        );
        reg.statuses.get_mut(url).unwrap().healthy = false;
        reg.statuses.get_mut(url).unwrap().half_open = false;
        assert!(!reg.is_healthy_peek(url), "a failed active probe series");
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
    fn successful_half_open_probe_records_recovery_time() {
        // Slow start (#157): a half-open probe succeeding is a genuine
        // recovery signal, same as an active health-check flipping the peer
        // back to healthy -- and on the routes[]/groups paths (whose
        // active-probe path has its own separate, pre-existing gap) it's the
        // ONLY recovery signal that exists at all. Without this,
        // slow_start_fraction never sees a recovery there and slowStartSecs
        // stays inert regardless of the routing-side fix.
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        {
            let mut e = reg.statuses.entry(url.to_owned()).or_default();
            e.ejected_until_secs = Some(now_secs().saturating_sub(1));
            e.ejection_count = 2;
        }
        assert!(reg.is_healthy(url)); // dispatches the half-open probe
        record_request_latency(&reg, url, 10_000, 200); // probe succeeds
        let e = reg.statuses.get(url).unwrap();
        assert!(
            e.recovery_time_secs.is_some(),
            "a successful half-open probe must record a recovery time"
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
    fn spawn_health_checks_no_routes_is_noop() {
        // With no routes supplied, spawn_health_checks must not panic.
        let reg = std::sync::Arc::new(UpstreamRegistry::new());
        // No tokio::spawn will be called since the iterator is empty.
        spawn_health_checks(reg, std::iter::empty::<(&UpstreamHealthCheck, &[String])>());
    }

    #[test]
    fn spawn_health_checks_empty_urls_is_noop() {
        // A route with a healthCheck config but no target URLs must be
        // skipped, not spawn a background task for an empty URL list.
        let reg = std::sync::Arc::new(UpstreamRegistry::new());
        let hc = UpstreamHealthCheck::default();
        let urls: Vec<String> = Vec::new();
        spawn_health_checks(reg, std::iter::once((&hc, urls.as_slice())));
    }

    #[test]
    #[cfg(feature = "proxy")]
    fn spawn_connection_warmup_no_routes_is_noop() {
        spawn_connection_warmup(std::iter::empty::<(&UpstreamHealthCheck, &[String])>());
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

    // ── traffic (least-bandwidth, #627) ───────────────────────────────────────

    #[test]
    fn traffic_is_recorded_per_url_and_unknown_urls_have_rate_zero() {
        let reg = UpstreamRegistry::new();
        assert_eq!(reg.traffic_rate("http://a:4000"), 0.0);
        reg.record_traffic("http://a:4000", 1_000);
        reg.record_traffic("http://a:4000", 2_000);
        reg.record_traffic("http://b:4000", 7);
        assert_eq!(reg.traffic.get("http://a:4000").unwrap().total(), 3_000);
        assert_eq!(reg.traffic.get("http://b:4000").unwrap().total(), 7);
        assert!(reg.traffic.get("http://c:4000").is_none());
    }

    /// Cost of the one thing `least-bandwidth` adds to the request path, `record_traffic` once per body
    /// chunk. Run: `cargo test -p lopatnov-conduit-upstream --release -- --ignored record_traffic_cost --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn record_traffic_cost() {
        use std::sync::Arc;
        use std::time::Instant;
        const CALLS: u64 = 5_000_000;
        let reg = Arc::new(UpstreamRegistry::new());
        let url = "http://upstream-1.internal:4000";
        reg.record_traffic(url, 1);
        for threads in [1u64, 4, 8] {
            let start = Instant::now();
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let reg = reg.clone();
                    std::thread::spawn(move || {
                        for _ in 0..CALLS {
                            reg.record_traffic(url, 16 * 1024);
                        }
                    })
                })
                .collect();
            handles.into_iter().for_each(|h| h.join().unwrap());
            let ns = start.elapsed().as_nanos() as f64 / (CALLS * threads) as f64;
            eprintln!("record_traffic: {threads} thread(s) on one URL: {ns:.1} ns per call");
        }
        // The in-flight pair every request of `least-conn`, `p2c` and `least-bandwidth` makes.
        let start = Instant::now();
        for _ in 0..CALLS {
            reg.conn_inc(url);
            reg.conn_dec(url);
        }
        eprintln!(
            "conn_inc + conn_dec (key exists): {:.1} ns per pair",
            start.elapsed().as_nanos() as f64 / CALLS as f64
        );
        let now = crate::traffic::monotonic_ms();
        let start = Instant::now();
        let mut rate = 0.0;
        for _ in 0..CALLS {
            rate += reg.traffic_rate_at(url, now);
        }
        eprintln!(
            "traffic_rate_at (one candidate, inside the window): {:.1} ns per call ({rate:e})",
            start.elapsed().as_nanos() as f64 / CALLS as f64
        );
        let start = Instant::now();
        for _ in 0..CALLS {
            rate += reg.traffic_rate(url);
        }
        eprintln!(
            "traffic_rate (reads the clock too): {:.1} ns per call ({rate:e})",
            start.elapsed().as_nanos() as f64 / CALLS as f64
        );
    }

    // ── pick_least_conn ───────────────────────────────────────────────────────

    #[test]
    fn pick_least_conn_returns_none_for_empty_list() {
        let reg = UpstreamRegistry::new();
        assert!(reg.pick_least_conn(&[], &[]).is_none());
    }

    #[test]
    fn pick_least_conn_picks_lowest_load() {
        let reg = UpstreamRegistry::new();
        let a = "http://a:4000".to_owned();
        let b = "http://b:4000".to_owned();
        reg.conn_inc(&a);
        reg.conn_inc(&a); // a = 2, b = 0
        let chosen = reg.pick_least_conn(&[a.clone(), b.clone()], &[]);
        assert_eq!(chosen.as_deref(), Some(b.as_str()), "b has lower load");
    }

    // ── pick_least_conn, weighted (#506) ──────────────────────────────────────

    /// Run `n` requests that never finish (every pick stays inflight) against `urls` and count how
    /// many each peer received.
    fn spread(
        reg: &UpstreamRegistry,
        urls: &[String],
        weighted: &[(String, u32)],
        n: usize,
    ) -> Vec<usize> {
        for _ in 0..n {
            reg.pick_least_conn(urls, weighted).expect("a candidate");
        }
        urls.iter().map(|u| reg.conn_load(u)).collect()
    }

    fn pair(a: u32, b: u32) -> (Vec<String>, Vec<(String, u32)>) {
        let urls = vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()];
        let weighted = vec![(urls[0].clone(), a), (urls[1].clone(), b)];
        (urls, weighted)
    }

    #[test]
    fn weighted_least_conn_shares_inflight_in_proportion_to_weight() {
        let (urls, weighted) = pair(3, 1);
        let reg = UpstreamRegistry::new();
        // 40 requests held open: a (weight 3) must carry three times what b carries.
        assert_eq!(spread(&reg, &urls, &weighted, 40), vec![30, 10]);
    }

    /// Ties are random, so compare what is guaranteed: weights absent, 1 or all 7 split 41 held-open requests 21/20.
    #[test]
    fn weighted_least_conn_with_equal_weights_matches_the_unweighted_pick() {
        // Ties are broken at random, so compare what is guaranteed: 41 held-open requests over two
        // peers split 21/20 either way round, whether the weights are absent, 1 or all the same.
        let (urls, ones) = pair(1, 1);
        let (_, sevens) = pair(7, 7);
        for weights in [&[][..], &ones[..], &sevens[..]] {
            let counts = spread(&UpstreamRegistry::new(), &urls, weights, 41);
            assert_eq!(counts.iter().sum::<usize>(), 41);
            assert!(
                counts[0].abs_diff(counts[1]) <= 1,
                "weights {weights:?} split 41 requests as {counts:?}"
            );
        }
    }

    /// With the weight-1 peer already carrying one request, the weight-3 peer takes the next two.
    #[test]
    fn weighted_least_conn_prefers_the_heavy_peer_when_loads_are_equal() {
        let (urls, weighted) = pair(1, 3);
        let reg = UpstreamRegistry::new();
        // a (weight 1) already carries one request: its ratio is 1, b's (weight 3) is 0, so b takes
        // the next two requests (ratios 1/3 and 2/3 are both below 1).
        reg.conn_inc(&urls[0]);
        for _ in 0..2 {
            assert_eq!(reg.pick_least_conn(&urls, &weighted).unwrap(), urls[1]);
        }
    }

    #[test]
    fn weighted_least_conn_never_picks_a_zero_weight_peer_while_another_has_weight() {
        let (urls, weighted) = pair(0, 1);
        let reg = UpstreamRegistry::new();
        // Even though a is idle and b is busy, a has no weight.
        reg.conn_inc(&urls[1]);
        reg.conn_inc(&urls[1]);
        assert_eq!(reg.pick_least_conn(&urls, &weighted).unwrap(), urls[1]);
    }

    #[test]
    fn weighted_least_conn_with_every_weight_zero_still_routes_by_load() {
        let (urls, weighted) = pair(0, 0);
        let reg = UpstreamRegistry::new();
        reg.conn_inc(&urls[0]);
        assert_eq!(reg.pick_least_conn(&urls, &weighted).unwrap(), urls[1]);
    }

    #[test]
    fn weighted_least_conn_handles_the_largest_weights() {
        let (urls, weighted) = pair(u32::MAX, u32::MAX - 1);
        let reg = UpstreamRegistry::new();
        reg.conn_inc(&urls[0]);
        // a: 1/(2^32-1), b: 0/(2^32-2) -> b has the lower ratio.
        assert_eq!(reg.pick_least_conn(&urls, &weighted).unwrap(), urls[1]);
    }

    /// Sequential traffic: every request is released before the next one arrives, so every pick sees
    /// all peers at the same load and is a tie.
    fn sequential_picks(urls: &[String], weighted: &[(String, u32)], n: usize) -> Vec<usize> {
        let reg = UpstreamRegistry::new();
        let mut counts = vec![0usize; urls.len()];
        for _ in 0..n {
            let url = reg.pick_least_conn(urls, weighted).expect("a candidate");
            counts[urls.iter().position(|u| *u == url).unwrap()] += 1;
            reg.conn_dec(&url);
        }
        counts
    }

    /// Sequential requests tie every time: 600 picks over three peers land about 200 each (standard deviation 11.5).
    #[test]
    fn least_conn_spreads_ties_when_nothing_stays_in_flight() {
        let urls: Vec<String> = (0..3).map(|i| format!("http://b{i}:4000")).collect();
        // 600 ties over 3 peers: 200 each, standard deviation about 11.5.
        for (i, count) in sequential_picks(&urls, &[], 600).into_iter().enumerate() {
            assert!(
                (140..=260).contains(&count),
                "peer {i} got {count} of 600 tied picks"
            );
        }
    }

    #[test]
    fn weighted_least_conn_ties_follow_the_weights() {
        let (urls, weighted) = pair(3, 1);
        // 800 ties, weights 3:1: 600 and 200, standard deviation about 12.
        let counts = sequential_picks(&urls, &weighted, 800);
        assert!(
            (540..=660).contains(&counts[0]),
            "weight 3 got {} of 800 tied picks",
            counts[0]
        );
    }

    #[test]
    fn weighted_least_conn_finds_weights_when_the_lists_are_not_aligned() {
        // The weighted list has a peer the candidate list lacks (filtered by capacity or ramp):
        // indices differ, the weight lookup must go by URL.
        let urls = vec!["http://b:4000".to_owned(), "http://c:4000".to_owned()];
        let weighted = vec![
            ("http://a:4000".to_owned(), 1),
            ("http://b:4000".to_owned(), 1),
            ("http://c:4000".to_owned(), 4),
        ];
        let reg = UpstreamRegistry::new();
        assert_eq!(spread(&reg, &urls, &weighted, 10), vec![2, 8]);
    }

    // ── filter_healthy ────────────────────────────────────────────────────────

    #[test]
    fn filter_healthy_returns_all_when_no_status_known() {
        let reg = UpstreamRegistry::new();
        let urls = vec!["http://a:4000".to_owned(), "http://b:4000".to_owned()];
        // No health status set → all optimistically healthy.
        let (healthy, fail_open) = reg.filter_healthy(&urls);
        assert_eq!(healthy.len(), 2);
        assert!(
            !fail_open,
            "optimistic-healthy is a real result, not a fail-open passthrough"
        );
    }

    #[test]
    fn filter_healthy_excludes_unhealthy() {
        let reg = UpstreamRegistry::new();
        let url_a = "http://a:4000".to_owned();
        let url_b = "http://b:4000".to_owned();
        reg.statuses.entry(url_a.clone()).or_default().healthy = false;
        let urls = vec![url_a, url_b.clone()];
        let (healthy, fail_open) = reg.filter_healthy(&urls);
        assert_eq!(healthy.len(), 1);
        assert_eq!(*healthy[0], url_b);
        assert!(!fail_open);
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
        let (result, fail_open) = reg.filter_healthy(&urls);
        // All unhealthy → fail-open: return all (try anyway).
        assert_eq!(
            result.len(),
            2,
            "fail-open: all must be returned when none healthy"
        );
        assert!(
            fail_open,
            "#374: callers must be able to tell this was fail-open"
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
