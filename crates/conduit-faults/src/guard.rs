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

        // Abort injection — checked first.
        if let Some(ref abort) = self.cfg.abort {
            if roll < abort.percent {
                let status = abort.status.unwrap_or(503).clamp(100, 999);
                let body = abort
                    .body
                    .clone()
                    .unwrap_or_else(|| "Fault injected".to_owned());
                response::write_response(
                    ctx.session,
                    status,
                    "text/plain",
                    Bytes::from(body),
                    ctx.extra_headers,
                )
                .await?;
                ctx.inflight.fetch_sub(1, Ordering::Relaxed);
                return Ok(FilterOutcome::Handled);
            }
        }

        // Delay injection.
        if let Some(ref delay) = self.cfg.delay {
            if roll < delay.percent {
                tokio::time::sleep(std::time::Duration::from_millis(delay.ms)).await;
            }
        }

        Ok(FilterOutcome::Continue)
    }
}
