//! The probe state update and the background health-check and connection-warmup tasks.
//!
//! Split out of `health.rs` (issue #633); [`spawn_health_checks`] and [`spawn_connection_warmup`] are
//! re-exported from [`crate::health`], where callers have always found them.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::UpstreamHealthCheck;
use crate::health::{now_secs, UpstreamEntry, UpstreamRegistry};
use crate::targets;

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

/// Spawn a Tokio background task for every `(healthCheck config, target URLs)`
/// pair supplied.
///
/// Call once after the Tokio runtime is available (e.g. from the root
/// crate's `AdminApiService::start()`).  Tasks are fire-and-forget; they run
/// until the process exits.
///
/// Takes an iterator of already-resolved `(&UpstreamHealthCheck, &[String])`
/// pairs rather than the root crate's `AppConfig` directly: `conduit-upstream`
/// doesn't (and, per the crate-extraction layering rule, must not) depend on
/// `AppConfig`/`ProxyConfig`/`ProxyRouteTarget` — those are still root-crate-
/// only types (a later migration phase, #143/#144, extracts proxy routing
/// itself). The root crate's own call site (`admin/api.rs`'s
/// `spawn_upstream_probes()`, which builds the slice with
/// `health_check_routes()`) resolves `AppConfig` down to this shape before
/// calling in — the same pattern `conduit-hotreload`'s `build_watch_config`
/// uses for its own analogous `AppConfig`-dependency problem (issue
/// #114/#140).
pub fn spawn_health_checks<'a, I>(registry: Arc<UpstreamRegistry>, routes: I)
where
    I: IntoIterator<Item = (&'a UpstreamHealthCheck, &'a [String])>,
{
    for (hc, urls) in routes {
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
            urls.to_vec(),
            path,
            hc.interval_secs.unwrap_or(10).max(1),
            hc.healthy_threshold.unwrap_or(1),
            hc.unhealthy_threshold.unwrap_or(3),
        );
    }
}

/// Spawn background connection warmup tasks for routes that have
/// `healthCheck.prewarmConnections` configured.
///
/// For each such route, sends `n` sequential HEAD requests to the configured
/// health-check path immediately after startup.
///
/// **`[🚫 BLOCKED]`** (2026-08-03 integrity audit; confirmed genuinely
/// blocked, not just unimplemented, 2026-09-06 — issue #158): each request
/// currently goes through its own freshly-built, short-lived `reqwest::Client`
/// (see `warmup_url`), which is dropped at the end of the loop body — it does
/// not populate Pingora's own upstream connector pool used by
/// `upstream_peer()` for real proxied traffic, so real user requests still
/// pay the full TCP-handshake cost.
///
/// Confirmed via `pingora-proxy` 0.8.1's vendored source
/// (`pingora-proxy-0.8.1/src/lib.rs`): `HttpProxy<SV, C>::client_upstream`
/// (the actual `Connector` whose keepalive pool `upstream_peer()`'s request
/// path reads from) is a **private field with no public accessor** anywhere
/// in the struct's `impl` blocks, and the `ProxyHttp` trait Conduit
/// implements never receives a reference to it in any hook. There is no
/// public API in Pingora 0.8 to reach, share, or pre-populate that specific
/// pool from outside the crate (re-checked against 0.9.0 when Conduit moved
/// to it: the field is still private, still no accessor) — the same class of
/// gap as this repo's other `[🚫 BLOCKED]` items (OCSP stapling, request
/// queue + backpressure), waiting on a future Pingora release. Left in place
/// because it's otherwise harmless (a handful
/// of HEAD requests at startup); does not deliver the latency benefit its
/// name implies, and cannot until Pingora exposes the real pool.
///
/// Takes the same `(&UpstreamHealthCheck, &[String])` iterator shape as
/// [`spawn_health_checks`], for the same reason — see that function's doc
/// comment.
#[cfg(feature = "proxy")]
pub fn spawn_connection_warmup<'a, I>(routes: I)
where
    I: IntoIterator<Item = (&'a UpstreamHealthCheck, &'a [String])>,
{
    for (hc, urls) in routes {
        let n = match hc.prewarm_connections {
            Some(0) | None => continue,
            Some(n) => n.min(8) as usize,
        };
        let path = hc.path.clone().unwrap_or_else(|| "/".to_string());
        for url in urls {
            let path = path.clone();
            tokio::spawn(warmup_url(url.clone(), path, n));
        }
    }
}

/// Perform `n` sequential HEAD requests to warm up the connection pool for `url`.
#[cfg(feature = "proxy")]
async fn warmup_url(url: String, path: String, n: usize) {
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
                let Some(host_port) = targets::url_to_host_port(url) else {
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
pub(crate) async fn probe_http(host_port: &str, path: &str) -> (bool, u64) {
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
