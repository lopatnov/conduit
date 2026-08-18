//! Logging-phase orchestration for [`ConduitProxy`].
//!
//! This module hosts the `logging()` trait-method body that used to live
//! directly in `service.rs` (called from a thin delegator in the
//! `impl ProxyHttp for ConduitProxy` block):
//!
//! - inflight / retry-budget / per-upstream connection-count decrements
//! - passive health updates (Peak EWMA latency, outlier-detection ejection)
//! - access-log writing
//! - Prometheus metric recording (request, upstream, and cache counters)
//! - early cache-refresh task spawn and OTel span finish
//!
//! `logging()` itself is a flat orchestrator — each bullet above lives in its
//! own helper function below.

use std::sync::atomic::Ordering;

use pingora_proxy::Session;

use crate::filter::logging;
use crate::proxy::ctx::{RequestCtx, UpstreamTarget};
#[cfg(feature = "cache")]
use crate::proxy::response_phase::fire_early_refresh;
use crate::proxy::service::ConduitProxy;

/// Body of [`pingora_proxy::ProxyHttp::logging`].
pub(super) async fn logging(
    proxy: &ConduitProxy,
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
) {
    // Decrement inflight for proxy requests (local handlers decrement inline).
    proxy.state.metrics.active_connections.dec();
    // The per-IP connection slot is released automatically here:
    // RequestCtx.ip_conn_slot (IpConnSlotGuard) is dropped when ctx is
    // cleared at the end of this function, so no manual fetch_sub needed.
    release_proxy_upstream(proxy, session, ctx);

    write_access_log_entry(proxy, session, ctx);

    // Record Prometheus metrics.
    // `method`/`status`/`route` borrow from `session` instead of allocating
    // owned `String`s, and `status_u16` is computed once and reused below
    // instead of round-tripping through `.to_string()` + `.parse::<u16>()`.
    let method = session.req_header().method.as_str();
    let response_written = session.response_written();
    let status_u16 = response_written.map(|h| h.status.as_u16()).unwrap_or(0);
    let status = response_written.map(|h| h.status.as_str()).unwrap_or("0");
    let elapsed = ctx
        .as_ref()
        .map(|c| c.start_time.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    record_request_metrics(proxy, session, ctx, method, status, status_u16, elapsed);

    #[cfg(feature = "cache")]
    spawn_early_cache_refresh(session, ctx);

    #[cfg(feature = "otlp")]
    finish_otel_span(session, ctx, method, status_u16, elapsed);
}

/// Release proxy-upstream bookkeeping for a completed request: inflight /
/// retry-budget decrements, the per-upstream connection slot (least-conn),
/// Peak EWMA latency + passive health tracking, and outlier-detection ejection.
///
/// No-op for local handlers (they decrement inline) and for requests that
/// never selected an upstream.
fn release_proxy_upstream(proxy: &ConduitProxy, session: &Session, ctx: &Option<RequestCtx>) {
    let Some(req_ctx) = ctx.as_ref() else {
        return;
    };
    if matches!(req_ctx.upstream, UpstreamTarget::Local(_)) {
        return;
    }
    proxy.state.inflight.fetch_sub(1, Ordering::Relaxed);
    // Decrement retry budget counter if this request was a retry.
    if req_ctx
        .retry
        .as_ref()
        .map(|r| r.is_retrying)
        .unwrap_or(false)
    {
        proxy.state.retry_inflight.fetch_sub(1, Ordering::Relaxed);
    }
    // proxy_upstream_url is populated for every proxied request (#155) so
    // passive-health attribution below runs regardless of strategy; only
    // release the conn_count slot when this request actually holds one.
    let Some(ref url) = req_ctx.proxy_upstream_url else {
        return;
    };
    if req_ctx.upstream_conn_slot {
        proxy.state.upstream_health.conn_dec(url);
    }

    // Update Peak EWMA latency and passive health tracking.
    let elapsed_us = req_ctx.start_time.elapsed().as_micros() as u64;
    let status = session
        .response_written()
        .map(|h| h.status.as_u16())
        .unwrap_or(0);
    let effective_status = passive_effective_status(req_ctx, url, status, elapsed_us);

    crate::proxy::health::record_request_latency(
        &proxy.state.upstream_health,
        url,
        elapsed_us,
        effective_status,
    );

    // Outlier Detection: eject upstream after threshold 5xx responses.
    let config = proxy.state.config.load();
    let site = config.sites.get(req_ctx.site_idx);
    if let Some(od) = site.and_then(|s| s.outlier_detection.as_ref()) {
        crate::proxy::health::maybe_eject(&proxy.state.upstream_health, url, od);
    }
}

/// Passive health check (Caddy pattern):
/// Count the response as a failure when it matches `unhealthyStatus` or
/// exceeds `unhealthyLatencyMs` — even if the HTTP status is 2xx.
/// A failure is signalled to `record_request_latency` by returning 503.
fn passive_effective_status(req_ctx: &RequestCtx, url: &str, status: u16, elapsed_us: u64) -> u16 {
    let latency_ms = elapsed_us / 1000;
    let unhealthy_by_status = !req_ctx.passive_unhealthy_status.is_empty()
        && req_ctx.passive_unhealthy_status.contains(&status);
    let unhealthy_by_latency = req_ctx
        .passive_unhealthy_latency_ms
        .map(|t| latency_ms > t)
        .unwrap_or(false);
    if (unhealthy_by_status || unhealthy_by_latency) && status < 500 {
        // Override: treat as server error for consecutive_5xx tracking.
        tracing::debug!(
            upstream = %url, status, latency_ms,
            "passive health: counting response as failure \
             (unhealthyStatus={unhealthy_by_status}, \
              unhealthyLatency={unhealthy_by_latency})"
        );
        503
    } else {
        status
    }
}

/// Write the access-log entry for this request.
fn write_access_log_entry(proxy: &ConduitProxy, session: &Session, ctx: &Option<RequestCtx>) {
    let start_time = ctx
        .as_ref()
        .map(|c| c.start_time)
        .unwrap_or_else(std::time::Instant::now);
    let config = proxy.state.config.load();
    let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
    let site = config.sites.get(site_idx);
    let request_id = session
        .req_header()
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok());
    let upstream_addr = ctx.as_ref().and_then(|c| c.proxy_upstream_url.as_deref());
    let upstream_ms = ctx
        .as_ref()
        .and_then(|c| c.upstream_start)
        .map(|t| t.elapsed().as_millis() as u64);
    logging::write_access_log(
        session,
        start_time,
        site.and_then(|s| s.logging.as_ref()),
        &proxy.state.log_writer,
        &logging::AccessLogContext {
            request_id,
            upstream_addr,
            upstream_ms,
        },
    );
}

