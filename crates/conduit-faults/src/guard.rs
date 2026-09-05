use std::sync::atomic::Ordering;

use async_trait::async_trait;
use bytes::Bytes;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use conduit_core::handler::response;
use pingora_core::Result;

use crate::config::FaultInjectionConfig;

/// Injects artificial faults (aborts or delays) for chaos-engineering and
/// testing retry/circuit-breaker behaviour.
///
/// **Should not be used in production.**  Use it in staging or test
/// environments to validate that your clients handle upstream failures
/// gracefully.
pub struct FaultInjectionGuard {
    pub cfg: FaultInjectionConfig,
}

/// Pure abort/delay decision, extracted from [`FaultInjectionGuard::apply`]
/// for testability — takes `roll` directly instead of computing it
/// internally from wall-clock time, so a test can exercise exact boundary
/// values deterministically.
///
/// Issue #282: the delay check used to compare `roll < delay.percent`
/// independently of the abort check, so the two percentage ranges
/// overlapped instead of being additive. With `abort.percent: 5,
/// delay.percent: 10`, rolls in `[0, 5)` always hit the abort branch first
/// — only rolls in `[5, 10)` ever reached the delay check, so only 5% of
/// requests were delayed, not the configured 10%. Offsetting the delay
/// threshold by `abort.percent` (0.0 when abort isn't configured) makes the
/// delay range start exactly where the abort range ends —
/// `[abort_percent, abort_percent + delay.percent)` — disjoint from the
/// abort range and restoring the configured delay proportion.
#[derive(Debug, PartialEq)]
enum FaultDecision {
    Continue,
    Delay { ms: u64 },
    Abort { status: u16, body: String },
}

fn decide(cfg: &FaultInjectionConfig, roll: f64) -> FaultDecision {
    if let Some(ref abort) = cfg.abort {
        if roll < abort.percent {
            let status = abort.status.unwrap_or(503).clamp(100, 999);
            let body = abort
                .body
                .clone()
                .unwrap_or_else(|| "Fault injected".to_owned());
            return FaultDecision::Abort { status, body };
        }
    }

    if let Some(ref delay) = cfg.delay {
        let abort_percent = cfg.abort.as_ref().map_or(0.0, |a| a.percent);
        if roll < abort_percent + delay.percent {
            return FaultDecision::Delay { ms: delay.ms };
        }
    }

    FaultDecision::Continue
}

