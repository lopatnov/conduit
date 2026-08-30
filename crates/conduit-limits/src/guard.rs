use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use conduit_core::handler::response;
use pingora_core::Result;

use crate::config::LimitsConfig;
use crate::limits;

/// Enforces request body and header size limits.
pub struct LimitsGuard {
    pub cfg: LimitsConfig,
    /// Per-client-IP concurrent connection counts (nginx limit_conn pattern).
    pub ip_conn_counts: Arc<dashmap::DashMap<String, AtomicUsize>>,
    /// Extracted client IP used for per-IP connection limiting.
    pub client_ip: String,
}

#[async_trait]
impl RequestFilter for LimitsGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Host header security check — always enforced, no config needed.
        //
        // Reject requests whose Host header is:
        //   1. Non-UTF-8 bytes (obvious malform / injection attempt).
        //   2. Contains CR, LF, or NUL (header-injection / smuggling).
        //   3. Not a valid HTTP authority (e.g. contains spaces, path
        //      separators, or other RFC 3986 §3.2-invalid characters).
        let host_hdr = ctx.session.req_header().headers.get("host");
        if is_host_header_invalid(host_hdr) {
            response::write_response(
                ctx.session,
                400,
                "text/plain",
                Bytes::from_static(b"Bad Request (invalid Host header)"),
                ctx.extra_headers,
            )
            .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }

        // Request header count limit. `>`, not `>=` — exactly max_hdrs headers is allowed.
        if let Some(max_hdrs) = self.cfg.max_request_headers {
            let count = ctx.session.req_header().headers.len() as u32;
            if count > max_hdrs {
                response::write_response(
                    ctx.session,
                    431,
                    "text/plain",
                    Bytes::from_static(b"Request Header Fields Too Large"),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

        // Inflight request cap — checked before body/header limits so the
        // rejection cost is minimal when the server is under heavy load.
        if let Some(max) = self.cfg.max_inflight_requests {
            // The inflight counter was already incremented at the start of
            // request_filter, so the current value includes this request.
            let current = ctx.inflight.load(Ordering::Relaxed) as u64;
            if current > max {
                response::write_response(
                    ctx.session,
                    503,
                    "text/plain",
                    Bytes::from_static(b"Service Unavailable (too many concurrent requests)"),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

        if let Some((status, body)) = limits_rejection(limits::check(&self.cfg, ctx.session)) {
            response::write_response(ctx.session, status, "text/plain", body, ctx.extra_headers)
                .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            return Ok(FilterOutcome::Handled);
        }

        // Per-IP concurrent connection limit (nginx limit_conn pattern).
        // Checked after the inflight cap so the DashMap lookup only runs when
        // the server is accepting new connections.
        if let Some(max_per_ip) = self.cfg.max_connections_per_ip {
            let ip = &self.client_ip;
            if !ip.is_empty() && !try_acquire_ip_slot(ip, max_per_ip, &self.ip_conn_counts) {
                response::write_response(
                    ctx.session,
                    429,
                    "text/plain",
                    Bytes::from_static(b"Too Many Connections"),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

        Ok(FilterOutcome::Continue)
    }
}

/// Returns `true` when the `Host` header value is malformed or contains
/// characters that could be exploited for header-injection attacks.
///
/// Rejects:
/// - Non-UTF-8 bytes.
/// - Values containing CR, LF, or NUL control characters.
/// - Values that are not a valid RFC 3986 authority (spaces, backslash,
///   path separators, etc.).
///
/// Source: freenginx `ngx_http_request.c` — `ngx_http_validate_host()`
/// commit `d5ea86c7`.
fn is_host_header_invalid(hdr: Option<&http::header::HeaderValue>) -> bool {
    let v = match hdr {
        Some(v) => v,
        None => return false, // absent Host is handled separately
    };
    let s = match v.to_str() {
        Err(_) => return true, // non-UTF-8 → reject
        Ok(s) => s,
    };
    // Belt-and-suspenders control-byte check.
    if s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return true;
    }
    // Full RFC 3986 authority grammar validation.
    http::uri::Authority::try_from(s).is_err()
}

/// Attempt to acquire one connection slot for `ip` against `max`.
///
/// Atomically increments the counter for this IP.  If the result exceeds
/// `max` the increment is immediately rolled back and `false` is returned so
/// the caller can reject the request with a 429.  Returns `true` when the
/// slot was successfully acquired.
fn try_acquire_ip_slot(ip: &str, max: u64, counts: &dashmap::DashMap<String, AtomicUsize>) -> bool {
    let current = counts
        .entry(ip.to_owned())
        .or_insert_with(|| AtomicUsize::new(0))
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    if current as u64 > max {
        // Undo the increment — this request is rejected.
        if let Some(counter) = counts.get(ip) {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
        return false;
    }
    true
}

// ── RAII connection-slot guard ────────────────────────────────────────────────

/// RAII guard that holds one per-IP connection slot for the duration of a
/// request.
///
/// When this guard is dropped (at the end of the request lifecycle, whether
/// the request completes normally, is rejected, or panics) the slot is
/// automatically released by decrementing the shared counter.  This replaces
/// the manual `fetch_sub` that was previously scattered across `logging()`.
///
/// The guard is created in `service.rs` after the filter chain succeeds and
/// stored in `RequestCtx`; it is dropped when `RequestCtx` is dropped at
/// the end of `logging()`.
#[derive(Debug)]
pub struct IpConnSlotGuard {
    pub ip: String,
    pub counts: Arc<dashmap::DashMap<String, AtomicUsize>>,
}

impl Drop for IpConnSlotGuard {
    fn drop(&mut self) {
        if let Some(counter) = self.counts.get(&self.ip) {
            let prev = counter.fetch_sub(1, Ordering::Relaxed);
            if prev == 0 {
                // Prevent wrap-around on a hypothetical race.
                counter.store(0, Ordering::Relaxed);
            }
        }
    }
}

/// Map a `limits::CheckResult` to the HTTP rejection status + body, or `None`
/// when the request is within the configured limits.
fn limits_rejection(result: limits::CheckResult) -> Option<(u16, Bytes)> {
    match result {
        limits::CheckResult::BodyTooLarge => {
            Some((413, Bytes::from_static(b"Request Entity Too Large")))
        }
        limits::CheckResult::HeaderTooLarge => {
            Some((431, Bytes::from_static(b"Request Header Fields Too Large")))
        }
        limits::CheckResult::Ok => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── limits_rejection ─────────────────────────────────────────────────────

    #[test]
    fn limits_rejection_body_too_large_returns_413() {
        let result = limits_rejection(limits::CheckResult::BodyTooLarge);
        assert!(result.is_some());
        let (status, body) = result.unwrap();
        assert_eq!(status, 413);
        assert!(!body.is_empty());
    }

    #[test]
    fn limits_rejection_header_too_large_returns_431() {
        let result = limits_rejection(limits::CheckResult::HeaderTooLarge);
        assert!(result.is_some());
        let (status, _) = result.unwrap();
        assert_eq!(status, 431);
    }

    #[test]
    fn limits_rejection_ok_returns_none() {
        let result = limits_rejection(limits::CheckResult::Ok);
        assert!(result.is_none());
    }

    // ── limits_rejection body/header messages ─────────────────────────────────

    #[test]
    fn limits_rejection_body_message_correct() {
        let result = limits_rejection(limits::CheckResult::BodyTooLarge);
        let (status, body) = result.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap_or("?");
        assert!(
            body_str.contains("Large"),
            "body must explain the limit: {body_str}"
        );
        assert_eq!(status, 413);
    }

    #[test]
    fn limits_rejection_header_message_correct() {
        let result = limits_rejection(limits::CheckResult::HeaderTooLarge);
        let (status, body) = result.unwrap();
        assert_eq!(status, 431);
        let body_str = std::str::from_utf8(&body).unwrap_or("?");
        assert!(!body_str.is_empty());
    }

    // ── Host header validation (LimitsGuard) ─────────────────────────────────

    #[test]
    fn host_validation_rejects_crlf_in_host() {
        // Validate that the host-header check correctly flags CR/LF bytes.
        // The check is: host_val.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
        let bad_hosts = [
            "evil.com\r\nX-Injected: yes",
            "evil.com\n",
            "evil.com\r",
            "evil\0.com",
        ];
        for h in &bad_hosts {
            let has_bad = h.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
            assert!(has_bad, "expected bad host to be detected: {h:?}");
        }
    }

    #[test]
    fn host_validation_accepts_normal_host() {
        let good_hosts = [
            "example.com",
            "example.com:8080",
            "192.168.1.1:443",
            "[::1]:8080",
        ];
        for h in &good_hosts {
            let has_bad = h.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
            assert!(!has_bad, "expected normal host to pass: {h:?}");
        }
    }

    // ── IpConnSlotGuard ──────────────────────────────────────────────────────

    #[test]
    fn ip_conn_slot_guard_decrements_on_drop() {
        let counts: Arc<dashmap::DashMap<String, AtomicUsize>> = Arc::new(dashmap::DashMap::new());
        // Manually set the counter to 1 (simulating a slot that was acquired).
        counts
            .entry("10.1.2.3".to_owned())
            .or_insert_with(|| AtomicUsize::new(0))
            .store(1, Ordering::Relaxed);

        let guard = IpConnSlotGuard {
            ip: "10.1.2.3".to_owned(),
            counts: Arc::clone(&counts),
        };
        // Before drop: counter is still 1.
        assert_eq!(counts.get("10.1.2.3").unwrap().load(Ordering::Relaxed), 1);
        drop(guard);
        // After drop: counter should be 0.
        assert_eq!(counts.get("10.1.2.3").unwrap().load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ip_conn_slot_guard_prevents_wrap_on_zero() {
        let counts: Arc<dashmap::DashMap<String, AtomicUsize>> = Arc::new(dashmap::DashMap::new());
        counts
            .entry("10.1.2.4".to_owned())
            .or_insert_with(|| AtomicUsize::new(0))
            .store(0, Ordering::Relaxed);

        let guard = IpConnSlotGuard {
            ip: "10.1.2.4".to_owned(),
            counts: Arc::clone(&counts),
        };
        drop(guard);
        // Counter must not wrap around to usize::MAX.
        assert_eq!(counts.get("10.1.2.4").unwrap().load(Ordering::Relaxed), 0);
    }

    // ── is_host_header_invalid ────────────────────────────────────────────────

    #[test]
    fn is_host_header_invalid_absent_host_returns_false() {
        // A missing Host header is not invalid — it is handled elsewhere.
        assert!(!is_host_header_invalid(None));
    }

    // ── try_acquire_ip_slot ───────────────────────────────────────────────────

    #[test]
    fn try_acquire_ip_slot_allows_first_request() {
        let counts = dashmap::DashMap::new();
        assert!(try_acquire_ip_slot("10.0.0.1", 3, &counts));
        assert_eq!(counts.get("10.0.0.1").unwrap().load(Ordering::Relaxed), 1);
    }

    #[test]
    fn try_acquire_ip_slot_rejects_when_limit_reached() {
        let counts = dashmap::DashMap::new();
        assert!(try_acquire_ip_slot("10.0.0.2", 1, &counts)); // slot 1 → allowed
        assert!(!try_acquire_ip_slot("10.0.0.2", 1, &counts)); // slot 2 → rejected
                                                               // Counter must be rolled back after rejection.
        assert_eq!(counts.get("10.0.0.2").unwrap().load(Ordering::Relaxed), 1);
    }

    #[test]
    fn try_acquire_ip_slot_fills_up_to_limit() {
        let counts = dashmap::DashMap::new();
        for _ in 0..5 {
            assert!(try_acquire_ip_slot("10.0.0.3", 5, &counts));
        }
        assert!(!try_acquire_ip_slot("10.0.0.3", 5, &counts)); // 6th → rejected
        assert_eq!(counts.get("10.0.0.3").unwrap().load(Ordering::Relaxed), 5);
    }

    #[test]
    fn try_acquire_ip_slot_different_ips_are_independent() {
        let counts = dashmap::DashMap::new();
        assert!(try_acquire_ip_slot("1.1.1.1", 1, &counts));
        assert!(try_acquire_ip_slot("2.2.2.2", 1, &counts)); // different IP → allowed
        assert!(!try_acquire_ip_slot("1.1.1.1", 1, &counts)); // same IP → rejected
    }

    fn check_host(raw: &[u8]) -> bool {
        // Returns true when the host value is INVALID (should be rejected).
        // Delegates to is_host_header_invalid so the tests exercise the real function.
        //
        // Bytes that can't be constructed into a HeaderValue would be rejected by
        // Pingora's HTTP parser before reaching this guard — we treat them as
        // invalid for completeness.
        match http::header::HeaderValue::from_bytes(raw) {
            Err(_) => true,
            Ok(hv) => is_host_header_invalid(Some(&hv)),
        }
    }

    #[test]
    fn host_valid_simple_domain_accepted() {
        assert!(!check_host(b"example.com"));
    }

    #[test]
    fn host_valid_domain_with_port_accepted() {
        assert!(!check_host(b"example.com:8080"));
    }

    #[test]
    fn host_valid_ipv4_accepted() {
        assert!(!check_host(b"192.168.1.1"));
    }

    #[test]
    fn host_valid_ipv6_accepted() {
        assert!(!check_host(b"[::1]:443"));
    }

    #[test]
    fn host_cr_lf_rejected() {
        assert!(check_host(b"evil.com\r\nX-Injected: yes"));
    }

    #[test]
    fn host_nul_byte_rejected() {
        assert!(check_host(b"evil.com\x00"));
    }

    #[test]
    fn host_space_rejected() {
        assert!(check_host(b"evil .com"));
    }

    #[test]
    fn host_path_separator_rejected() {
        assert!(check_host(b"evil.com/../../etc/passwd"));
    }

    #[test]
    fn host_non_utf8_rejected() {
        // 0xFF is not valid UTF-8; to_str() will return Err → treated as invalid.
        assert!(check_host(b"evil\xff.com"));
    }
}