/// Record Prometheus request / upstream / cache metrics for this request.
fn record_request_metrics(
    proxy: &ConduitProxy,
    session: &Session,
    ctx: &Option<RequestCtx>,
    method: &str,
    status: &str,
    status_u16: u16,
    elapsed: f64,
) {
    proxy
        .state
        .metrics
        .requests_total
        .with_label_values(&[method, status])
        .inc();
    proxy
        .state
        .metrics
        .request_duration_seconds
        .with_label_values(&[method, status])
        .observe(elapsed);

    // Upstream error counter (5xx status codes from upstream).
    if status_u16 >= 500 {
        let route = session.req_header().uri.path();
        proxy
            .state
            .metrics
            .upstream_errors_total
            .with_label_values(&[route, status])
            .inc();
    }

    record_upstream_metrics(proxy, ctx, status, status_u16);
    record_cache_metrics(proxy, session, ctx);
}

/// Per-upstream metrics: active-connections decrement, requests_total,
/// latency_seconds, and the per-peer response-code breakdown (#40).
fn record_upstream_metrics(
    proxy: &ConduitProxy,
    ctx: &Option<RequestCtx>,
    status: &str,
    status_u16: u16,
) {
    let Some(url) = ctx.as_ref().and_then(|c| c.proxy_upstream_url.as_deref()) else {
        return;
    };
    // Decrement the active-connections gauge now that this request finished.
    proxy
        .state
        .metrics
        .upstream_active_connections
        .with_label_values(&[url])
        .dec();

    proxy
        .state
        .metrics
        .upstream_requests_total
        .with_label_values(&[url, status])
        .inc();

    // Per-peer response code breakdown (#40).
    crate::proxy::health::record_response_status(&proxy.state.upstream_health, url, status_u16);
    if let Some(upstream_secs) = ctx
        .as_ref()
        .and_then(|c| c.upstream_start)
        .map(|t| t.elapsed().as_secs_f64())
    {
        proxy
            .state
            .metrics
            .upstream_latency_seconds
            .with_label_values(&[url])
            .observe(upstream_secs);
    }
}