#[async_trait]
impl RequestFilter for FaultInjectionGuard {
    async fn apply<'a>(&self, ctx: &mut FilterContext<'a>) -> Result<FilterOutcome> {
        // Use a simple pseudo-random roll based on the current time nanoseconds.
        // Good enough for percentage-based fault injection; not cryptographically
        // random, but that's not required here.
        let roll: f64 = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as f64;
            (ns % 10_000.0) / 100.0 // 0.0 – 99.99
        };

        match decide(&self.cfg, roll) {
            FaultDecision::Abort { status, body } => {
                response::write_response(
                    ctx.session,
                    status,
                    "text/plain",
                    Bytes::from(body),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                Ok(FilterOutcome::Handled)
            }
            FaultDecision::Delay { ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                Ok(FilterOutcome::Continue)
            }
            FaultDecision::Continue => Ok(FilterOutcome::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::config::{FaultAbort, FaultDelay};

    /// Build a real [`pingora_proxy::Session`] with a parsed GET request
    /// already read off the wire, so guards that touch `ctx.session`
    /// (`req_header()`, `write_response`, …) can be exercised as real unit
    /// tests instead of only via `tests/*.rs` integration tests (which
    /// `cargo llvm-cov --lib` never instruments — see #248).
    ///
    /// `pingora_proxy::Session::new_h1` is a public constructor whose own
    /// doc comment says it's "mostly used for testing and mocking". It just
    /// needs a `Stream` (`Box<dyn IO>`); pingora-core implements `IO` for
    /// `tokio::io::DuplexStream` specifically for this purpose (see
    /// `pingora_core::protocols::mod::ext_io_impl`, "mostly for testing") —
    /// no real socket needed, just an in-memory pipe.
    async fn session_with_request() -> (pingora_proxy::Session, tokio::io::DuplexStream) {
        let (server_side, mut client_side) = tokio::io::duplex(4096);
        client_side
            .write_all(b"GET /test HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();

        let stream: pingora_core::protocols::Stream = Box::new(server_side);
        let mut session = pingora_proxy::Session::new_h1(stream);
        session
            .as_downstream_mut()
            .read_request()
            .await
            .expect("read_request");

        (session, client_side)
    }

    fn guard_with(cfg: FaultInjectionConfig) -> FaultInjectionGuard {
        FaultInjectionGuard { cfg }
    }

    #[tokio::test]
    async fn abort_at_100_percent_always_returns_handled() {
        let (mut session, mut client_sock) = session_with_request().await;
        let guard = guard_with(FaultInjectionConfig {
            abort: Some(FaultAbort {
                percent: 100.0,
                status: Some(503),
                body: Some("boom".to_owned()),
            }),
            delay: None,
        });
        let inflight = AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Handled));
        assert_eq!(inflight.load(Ordering::Relaxed), 0);

        // Verify the actual bytes written back to the client carry the
        // configured status and body, not just the outcome enum.
        let mut buf = vec![0u8; 512];
        let n = client_sock.read(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 503"), "got: {response}");
        assert!(response.contains("boom"), "got: {response}");
    }

    #[tokio::test]
    async fn abort_uses_default_status_and_body_when_unset() {
        let (mut session, mut client_sock) = session_with_request().await;
        let guard = guard_with(FaultInjectionConfig {
            abort: Some(FaultAbort {
                percent: 100.0,
                status: None,
                body: None,
            }),
            delay: None,
        });
        let inflight = AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Handled));

        let mut buf = vec![0u8; 512];
        let n = client_sock.read(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 503"), "got: {response}");
        assert!(response.contains("Fault injected"), "got: {response}");
    }

    #[tokio::test]
    async fn delay_at_100_percent_sleeps_then_continues() {
        let (mut session, _client_sock) = session_with_request().await;
        let guard = guard_with(FaultInjectionConfig {
            abort: None,
            delay: Some(FaultDelay {
                percent: 100.0,
                ms: 1,
            }),
        });
        let inflight = AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Continue));
        // Delay path never touches inflight — only abort's Handled path does.
        assert_eq!(inflight.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn no_fault_configured_always_continues() {
        let (mut session, _client_sock) = session_with_request().await;
        let guard = guard_with(FaultInjectionConfig {
            abort: None,
            delay: None,
        });
        let inflight = AtomicUsize::new(1);
        let mut ctx = FilterContext {
            session: &mut session,
            extra_headers: &[],
            inflight: &inflight,
        };

        let outcome = guard.apply(&mut ctx).await.expect("apply");
        assert!(matches!(outcome, FilterOutcome::Continue));
    }

    // ── decide() — pure abort/delay boundary logic (issue #282) ─────────

    fn cfg_with(abort_percent: f64, delay_percent: f64, delay_ms: u64) -> FaultInjectionConfig {
        FaultInjectionConfig {
            abort: Some(FaultAbort {
                percent: abort_percent,
                status: None,
                body: None,
            }),
            delay: Some(FaultDelay {
                percent: delay_percent,
                ms: delay_ms,
            }),
        }
    }

    #[test]
    fn decide_abort_and_delay_ranges_are_additive_not_overlapping() {
        // abort=5%, delay=10% -> combined range is [0, 15): [0,5) aborts,
        // [5,15) delays. Before #282's fix, the delay check compared
        // `roll < delay.percent` (10) directly, so only [5,10) delayed --
        // 5% of requests, not the configured 10%.
        let cfg = cfg_with(5.0, 10.0, 42);

        // Squarely inside the abort range.
        assert!(matches!(decide(&cfg, 2.0), FaultDecision::Abort { .. }));
        // At the abort/delay boundary: exactly abort.percent must NOT abort
        // (abort's own check is `roll < percent`, strictly less-than) --
        // falls through to delay.
        assert_eq!(decide(&cfg, 5.0), FaultDecision::Delay { ms: 42 });
        // The actual regression case: under the pre-#282 bug this compared
        // `roll < delay.percent` = `12.0 < 10.0` = false -> Continue. The
        // fix compares `roll < abort_percent + delay.percent` =
        // `12.0 < 15.0` = true -> Delay, restoring the full 10%-wide range.
        assert_eq!(decide(&cfg, 12.0), FaultDecision::Delay { ms: 42 });
        // Just before the combined range ends (5 + 10 = 15).
        assert_eq!(decide(&cfg, 14.9), FaultDecision::Delay { ms: 42 });
        // At and beyond the combined range end -- neither fires.
        assert_eq!(decide(&cfg, 15.0), FaultDecision::Continue);
        assert_eq!(decide(&cfg, 50.0), FaultDecision::Continue);
    }

    #[test]
    fn decide_delay_only_unaffected_by_missing_abort() {
        // No abort configured -- abort_percent defaults to 0.0, so the
        // delay range is exactly [0, delay.percent), matching pre-#282
        // behavior for the no-abort case (this fix only changes behavior
        // when abort *is* configured).
        let cfg = FaultInjectionConfig {
            abort: None,
            delay: Some(FaultDelay {
                percent: 10.0,
                ms: 7,
            }),
        };
        assert_eq!(decide(&cfg, 0.0), FaultDecision::Delay { ms: 7 });
        assert_eq!(decide(&cfg, 9.9), FaultDecision::Delay { ms: 7 });
        assert_eq!(decide(&cfg, 10.0), FaultDecision::Continue);
    }

    #[test]
    fn decide_abort_status_and_body_defaults() {
        let cfg = FaultInjectionConfig {
            abort: Some(FaultAbort {
                percent: 100.0,
                status: None,
                body: None,
            }),
            delay: None,
        };
        assert_eq!(
            decide(&cfg, 0.0),
            FaultDecision::Abort {
                status: 503,
                body: "Fault injected".to_owned()
            }
        );
    }
}
