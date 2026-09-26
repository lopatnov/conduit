//! Request body buffering for retry replay (linkerd2-proxy `ReplayBody`
//! pattern) -- `request_body_filter` (the
//! [`pingora_proxy::ProxyHttp::request_body_filter`] trait-method body),
//! `enforce_max_body_bytes`, `buffer_body_chunk`.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.

use crate::proxy::ctx::RequestCtx;
use crate::proxy::service::ConduitProxy;

/// Body of [`pingora_proxy::ProxyHttp::request_body_filter`].
///
/// Buffer request body chunks for retry replay (stale-if-error pattern).
///
/// Only buffers when:
///   1. The route has `retry` configured.
///   2. The body is within `limits.maxBodyBufferBytes` (default 1 MiB).
///   3. `body_too_large` flag is not already set.
///
/// Uses the linkerd2-proxy ReplayBody pattern: accumulate `Bytes` chunks
/// (cheap reference-counted clones) into `RequestCtx.body_buffer`.  On
/// overflow the buffer is discarded and `body_too_large` is set — retries
/// still happen but without body replay.
pub(crate) async fn request_body_filter(
    proxy: &ConduitProxy,
    body: &mut Option<bytes::Bytes>,
    ctx: &mut Option<RequestCtx>,
) -> pingora_core::Result<()> {
    let Some(req_ctx) = ctx.as_mut() else {
        return Ok(());
    };

    let chunk_len = body.as_ref().map(|c| c.len()).unwrap_or(0);
    req_ctx.limits.actual_body_bytes += chunk_len as u64;

    // Enforce maxBodyBytes on the ACTUAL received bytes.
    // The LimitsGuard only checks the Content-Length header; chunked clients bypass it.
    if chunk_len > 0 {
        let max_body = {
            let config = proxy.state.config.load();
            config
                .sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.max_body_bytes)
        };
        if enforce_max_body_bytes(req_ctx, body, chunk_len, max_body) {
            return Ok(());
        }

        // Slow-loris upload defense: leaky-bucket minimum-rate check (#51).
        //
        // Pattern: freenginx `ngx_http_request_body.c` commit b85480cc
        //          (client_body_min_rate).
        //
        // Algorithm:
        //   `excess += chunk_bytes − min_rate × elapsed_secs`
        //
        // "Excess" is the surplus above the minimum rate (positive =
        // client sending fast, negative = client is behind).
        // Surplus is capped at one second's worth (min_rate bytes) to
        // prevent unlimited credit from fast initial bursts.
        // When the client falls more than one second behind
        // (excess < -min_rate) the connection is closed with 408.
        let min_rate_opt = {
            let config = proxy.state.config.load();
            config
                .sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.min_upload_rate_bytes_per_sec)
                .filter(|&r| r > 0)
        };
        if let Some(min_rate) = min_rate_opt {
            let now = std::time::Instant::now();
            // elapsed_secs = 0 on the first chunk so the chunk bytes are
            // credited to the bucket immediately (no time-based drain).
            let elapsed_secs = req_ctx
                .limits
                .upload_last_chunk
                .map(|last| now.duration_since(last).as_secs_f64())
                .unwrap_or(0.0);
            if crate::filter::limits::upload_rate_step(
                &mut req_ctx.limits.upload_excess_bytes,
                chunk_len,
                min_rate,
                elapsed_secs,
            ) {
                tracing::debug!(
                    min_rate,
                    excess = req_ctx.limits.upload_excess_bytes,
                    "upload rate below minimum — closing connection (408)"
                );
                *body = None;
                return Err(pingora_core::Error::explain(
                    pingora_core::ErrorType::HTTPStatus(408),
                    "upload rate below minUploadRateBytesPerSec",
                ));
            }
            req_ctx.limits.upload_last_chunk = Some(now);
        }
    }

    // Retry body buffering (separate from size enforcement).
    // Only buffer when retry is configured (otherwise wasteful).
    if req_ctx.proxy.retry.is_none() || req_ctx.body_too_large {
        return Ok(());
    }

    if let Some(chunk) = body.as_ref() {
        let max_bytes = {
            let config = proxy.state.config.load();
            config
                .sites
                .get(req_ctx.site_idx)
                .and_then(|s| s.limits.as_ref())
                .and_then(|l| l.max_body_buffer_bytes)
                .unwrap_or(1_048_576) // default 1 MiB
        };
        buffer_body_chunk(req_ctx, chunk, max_bytes);
    }
    Ok(())
}

/// Enforce the `maxBodyBytes` hard limit on actual received bytes.
///
/// Returns `true` when the limit was exceeded (caller should return early).
/// Mutates `body` to `None` to drop the chunk and sets `body_too_large` on the
/// context.  Logs a warning only on the first violation.
pub(super) fn enforce_max_body_bytes(
    req_ctx: &mut RequestCtx,
    body: &mut Option<bytes::Bytes>,
    chunk_len: usize,
    max_body: Option<u64>,
) -> bool {
    let Some(max) = max_body else {
        return false;
    };
    if req_ctx.limits.actual_body_bytes > max {
        // Drop this chunk — prevents forwarding to upstream.
        *body = None;
        let prev = req_ctx.limits.actual_body_bytes - chunk_len as u64;
        if prev <= max {
            // Log only on first violation.
            tracing::warn!(
                actual = req_ctx.limits.actual_body_bytes,
                max,
                "request body exceeded maxBodyBytes (chunked/no Content-Length) \
                 — body dropped, upstream will receive truncated request"
            );
        }
        req_ctx.body_buffer.clear();
        req_ctx.body_too_large = true;
        return true;
    }
    false
}

/// Buffer a body chunk for retry replay (linkerd ReplayBody pattern).
///
/// Clears the buffer and sets `body_too_large` when adding the chunk would
/// exceed `max_bytes`.
pub(super) fn buffer_body_chunk(req_ctx: &mut RequestCtx, chunk: &bytes::Bytes, max_bytes: u64) {
    let current_size: usize = req_ctx.body_buffer.iter().map(|b| b.len()).sum();
    if current_size + chunk.len() > max_bytes as usize {
        // Discard buffer — linkerd pattern: clear on overflow.
        req_ctx.body_buffer.clear();
        req_ctx.body_too_large = true;
        tracing::debug!(
            size = current_size + chunk.len(),
            max = max_bytes,
            "request body exceeded buffer limit — retry will not replay body"
        );
    } else {
        // Cheap clone: Bytes is reference-counted.
        req_ctx.body_buffer.push(chunk.clone());
    }
}