/// Cache hit / miss counters (only for proxy requests with caching enabled).
fn record_cache_metrics(proxy: &ConduitProxy, session: &Session, ctx: &Option<RequestCtx>) {
    if ctx
        .as_ref()
        .and_then(|c| c.proxy_cache_cfg.as_ref())
        .is_none()
    {
        return;
    }
    use pingora_cache::CachePhase;
    let route = session.req_header().uri.path();
    match session.cache.phase() {
        CachePhase::Hit => {
            proxy
                .state
                .metrics
                .cache_hits_total
                .with_label_values(&[route])
                .inc();
        }
        CachePhase::Miss | CachePhase::Expired => {
            proxy
                .state
                .metrics
                .cache_misses_total
                .with_label_values(&[route])
                .inc();
        }
        _ => {}
    }
}

/// Early cache refresh (#31): when `response_filter` detected that the cached
/// entry is within the early-refresh window, spawn a fire-and-forget GET to
/// the upstream URL so the cache is refreshed before clients see a stale entry.
///
/// This runs in `logging()` rather than `response_filter` so the task is
/// spawned after the response is fully sent to the client, not while the main
/// request is still in flight.
#[cfg(feature = "cache")]
fn spawn_early_cache_refresh(session: &Session, ctx: &Option<RequestCtx>) {
    let Some(early_url) = ctx
        .as_ref()
        .and_then(|c| c.early_refresh_upstream_url.as_deref().map(str::to_owned))
    else {
        return;
    };
    let path = session
        .req_header()
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_default();
    tracing::debug!(upstream = %early_url, path = %path, "spawning early cache refresh");
    tokio::spawn(async move {
        fire_early_refresh(&early_url, &path).await;
    });
}

/// OpenTelemetry: finish the request span with all request attributes.
#[cfg(feature = "otlp")]
fn finish_otel_span(
    session: &Session,
    ctx: &mut Option<RequestCtx>,
    method: &str,
    status_u16: u16,
    elapsed: f64,
) {
    use opentelemetry::{trace::Span, KeyValue};
    let Some(req_ctx) = ctx.as_mut() else {
        return;
    };
    let Some(mut span) = req_ctx.otel_span.take() else {
        return;
    };
    span.set_attribute(KeyValue::new("http.method", method.to_string()));
    span.set_attribute(KeyValue::new(
        "http.path",
        session.req_header().uri.path().to_owned(),
    ));
    span.set_attribute(KeyValue::new("http.status_code", status_u16 as i64));
    span.set_attribute(KeyValue::new("http.duration_ms", (elapsed * 1000.0) as i64));
    // `finish_otel_span` is the last helper called in `logging()` and has
    // mutable access to `ctx`, so take the URL instead of cloning it — the
    // context is cleared by Pingora immediately after this returns.
    if let Some(url) = req_ctx.proxy_upstream_url.take() {
        span.set_attribute(KeyValue::new("upstream.url", url));
    }
    // Attach the X-Request-ID so the trace is correlatable with logs.
    if let Some(rid) = session
        .req_header()
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
    {
        span.set_attribute(KeyValue::new("request.id", rid.to_owned()));
    }
    // Mark 5xx as errors in the trace.
    if status_u16 >= 500 {
        span.set_status(opentelemetry::trace::Status::error("upstream returned 5xx"));
    }
    span.end();
}
