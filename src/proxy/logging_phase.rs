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
//! Pure mechanical split from `service.rs` — no behavioral change.

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
    if let Some(req_ctx) = ctx.as_ref() {
        if !matches!(req_ctx.upstream, UpstreamTarget::Local(_)) {
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
            // For least-conn routes, release the per-upstream slot.
            if let Some(ref url) = req_ctx.proxy_upstream_url {
                proxy.state.upstream_health.conn_dec(url);

                // Update Peak EWMA latency and passive health tracking.
                let elapsed_us = req_ctx.start_time.elapsed().as_micros() as u64;
                let status = session
                    .response_written()
                    .map(|h| h.status.as_u16())
                    .unwrap_or(0);

                // Passive health check (Caddy pattern):
                // Count response as failure when it matches unhealthyStatus or
                // exceeds unhealthyLatencyMs — even if the HTTP status is 2xx.
                // We signal a failure to record_request_latency by passing 503.
                let effective_status = {
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
                };

                crate::proxy::health::record_request_latency(
                    &proxy.state.upstream_health,
                    url,
                    elapsed_us,
                    effective_status,
                );

                // Outlier Detection: eject upstream after threshold 5xx responses.
                {
                    let config = proxy.state.config.load();
                    let site = config.sites.get(req_ctx.site_idx);
                    let od_cfg = site.and_then(|s| s.outlier_detection.as_ref());
                    if let Some(od) = od_cfg {
                        crate::proxy::health::maybe_eject(&proxy.state.upstream_health, url, od);
                    }
                }
            }
        }
    }

    // Write access log entry.
    let start_time = ctx
        .as_ref()
        .map(|c| c.start_time)
        .unwrap_or_else(std::time::Instant::now);
    {
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
            site,
            &proxy.state.log_writer,
            &logging::AccessLogContext {
                request_id,
                upstream_addr,
                upstream_ms,
            },
        );
    }

    // Record Prometheus metrics.
    let method = session.req_header().method.as_str().to_owned();
    let status = session
        .response_written()
        .map(|h| h.status.as_u16().to_string())
        .unwrap_or_else(|| "0".to_owned());
    let elapsed = ctx
        .as_ref()
        .map(|c| c.start_time.elapsed().as_secs_f64())
        .unwrap_or(0.0);

    proxy
        .state
        .metrics
        .requests_total
        .with_label_values(&[&method, &status])
        .inc();
    proxy
        .state
        .metrics
        .request_duration_seconds
        .with_label_values(&[&method, &status])
        .observe(elapsed);

    // Upstream error counter (5xx status codes from upstream).
    {
        let status_u16 = status.parse::<u16>().unwrap_or(0);
        if status_u16 >= 500 {
            let route = session.req_header().uri.path().to_owned();
            proxy
                .state
                .metrics
                .upstream_errors_total
                .with_label_values(&[&route, &status])
                .inc();
        }
    }

    // Per-upstream metrics: active_connections decrement, requests_total, latency_seconds.
    if let Some(url) = ctx.as_ref().and_then(|c| c.proxy_upstream_url.as_deref()) {
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
            .with_label_values(&[url, &status])
            .inc();

        // Per-peer response code breakdown (#40).
        let status_u16 = status.parse::<u16>().unwrap_or(0);
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

    // Cache hit / miss counters (only for proxy requests with caching enabled).
    if ctx
        .as_ref()
        .and_then(|c| c.proxy_cache_cfg.as_ref())
        .is_some()
    {
        use pingora_cache::CachePhase;
        let route = session.req_header().uri.path().to_owned();
        match session.cache.phase() {
            CachePhase::Hit => {
                proxy
                    .state
                    .metrics
                    .cache_hits_total
                    .with_label_values(&[&route])
                    .inc();
            }
            CachePhase::Miss | CachePhase::Expired => {
                proxy
                    .state
                    .metrics
                    .cache_misses_total
                    .with_label_values(&[&route])
                    .inc();
            }
            _ => {}
        }
    }

    // ── Early cache refresh (#31) ─────────────────────────────────────────
    // When `response_filter` detected that the cached entry is within the
    // early-refresh window, spawn a fire-and-forget GET to the upstream URL
    // so the cache is refreshed before clients see a stale entry.
    //
    // We intentionally do this in `logging()` rather than `response_filter`
    // so the task is spawned after the response is fully sent to the client,
    // not while the main request is still in flight.
    #[cfg(feature = "cache")]
    if let Some(early_url) = ctx
        .as_ref()
        .and_then(|c| c.early_refresh_upstream_url.as_deref().map(str::to_owned))
    {
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

    // ── OpenTelemetry: finish span with all request attributes ────────────
    #[cfg(feature = "otlp")]
    if let Some(req_ctx) = ctx.as_mut() {
        if let Some(mut span) = req_ctx.otel_span.take() {
            use opentelemetry::{trace::Span, KeyValue};
            let status_u16 = status.parse::<u16>().unwrap_or(0);
            span.set_attribute(KeyValue::new("http.method", method.clone()));
            span.set_attribute(KeyValue::new(
                "http.path",
                session.req_header().uri.path().to_owned(),
            ));
            span.set_attribute(KeyValue::new("http.status_code", status_u16 as i64));
            span.set_attribute(KeyValue::new("http.duration_ms", (elapsed * 1000.0) as i64));
            if let Some(ref url) = req_ctx.proxy_upstream_url {
                span.set_attribute(KeyValue::new("upstream.url", url.clone()));
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
    }
}
