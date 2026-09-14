//! Retry decision logic, retry bookkeeping, and per-attempt conn-slot
//! (#216) / capacity (#216 part 2) admission for retry-configured routes.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.
//!
//! ~540 production lines, over `conventions.md`'s 400-line soft limit --
//! deliberately kept as one file rather than split further: retry decision
//! logic, retry bookkeeping, and conn-slot/capacity admission are one
//! tightly-coupled concern (see `select_retry_target`'s own doc comment for
//! how directly `acquire_conn_slot`/`release_conn_slot` and the retry-index
//! formula depend on each other) -- splitting them would spread one
//! invariant across files rather than reduce complexity.

use std::sync::atomic::Ordering;
use std::time::Duration;

use pingora_core::upstreams::peer::HttpPeer;
use pingora_proxy::Session;

use crate::proxy::ctx::{RequestCtx, RetryState};
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::service::ConduitProxy;

/// Synthetic status fed to `record_request_latency` for a connect-phase or
/// proxy-phase-timeout retry failure, neither of which has a real HTTP
/// status (#216 findings C/D). Must be `>= 500` -- `record_request_latency`
/// treats anything below that as a success for `consecutive_5xx` purposes
/// and resets the counter to zero, which would actively defeat outlier
/// detection rather than merely fail to help it (Gitar finding on PR #371).
const SYNTHETIC_RETRY_FAILURE_STATUS: u16 = 503;

impl ConduitProxy {
    /// Record passive health (EWMA latency/`consecutive_5xx` via
    /// `record_request_latency`, outlier-detection ejection) and, when
    /// `connection_established` is `true`, the Prometheus per-upstream
    /// stats for the peer `req_ctx.proxy.proxy_upstream_url` currently points at
    /// — before it's abandoned for a retry.
    ///
    /// Shared by all three retry-decision paths (5xx, connect-phase,
    /// proxy-phase timeout) so a peer that fails mid-retry-sequence is
    /// treated identically regardless of which failure mode caught it
    /// (#216 findings C/D — previously only the 5xx path fed *any* of
    /// this). No-ops when no upstream URL is currently tracked.
    ///
    /// `connection_established` distinguishes "a response was received (or
    /// the connection was at least established and a request sent)" from
    /// "the connection attempt itself failed" — `upstream_request_filter`
    /// only ever runs (incrementing `upstream_active_connections` and
    /// later needing `upstream_requests_total`/`upstream_latency_seconds`
    /// observed) once a connection actually succeeds. A connect-phase
    /// failure never reaches it, so there is nothing to reconcile there;
    /// decrementing the gauge anyway would introduce the opposite leak.
    ///
    /// `status` MUST be a real or synthetic failure status `>= 500` for a
    /// connect-phase/timeout caller (there is no real HTTP status for
    /// those) -- **never `0`**. `record_request_latency` treats any
    /// `status < 500` as a *success* for `consecutive_5xx` purposes
    /// (resets it to zero), so passing `0` would not just fail to feed
    /// outlier detection (the original findings C/D gap) but *actively
    /// mask* a hard-down peer by wiping out any `consecutive_5xx` count
    /// already accumulated from real 5xx responses (caught by Gitar
    /// reviewing PR #371).
    fn record_retry_failure_health(
        &self,
        req_ctx: &RequestCtx,
        config: &crate::config::schema::AppConfig,
        status: u16,
        connection_established: bool,
    ) {
        let Some(url) = req_ctx.proxy.proxy_upstream_url.as_deref() else {
            return;
        };
        let elapsed_us = req_ctx.start_time.elapsed().as_micros() as u64;
        crate::proxy::health::record_request_latency(
            &self.state.upstream_health,
            url,
            elapsed_us,
            status,
        );
        if let Some(od) = config
            .sites
            .get(req_ctx.site_idx)
            .and_then(|s| s.outlier_detection.as_ref())
        {
            crate::proxy::health::maybe_eject(&self.state.upstream_health, url, od);
        }
        if connection_established {
            self.state
                .metrics
                .upstream_active_connections
                .with_label_values(&[url])
                .dec();
            self.state
                .metrics
                .upstream_requests_total
                .with_label_values(&[url, &status.to_string()])
                .inc();
            if let Some(upstream_secs) = req_ctx
                .proxy
                .upstream_start
                .map(|t| t.elapsed().as_secs_f64())
            {
                self.state
                    .metrics
                    .upstream_latency_seconds
                    .with_label_values(&[url])
                    .observe(upstream_secs);
            }
        }
    }

    /// Record a failed upstream attempt immediately before triggering a Pingora
    /// retry.
    ///
    /// Updates all passive health state for the upstream that just returned
    /// `status` via [`record_retry_failure_health`](Self::record_retry_failure_health),
    /// then releases the connection slot (via
    /// [`release_conn_slot`]) so the next `upstream_peer()` call starts
    /// with no inherited slot.
    ///
    /// Without this, a successful retry on a different backend would silently
    /// absorb the failure without updating the health record of the backend that
    /// actually failed.
    pub(crate) fn record_failed_upstream_for_retry(
        &self,
        ctx: &mut Option<RequestCtx>,
        config: &crate::config::schema::AppConfig,
        status: u16,
    ) {
        let Some(req_ctx_mut) = ctx.as_mut() else {
            return;
        };
        if req_ctx_mut.proxy.proxy_upstream_url.is_none() {
            return;
        }
        // Record health/metrics for the failed peer BEFORE releasing its
        // slot -- record_retry_failure_health reads proxy_upstream_url,
        // which release_conn_slot below clears.
        self.record_retry_failure_health(req_ctx_mut, config, status, true);
        release_conn_slot(req_ctx_mut, &self.state.upstream_health);
        req_ctx_mut.proxy.upstream_start = None;
    }

    /// Check the retry budget and increment `retry_inflight` if a retry is allowed.
    ///
    /// Returns `true` when the retry may proceed, `false` when the budget is
    /// exhausted and the retry should be suppressed.
    ///
    /// Concurrent requests may race past this check, so the budget is a *soft*
    /// limit — occasional over-budget retries are acceptable.
    pub(super) fn retry_budget_allows(&self, retry: &mut RetryState) -> bool {
        if let Some(budget_pct) = retry.budget_percent {
            let inflight = self.state.inflight.load(Ordering::Relaxed).max(1) as f64;
            let current_retries = self.state.retry_inflight.load(Ordering::Relaxed) as f64;
            let limit = (inflight * budget_pct / 100.0).ceil() as usize;
            let current = current_retries as usize;
            if current >= limit {
                tracing::debug!(
                    budget_pct,
                    current_retries,
                    inflight,
                    "retry budget exhausted — suppressing retry"
                );
                return false;
            }
        }
        // Increment at most once per request, not once per retry decision
        // (#368) -- `is_retrying` already tracks "this request is currently
        // counted," so re-checking it here is what makes the increment
        // idempotent across a request's 2nd, 3rd, ... retry attempt.
        // `logging()` decrements exactly once per request when `is_retrying`
        // is set, so a request with `attempts: 3` that retries twice used to
        // net +1 permanently leaked per request (+1, +1, -1) -- `inflight`
        // (the budget's denominator) counts *requests*, so counting a
        // retrying request once is also the semantically correct reading,
        // not just a leak patch.
        if !retry.is_retrying {
            self.state.retry_inflight.fetch_add(1, Ordering::Relaxed);
            retry.is_retrying = true;
        }
        true
    }

    /// Evaluate whether a connect-phase error should trigger a retry.
    pub(super) fn try_retry_connect_error(
        &self,
        session: &Session,
        req_ctx: &mut RequestCtx,
        e: &mut Box<pingora_core::Error>,
        config: &crate::config::schema::AppConfig,
    ) {
        use pingora_core::ErrorType::*;
        let is_conn_err = matches!(
            e.etype(),
            ConnectRefused
                | ConnectNoRoute
                | ConnectError
                | ConnectProxyFailure
                | BindError
                | SocketError
        );
        let is_timeout = matches!(e.etype(), ConnectTimedout);
        let condition = if is_conn_err {
            "connection_error"
        } else {
            "timeout"
        };
        // Only retry safe/idempotent HTTP methods — RFC 7231 § 4.2.2.
        let method = session.req_header().method.as_str();
        let should_retry = {
            let Some(retry) = req_ctx.proxy.retry.as_mut() else {
                return;
            };
            is_safe_http_method(method)
                && ((is_conn_err && retry.has_condition("connection_error"))
                    || (is_timeout && retry.has_condition("timeout")))
                && self.retry_budget_allows(retry)
        };
        if should_retry {
            e.set_retry(true);
            self.state
                .metrics
                .retry_attempts_total
                .with_label_values(&["<connect>", condition])
                .inc();
            // #216 (findings C/D): connect-phase failures never fed passive
            // health / outlier detection before -- only the 5xx path did
            // (via record_failed_upstream_for_retry). A peer that's
            // hard-down (connection refused/timed out) on a retry-
            // configured route was never ejected by outlier detection as a
            // result. SYNTHETIC_RETRY_FAILURE_STATUS (503), not 0: a status
            // < 500 resets consecutive_5xx to zero in record_request_latency
            // -- passing 0 would actively mask a hard-down peer instead of
            // just failing to help (caught by Gitar reviewing PR #371).
            // connection_established=false: upstream_request_filter never
            // ran for a connect-phase failure, so there is no
            // active-connections gauge increment to reconcile.
            self.record_retry_failure_health(
                req_ctx,
                config,
                SYNTHETIC_RETRY_FAILURE_STATUS,
                false,
            );
            // Clear proxy_upstream_url/upstream_conn_slot immediately,
            // symmetric with try_retry_proxy_error's timeout branch: if
            // set_retry(true) doesn't actually result in another
            // upstream_peer() call, leaving the URL set would make
            // logging()'s terminal release_proxy_upstream record this same
            // connect failure's health a SECOND time (a spurious extra
            // consecutive_5xx increment / EWMA sample -- no gauge risk
            // here specifically, since connection_established=false never
            // touched it above). Found by security-engineer reviewing
            // PR #371's fix for the analogous timeout-branch gap.
            release_conn_slot(req_ctx, &self.state.upstream_health);
        }
    }

    /// Evaluate whether a proxy-phase error should trigger a retry.
    pub(super) fn try_retry_proxy_error(
        &self,
        session: &Session,
        req_ctx: &mut RequestCtx,
        e: &mut Box<pingora_core::Error>,
        config: &crate::config::schema::AppConfig,
    ) {
        use pingora_core::ErrorType::*;
        let is_timeout = matches!(e.etype(), ReadTimedout | WriteTimedout);
        let is_5xx_retry = matches!(e.etype(), Custom("5xx_retry"));
        let condition = if is_timeout { "timeout" } else { "5xx" };
        // Only retry safe/idempotent methods.
        let method = session.req_header().method.as_str();
        let should_retry = {
            let Some(retry) = req_ctx.proxy.retry.as_mut() else {
                return;
            };
            is_safe_http_method(method)
                && ((is_timeout && retry.has_condition("timeout"))
                    || (is_5xx_retry && retry.has_condition("5xx")))
                && self.retry_budget_allows(retry)
        };
        if should_retry {
            e.set_retry(true);
            let route = session.req_header().uri.path().to_owned();
            self.state
                .metrics
                .retry_attempts_total
                .with_label_values(&[route.as_str(), condition])
                .inc();
            // #216 (findings C/D): only the timeout branch needs new
            // health recording here -- a 5xx failure was already fully
            // recorded (health, gauge, upstream_requests_total/latency) by
            // record_failed_upstream_for_retry in response_phase.rs, which
            // runs BEFORE this Custom("5xx_retry") error is even
            // constructed; recording it again here would double-count.
            if is_timeout {
                // connection_established=true: a read/write timeout occurs
                // only after the connection succeeded and
                // upstream_request_filter already incremented the
                // active-connections gauge for this attempt -- unlike the
                // connect-phase case, that increment DOES need
                // reconciling here. SYNTHETIC_RETRY_FAILURE_STATUS (503),
                // not 0 -- see record_retry_failure_health's doc comment
                // (Gitar finding on PR #371).
                self.record_retry_failure_health(
                    req_ctx,
                    config,
                    SYNTHETIC_RETRY_FAILURE_STATUS,
                    true,
                );
                // Clear proxy_upstream_url/upstream_conn_slot immediately,
                // mirroring record_failed_upstream_for_retry's 5xx-path
                // ordering (record health first, then release), rather than
                // relying solely on upstream_peer's retry-restore to do it
                // on the NEXT attempt. Without this, if set_retry(true)
                // does not actually result in another upstream_peer() call
                // (e.g. Pingora declines to retry after all -- a truncated
                // retry buffer, or attempts genuinely exhausted right after
                // this decision), proxy_upstream_url stays pointing at this
                // failed attempt and logging()'s own unconditional
                // active-connections decrement (record_upstream_metrics)
                // would fire AGAIN for the same URL, driving the gauge
                // negative (Gitar finding on PR #371).
                release_conn_slot(req_ctx, &self.state.upstream_health);
            }
        }
    }
}

/// Body of [`pingora_proxy::ProxyHttp::fail_to_connect`].
pub(crate) fn fail_to_connect(
    proxy: &ConduitProxy,
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
    mut e: Box<pingora_core::Error>,
) -> Box<pingora_core::Error> {
    if let Some(req_ctx) = ctx.as_mut() {
        let has_attempts_left = req_ctx
            .proxy
            .retry
            .as_ref()
            .map(RetryState::has_attempts_left)
            .unwrap_or(false);
        if has_attempts_left {
            let config = proxy.state.config.load();
            proxy.try_retry_connect_error(session, req_ctx, &mut e, &config);
        }
    }
    e
}

/// Body of [`pingora_proxy::ProxyHttp::error_while_proxy`].
pub(crate) fn error_while_proxy(
    proxy: &ConduitProxy,
    peer: &HttpPeer,
    session: &mut Session,
    e: Box<pingora_core::Error>,
    ctx: &mut Option<RequestCtx>,
    client_reused: bool,
) -> Box<pingora_core::Error> {
    let mut e = e.more_context(format!("Peer: {peer}"));
    e.retry
        .decide_reuse(client_reused && !session.as_ref().retry_buffer_truncated());

    if let Some(req_ctx) = ctx.as_mut() {
        let has_attempts_left = req_ctx
            .proxy
            .retry
            .as_ref()
            .map(RetryState::has_attempts_left)
            .unwrap_or(false);
        if has_attempts_left {
            let config = proxy.state.config.load();
            proxy.try_retry_proxy_error(session, req_ctx, &mut e, &config);
        }
    }
    e
}

// ── request-side helpers ──────────────────────────────────────────────────────

/// Apply ±50 % jitter to a backoff duration.
///
/// Uses splitmix64 seeded from current nanoseconds — the same fast RNG used
/// elsewhere in the proxy.  Returns a value in `[ms/2, ms*3/2)`.
pub(crate) fn jitter_backoff_ms(ms: u64) -> u64 {
    if ms == 0 {
        return 0;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let mut x = seed
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(0x6c62272e07bb0142);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    // jitter ∈ [0, ms) → result ∈ [ms/2, ms/2 + ms) = [ms/2, 3ms/2)
    let jitter = x % ms;
    ms / 2 + jitter
}

/// Sleep for the configured backoff duration when this is a retry attempt (not the first try).
///
/// When `retry.backoff_jitter` is `true`, applies ±50 % randomness to spread
/// retries in time and avoid synchronized thundering herds.
pub(super) async fn apply_backoff(retry: &RetryState) {
    if retry.attempt > 0 {
        if let Some(ms) = retry.backoff_ms {
            let effective_ms = if retry.backoff_jitter {
                jitter_backoff_ms(ms)
            } else {
                ms
            };
            tokio::time::sleep(Duration::from_millis(effective_ms)).await;
        }
    }
}

/// Returns `true` for HTTP methods that are safe to retry.
///
/// RFC 7231 § 4.2.2 defines **idempotent** methods: a request is idempotent
/// when repeating it has the same effect as sending it once.  We only retry
/// these to prevent double-mutations (double charges, double emails, etc.).
///
/// `PUT` and `DELETE` are technically idempotent but are excluded here because
/// in practice applications often treat them as non-idempotent.  Operators who
/// want to retry them can configure `retry.conditions: ["connection_error"]`
/// without worrying — this function is the default safety gate.
///
/// Safe to retry: GET, HEAD, OPTIONS, TRACE.
pub(super) fn is_safe_http_method(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "TRACE"
    )
}

/// Release the `conn_count` slot this request currently holds (if any) and
/// clear both `proxy_upstream_url` and `upstream_conn_slot` (#216).
///
/// Idempotent: a no-op when `proxy_upstream_url` is already `None` (e.g.
/// already released by `record_failed_upstream_for_retry` on the 5xx retry
/// path). This is the only correct way to stop pointing at an upstream —
/// see the invariant documented on [`RequestCtx::upstream_conn_slot`].
pub(super) fn release_conn_slot(req_ctx: &mut RequestCtx, health: &UpstreamRegistry) {
    let Some(url) = req_ctx.proxy.proxy_upstream_url.take() else {
        return;
    };
    if std::mem::take(&mut req_ctx.proxy.upstream_conn_slot) {
        health.conn_dec(&url);
    }
}

/// Point this request at `url`, acquiring a real `conn_count` slot when
/// `tracked` is `true` (#216).
///
/// Debug-asserts that no slot is currently held — callers must
/// [`release_conn_slot`] first, never assign `proxy_upstream_url` /
/// `upstream_conn_slot` directly.
pub(super) fn acquire_conn_slot(
    req_ctx: &mut RequestCtx,
    health: &UpstreamRegistry,
    url: String,
    tracked: bool,
) {
    debug_assert!(
        req_ctx.proxy.proxy_upstream_url.is_none() && !req_ctx.proxy.upstream_conn_slot,
        "acquire_conn_slot called while a slot is already held for {:?} — call \
         release_conn_slot first",
        req_ctx.proxy.proxy_upstream_url
    );
    if tracked {
        health.conn_inc(&url);
    }
    req_ctx.proxy.proxy_upstream_url = Some(url);
    req_ctx.proxy.upstream_conn_slot = tracked;
}

/// Choose the URL for this attempt of a retry-configured request, and own
/// all `proxy_upstream_url`/`upstream_conn_slot` bookkeeping for it (#216
/// part 2 — real per-attempt capacity admission, building on part 1's leak
/// fix). Single source of truth for the retry index: replaces what used to
/// be two independently-recomputed copies of the same formula (one here,
/// one in `upstream_peer`'s old retry-restore block) that had to be kept in
/// lockstep by hand and could silently diverge.
///
/// **Attempt 0 (the request's first attempt) trusts routing's decision
/// verbatim** — `retry.urls[0]` is guaranteed equal to the peer
/// `pick_bounded`/`pick_with_retry` already chose (#367), and routing
/// already acquired whatever `conn_count` slot that decision implies
/// *before* `upstream_peer` was ever called. This function does not
/// re-probe capacity or touch slot bookkeeping for attempt 0: doing either
/// would silently override a full strategy-aware, capacity-aware,
/// ramp-aware decision with a much cruder "first admissible peer in a fixed
/// rotation" rule, and would double-acquire (or wrongly downgrade) a slot
/// routing already holds.
///
/// **Attempt 1+ (an actual retry) forward-probes** `retry.urls` starting at
/// this attempt's rotation index for a peer under
/// `retry.max_conns_per_upstream`, mirroring the `conduit-proxy-http`
/// crate's private `capacity::hash_pick_bounded`'s existing forward-probe
/// pattern rather than filtering the list — filtering would renumber every
/// subsequent attempt's rotation instead of skipping just the one saturated
/// peer. **Fails open** (falls back to the naive rotation URL) when every
/// peer is saturated, matching this codebase's established soft-cap
/// convention (`capacity.rs`'s module doc, `retry.budgetPercent`): capacity
/// having deteriorated mid-request should not turn into a hard failure on a
/// request that has already spent attempts. `retry.tracks_conn_slot`
/// mirrors `RouteResolution.upstream_conn_slot`'s own formula
/// (`is_least_conn || circuit_tracking`) computed at routing time.
pub(super) fn select_retry_target(req_ctx: &mut RequestCtx, health: &UpstreamRegistry) -> String {
    // Compute this attempt's target using only a borrow of req_ctx.proxy.retry,
    // ended before release_conn_slot/acquire_conn_slot need to borrow the
    // whole req_ctx.
    let (chosen, tracked) = {
        let retry = req_ctx
            .proxy
            .retry
            .as_mut()
            .expect("select_retry_target called only when req_ctx.proxy.retry.is_some()");
        let len = retry.urls.len();
        let base = retry.attempt % len;
        let is_first_attempt = retry.attempt == 0;
        retry.attempt += 1;

        if is_first_attempt {
            (retry.urls[0].clone(), None)
        } else {
            let chosen = match retry.max_conns_per_upstream {
                Some(max) => (0..len)
                    .map(|i| retry.urls[(base + i) % len].clone())
                    .find(|u| health.conn_load(u) < max as usize)
                    .unwrap_or_else(|| retry.urls[base].clone()),
                None => retry.urls[base].clone(),
            };
            (chosen, Some(retry.tracks_conn_slot))
        }
    };

    if let Some(tracked) = tracked {
        release_conn_slot(req_ctx, health);
        acquire_conn_slot(req_ctx, health, chosen.clone(), tracked);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::schema::AppConfig;
    use crate::proxy::ctx::{LocalHandler, ProxyReqState, UpstreamTarget};
    use crate::proxy::service::AppState;

    // `make_proxy`/`make_ctx` are duplicated across this file, `handlers.rs`,
    // `peer.rs`, and `transform.rs`'s test modules -- each needs one, and
    // none of these sibling modules depend on each other. `peer.rs` holds
    // the "canonical" original location for `make_ctx`.
    fn make_proxy() -> ConduitProxy {
        let config = crate::config::schema::AppConfig::default();
        let state = AppState::new(config, std::path::PathBuf::from("."), None);
        ConduitProxy {
            state: std::sync::Arc::new(state),
        }
    }

    fn make_ctx(upstream: UpstreamTarget) -> RequestCtx {
        RequestCtx::new(0, upstream, ProxyReqState::default(), None)
    }

    // ── jitter_backoff_ms ─────────────────────────────────────────────────────

    #[test]
    fn jitter_result_is_within_50_percent_range() {
        // Run 100 times to reduce flakiness from the time-seeded RNG.
        for _ in 0..100 {
            let ms = 200u64;
            let result = jitter_backoff_ms(ms);
            assert!(
                result >= ms / 2 && result < ms + ms / 2,
                "jitter result {result} must be in [{}, {})",
                ms / 2,
                ms + ms / 2
            );
        }
    }

    #[test]
    fn jitter_zero_ms_returns_zero() {
        assert_eq!(jitter_backoff_ms(0), 0);
    }

    #[test]
    fn jitter_one_ms_returns_zero_or_one() {
        let result = jitter_backoff_ms(1);
        // ms=1 → ms/2 = 0, jitter ∈ [0, 1) → result ∈ {0}
        assert!(result < 2, "result {result} out of range for ms=1");
    }

    // ── is_safe_http_method ───────────────────────────────────────────────────

    #[test]
    fn safe_methods_are_safe() {
        assert!(is_safe_http_method("GET"));
        assert!(is_safe_http_method("HEAD"));
        assert!(is_safe_http_method("OPTIONS"));
        assert!(is_safe_http_method("TRACE"));
    }

    #[test]
    fn unsafe_methods_are_not_safe() {
        assert!(!is_safe_http_method("POST"));
        assert!(!is_safe_http_method("PUT"));
        assert!(!is_safe_http_method("DELETE"));
        assert!(!is_safe_http_method("PATCH"));
    }

    #[test]
    fn safe_method_case_insensitive() {
        assert!(is_safe_http_method("get"));
        assert!(is_safe_http_method("Get"));
    }

    #[test]
    fn retry_budget_allows_without_budget_config() {
        let proxy = make_proxy();
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None, // no budget limit
            is_retrying: false,
            max_conns_per_upstream: None,
            tracks_conn_slot: false,
        };
        // No budget configured → always allows retry.
        assert!(proxy.retry_budget_allows(&mut retry));
        assert!(retry.is_retrying);
    }

    #[test]
    fn retry_budget_allows_within_budget() {
        let proxy = make_proxy();
        proxy.state.inflight.store(10, Ordering::Relaxed); // 10 inflight
        proxy.state.retry_inflight.store(0, Ordering::Relaxed); // 0 retries
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: Some(50.0), // 50% budget → up to 5 retries
            is_retrying: false,
            max_conns_per_upstream: None,
            tracks_conn_slot: false,
        };
        assert!(
            proxy.retry_budget_allows(&mut retry),
            "within budget must allow"
        );
        assert!(retry.is_retrying);
    }

    #[test]
    fn retry_budget_denies_when_exhausted() {
        let proxy = make_proxy();
        proxy.state.inflight.store(10, Ordering::Relaxed); // 10 inflight
        proxy.state.retry_inflight.store(10, Ordering::Relaxed); // already 10 retries = 100% of budget
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: Some(50.0), // 50% → max 5, current=10 → denied
            is_retrying: false,
            max_conns_per_upstream: None,
            tracks_conn_slot: false,
        };
        assert!(
            !proxy.retry_budget_allows(&mut retry),
            "exhausted budget must deny"
        );
        assert!(!retry.is_retrying);
    }

    /// Regression test for #368: calling `retry_budget_allows` twice on the
    /// SAME `RetryState` (as happens for a request that retries more than
    /// once, e.g. `attempts: 3` with both retries taken) must only increment
    /// `retry_inflight` once, not once per call. Reverting the `is_retrying`
    /// guard (unconditional `fetch_add`) would make `retry_inflight` end up
    /// at 2 here instead of 1 -- a leak that compounds across every
    /// multi-attempt retry sequence for the life of the process and can
    /// eventually make `retry.budgetPercent` silently suppress all retries
    /// sitewide.
    #[test]
    fn retry_budget_allows_increments_inflight_once_per_request_not_per_attempt() {
        let proxy = make_proxy();
        proxy.state.inflight.store(10, Ordering::Relaxed);
        proxy.state.retry_inflight.store(0, Ordering::Relaxed);
        let mut retry = RetryState {
            urls: vec!["http://a:4000".to_owned()],
            attempt: 0,
            max_attempts: 3,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: Some(50.0),
            is_retrying: false,
            max_conns_per_upstream: None,
            tracks_conn_slot: false,
        };
        // First retry decision for this request.
        assert!(proxy.retry_budget_allows(&mut retry));
        assert_eq!(proxy.state.retry_inflight.load(Ordering::Relaxed), 1);
        // Second retry decision for the SAME request (e.g. its 2nd retry
        // attempt) -- must NOT increment again.
        assert!(proxy.retry_budget_allows(&mut retry));
        assert_eq!(
            proxy.state.retry_inflight.load(Ordering::Relaxed),
            1,
            "retry_inflight must not increment again for a request already counted as retrying"
        );
    }

    // ── select_retry_target (#216 part 2) ─────────────────────────────────────

    fn make_retry_ctx(
        urls: &[&str],
        attempt: usize,
        max_conns_per_upstream: Option<u64>,
        tracks_conn_slot: bool,
    ) -> RequestCtx {
        let mut ctx = make_ctx(UpstreamTarget::Proxy {
            addr: "unused:0".to_owned(),
            tls: false,
            sni: String::new(),
            strip_prefix: None,
            rewrite: None,
            mirror_url: None,
            upstream_tls: None,
        });
        ctx.proxy.retry = Some(RetryState {
            urls: urls.iter().map(|u| u.to_string()).collect(),
            attempt,
            max_attempts: 5,
            conditions: vec!["5xx".to_owned()],
            backoff_ms: None,
            backoff_jitter: false,
            budget_percent: None,
            is_retrying: false,
            max_conns_per_upstream,
            tracks_conn_slot,
        });
        ctx
    }

    /// Attempt 0 must trust routing's decision verbatim: return
    /// `retry.urls[0]` without touching `proxy_upstream_url`/
    /// `upstream_conn_slot` at all, even when a slot is already held (as
    /// routing would have already set up before `upstream_peer` runs).
    /// Re-probing or re-acquiring here would silently override a full
    /// strategy-aware pick_bounded/pick_with_retry decision.
    #[test]
    fn select_retry_target_attempt_zero_trusts_routing_without_touching_slot() {
        let reg = UpstreamRegistry::new();
        // Deliberately set retry.tracks_conn_slot to `true` while routing's
        // OWN decision (upstream_conn_slot below) was `false` -- an
        // attribution-only pick (e.g. no cap configured and not
        // least-conn). If attempt 0 incorrectly ran release_conn_slot/
        // acquire_conn_slot (using retry.tracks_conn_slot), it would flip
        // upstream_conn_slot to `true` and increment conn_load -- an
        // observable difference from "untouched" that a same-peer,
        // same-tracked-value scenario could never catch.
        let mut ctx = make_retry_ctx(&["http://a:80", "http://b:80"], 0, Some(1), true);
        // Simulate what routing already set up before upstream_peer ran.
        ctx.proxy.proxy_upstream_url = Some("http://a:80".to_owned());
        ctx.proxy.upstream_conn_slot = false;

        let chosen = select_retry_target(&mut ctx, &reg);

        assert_eq!(chosen, "http://a:80");
        assert_eq!(ctx.proxy.retry.as_ref().unwrap().attempt, 1);
        assert_eq!(
            ctx.proxy.proxy_upstream_url.as_deref(),
            Some("http://a:80"),
            "attempt 0 must not touch proxy_upstream_url"
        );
        assert!(
            !ctx.proxy.upstream_conn_slot,
            "attempt 0 must not touch upstream_conn_slot -- must stay exactly as routing set it, \
             even though retry.tracks_conn_slot is true"
        );
        assert_eq!(
            reg.conn_load("http://a:80"),
            0,
            "attempt 0 must not acquire a slot nothing asked it to"
        );
    }

    /// The actual point of #216 part 2: a retry attempt must forward-probe
    /// past a saturated peer instead of naively rotating into it. Peer at
    /// `base` is at its cap; the next peer in rotation is under capacity —
    /// the retry must land on the SECOND peer, not the first.
    #[test]
    fn select_retry_target_retry_forward_probes_past_saturated_peer() {
        let reg = UpstreamRegistry::new();
        reg.conn_inc("http://a:80"); // a is now at the cap of 1
                                     // attempt=2, len=2 -> base = 2 % 2 = 0, i.e. the NAIVE (non-probing)
                                     // target would be urls[0] = "http://a:80", the saturated one --
                                     // this is the case that actually exercises the forward-probe
                                     // skipping past it to urls[1].
        let mut ctx = make_retry_ctx(&["http://a:80", "http://b:80"], 2, Some(1), true);

        let chosen = select_retry_target(&mut ctx, &reg);

        assert_eq!(
            chosen, "http://b:80",
            "must forward-probe past the saturated peer to the next admissible one"
        );
        assert_eq!(ctx.proxy.retry.as_ref().unwrap().attempt, 3);
        assert_eq!(
            reg.conn_load("http://b:80"),
            1,
            "the new peer's slot must be acquired"
        );
    }

    /// Fail-open guarantee: when every peer in the rotation is saturated,
    /// `select_retry_target` must still return a URL (the naive rotation
    /// target), never panic or loop forever. Matches this codebase's
    /// established soft-cap convention (capacity.rs's module doc,
    /// retry.budgetPercent) -- capacity deteriorating mid-request must not
    /// turn into a hard failure on a request that already spent attempts.
    #[test]
    fn select_retry_target_fails_open_when_every_peer_is_saturated() {
        let reg = UpstreamRegistry::new();
        reg.conn_inc("http://a:80");
        reg.conn_inc("http://b:80");
        let mut ctx = make_retry_ctx(&["http://a:80", "http://b:80"], 1, Some(1), true);

        let chosen = select_retry_target(&mut ctx, &reg);

        // base = attempt % len = 1 % 2 = 1 -> naive fallback is urls[1].
        assert_eq!(chosen, "http://b:80");
        assert_eq!(ctx.proxy.retry.as_ref().unwrap().attempt, 2);
    }

    /// `max_conns_per_upstream: None` (no cap configured) must skip the
    /// forward-probe entirely and use the naive rotation index -- matching
    /// pre-#216-part-2 behavior exactly when there's nothing to admit
    /// against.
    #[test]
    fn select_retry_target_no_cap_configured_uses_naive_rotation() {
        let reg = UpstreamRegistry::new();
        // Even though b is "saturated" by some unrelated bookkeeping, with
        // no cap configured there is nothing to forward-probe against --
        // base = attempt(1) % len(2) = 1, so the naive target is urls[1].
        reg.conn_inc("http://b:80");
        reg.conn_inc("http://b:80");
        reg.conn_inc("http://b:80");
        let mut ctx = make_retry_ctx(&["http://a:80", "http://b:80"], 1, None, true);

        let chosen = select_retry_target(&mut ctx, &reg);

        assert_eq!(
            chosen, "http://b:80",
            "no cap -> naive rotation, no probing"
        );
        assert_eq!(ctx.proxy.retry.as_ref().unwrap().attempt, 2);
    }

    /// `tracks_conn_slot: false` must acquire the new URL without
    /// incrementing conn_count -- matching part 1's undercount-preserving
    /// behavior for routes that don't track retries (e.g. no cap
    /// configured and not least-conn).
    #[test]
    fn select_retry_target_untracked_acquires_without_incrementing() {
        let reg = UpstreamRegistry::new();
        let mut ctx = make_retry_ctx(&["http://a:80", "http://b:80"], 1, None, false);

        let chosen = select_retry_target(&mut ctx, &reg);

        assert_eq!(chosen, "http://b:80");
        assert_eq!(reg.conn_load("http://b:80"), 0);
        assert!(!ctx.proxy.upstream_conn_slot);
    }

    /// A retry attempt must release the PREVIOUS attempt's slot before
    /// acquiring the new one -- the actual leak-closing behavior from part
    /// 1, still correct after part 2's forward-probing was layered on top.
    /// base = attempt(1) % len(2) = 1, so the probe starts at (and, being
    /// under the cap of 5, immediately accepts) `urls[1]` = "http://b:80" --
    /// a DIFFERENT peer than the one the previous attempt held a slot on.
    #[test]
    fn select_retry_target_releases_previous_slot_before_acquiring_new_one() {
        let reg = UpstreamRegistry::new();
        reg.conn_inc("http://a:80"); // the previous attempt's slot
        let mut ctx = make_retry_ctx(&["http://a:80", "http://b:80"], 1, Some(5), true);
        ctx.proxy.proxy_upstream_url = Some("http://a:80".to_owned());
        ctx.proxy.upstream_conn_slot = true;

        let chosen = select_retry_target(&mut ctx, &reg);

        assert_eq!(chosen, "http://b:80");
        assert_eq!(
            reg.conn_load("http://a:80"),
            0,
            "the previous attempt's slot on a DIFFERENT peer must be released"
        );
        assert_eq!(reg.conn_load("http://b:80"), 1);
        assert_eq!(ctx.proxy.proxy_upstream_url.as_deref(), Some("http://b:80"));
    }

    // ── record_failed_upstream_for_retry ──────────────────────────────────────

    /// ctx = None → function must return without panicking.
    #[test]
    fn record_failed_upstream_ctx_none_is_noop() {
        let proxy = make_proxy();
        let mut ctx: Option<RequestCtx> = None;
        let config = AppConfig::default();
        // Must not panic.
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 500);
    }

    /// ctx is Some but proxy_upstream_url is None → function returns early.
    #[test]
    fn record_failed_upstream_url_none_returns_early() {
        let proxy = make_proxy();
        let inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        // proxy_upstream_url defaults to None — no URL to record.
        assert!(inner.proxy.proxy_upstream_url.is_none());
        let mut ctx = Some(inner);
        let config = AppConfig::default();
        // Must not panic on the early-return path.
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 503);
    }

    /// Happy path: URL is set → it is taken (cleared) and upstream_start is
    /// reset to None.
    #[test]
    fn record_failed_upstream_records_attempt_and_clears_url() {
        let proxy = make_proxy();
        let mut inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        inner.proxy.proxy_upstream_url = Some("http://backend:4000".to_owned());
        inner.proxy.upstream_start = None;
        let mut ctx = Some(inner);
        let config = AppConfig::default();
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 502);
        let req = ctx.as_ref().unwrap();
        // URL must have been taken (cleared).
        assert!(req.proxy.proxy_upstream_url.is_none());
        // upstream_start should remain None (wasn't set).
        assert!(req.proxy.upstream_start.is_none());
    }

    /// When upstream_start is set, the latency histogram observe branch runs.
    #[test]
    fn record_failed_upstream_observes_latency_when_start_is_set() {
        let proxy = make_proxy();
        let mut inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        inner.proxy.proxy_upstream_url = Some("http://backend:4001".to_owned());
        inner.proxy.upstream_start = Some(std::time::Instant::now());
        let mut ctx = Some(inner);
        let config = AppConfig::default();
        // Must not panic even when upstream_start is Some.
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 500);
        let req = ctx.as_ref().unwrap();
        // upstream_start reset to None after recording.
        assert!(req.proxy.upstream_start.is_none());
    }

    /// When the site config has outlier_detection set, maybe_eject() is called.
    #[test]
    fn record_failed_upstream_triggers_outlier_detection_when_configured() {
        let proxy = make_proxy();
        let mut inner = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        inner.proxy.proxy_upstream_url = Some("http://backend:4002".to_owned());
        inner.site_idx = 0;
        let mut ctx = Some(inner);

        // Build an AppConfig with outlier detection enabled on site 0.
        let mut config = AppConfig::default();
        let site = crate::config::schema::SiteConfig {
            outlier_detection: Some(crate::config::schema::OutlierDetectionConfig {
                consecutive_5xx: Some(1),
                base_ejection_time_secs: Some(5),
                max_ejection_time_secs: Some(30),
                max_ejection_percent: Some(50),
            }),
            ..Default::default()
        };
        config.sites = vec![site];

        // Must not panic — exercises the maybe_eject() call inside the if-let branch.
        proxy.record_failed_upstream_for_retry(&mut ctx, &config, 503);
    }

    // ── release_conn_slot / acquire_conn_slot (#216) ──────────────────────────

    #[test]
    fn release_conn_slot_noop_when_no_url_held() {
        let reg = UpstreamRegistry::new();
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        assert!(ctx.proxy.proxy_upstream_url.is_none());
        release_conn_slot(&mut ctx, &reg); // must not panic
        assert!(ctx.proxy.proxy_upstream_url.is_none());
        assert!(!ctx.proxy.upstream_conn_slot);
    }

    #[test]
    fn release_conn_slot_decrements_when_tracked() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        reg.conn_inc(url);
        assert_eq!(reg.conn_load(url), 1);

        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        ctx.proxy.upstream_conn_slot = true;

        release_conn_slot(&mut ctx, &reg);

        assert_eq!(
            reg.conn_load(url),
            0,
            "release must decrement a tracked slot"
        );
        assert!(ctx.proxy.proxy_upstream_url.is_none());
        assert!(!ctx.proxy.upstream_conn_slot);
    }

    #[test]
    fn release_conn_slot_does_not_decrement_when_untracked() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        // No conn_inc — attribution-only, matching the "passive-health-only"
        // shape documented on RequestCtx::upstream_conn_slot.
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        ctx.proxy.upstream_conn_slot = false;

        release_conn_slot(&mut ctx, &reg);

        assert_eq!(reg.conn_load(url), 0);
        assert!(ctx.proxy.proxy_upstream_url.is_none());
    }

    #[test]
    fn release_conn_slot_is_idempotent() {
        let reg = UpstreamRegistry::new();
        let url = "http://u:4000";
        reg.conn_inc(url);

        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        ctx.proxy.upstream_conn_slot = true;

        release_conn_slot(&mut ctx, &reg);
        assert_eq!(reg.conn_load(url), 0);
        // Second call on the same (now-cleared) ctx must be a no-op, not a
        // second (incorrect, underflowing) decrement.
        release_conn_slot(&mut ctx, &reg);
        assert_eq!(reg.conn_load(url), 0);
    }

    #[test]
    fn acquire_conn_slot_increments_when_tracked() {
        let reg = UpstreamRegistry::new();
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));

        acquire_conn_slot(&mut ctx, &reg, "http://u:4000".to_owned(), true);

        assert_eq!(reg.conn_load("http://u:4000"), 1);
        assert_eq!(
            ctx.proxy.proxy_upstream_url.as_deref(),
            Some("http://u:4000")
        );
        assert!(ctx.proxy.upstream_conn_slot);
    }

    #[test]
    fn acquire_conn_slot_does_not_increment_when_untracked() {
        let reg = UpstreamRegistry::new();
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));

        acquire_conn_slot(&mut ctx, &reg, "http://u:4000".to_owned(), false);

        assert_eq!(reg.conn_load("http://u:4000"), 0);
        assert_eq!(
            ctx.proxy.proxy_upstream_url.as_deref(),
            Some("http://u:4000")
        );
        assert!(!ctx.proxy.upstream_conn_slot);
    }

    /// Regression test for #216: release-then-acquire-a-different-URL — the
    /// EXACT sequence `upstream_peer`'s retry-restore block now performs on
    /// every retry attempt — must leave the FIRST url's slot at 0 and the
    /// SECOND at 1. Before this fix, a connect-phase or proxy-phase-timeout
    /// retry failure never released the first URL's slot at all (only the
    /// 5xx path did, via `record_failed_upstream_for_retry`), leaking it
    /// permanently: `conn_count` would rise monotonically until the
    /// affected upstream was permanently excluded by `Capacity::evaluate`.
    #[test]
    fn release_then_acquire_different_url_transfers_the_slot_cleanly() {
        let reg = UpstreamRegistry::new();
        let url1 = "http://u1:4000";
        let url2 = "http://u2:4000";
        reg.conn_inc(url1); // simulates attempt 1's routing-time conn_inc

        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url1.to_owned());
        ctx.proxy.upstream_conn_slot = true;

        // Exactly what upstream_peer's retry-restore block does on the next
        // attempt after ANY failure mode (connect-phase, proxy-phase
        // timeout, or 5xx).
        release_conn_slot(&mut ctx, &reg);
        acquire_conn_slot(&mut ctx, &reg, url2.to_owned(), false);

        assert_eq!(
            reg.conn_load(url1),
            0,
            "the OLD url's slot must be released, not leaked"
        );
        assert_eq!(reg.conn_load(url2), 0, "tracked=false acquires no new slot");
        assert_eq!(ctx.proxy.proxy_upstream_url.as_deref(), Some(url2));
        assert!(!ctx.proxy.upstream_conn_slot);
    }

    #[test]
    #[should_panic(expected = "acquire_conn_slot called while a slot is already held")]
    fn acquire_conn_slot_panics_in_debug_if_slot_already_held() {
        let reg = UpstreamRegistry::new();
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some("http://u1:4000".to_owned());
        ctx.proxy.upstream_conn_slot = true;
        // Missing release_conn_slot() call before this -- must trip the
        // debug_assert! guarding the invariant documented on
        // RequestCtx::upstream_conn_slot.
        acquire_conn_slot(&mut ctx, &reg, "http://u2:4000".to_owned(), true);
    }

    // ── record_retry_failure_health (#216 findings C/D) ───────────────────────

    #[test]
    fn record_retry_failure_health_noop_when_no_url_tracked() {
        let proxy = make_proxy();
        let ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        assert!(ctx.proxy.proxy_upstream_url.is_none());
        let config = AppConfig::default();
        // Must not panic on the early-return path.
        proxy.record_retry_failure_health(&ctx, &config, 0, false);
    }

    /// connection_established=false (connect-phase failure) must NOT touch
    /// the Prometheus active-connections gauge — upstream_request_filter
    /// never ran for a connect that never succeeded, so there is nothing to
    /// reconcile. Decrementing anyway would introduce the OPPOSITE leak.
    #[test]
    fn record_retry_failure_health_skips_gauge_when_connection_not_established() {
        let proxy = make_proxy();
        let url = "http://u:4000";
        // Simulate the gauge already at its natural starting point for a
        // connect-phase failure: never incremented for this attempt.
        let before = proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .get();

        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        let config = AppConfig::default();

        proxy.record_retry_failure_health(&ctx, &config, 0, false);

        let after = proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .get();
        assert_eq!(
            before, after,
            "connection_established=false must not touch the gauge"
        );
    }

    /// connection_established=true (proxy-phase timeout, or the 5xx path via
    /// `record_failed_upstream_for_retry`) MUST decrement the gauge —
    /// `upstream_request_filter` incremented it when the connection
    /// succeeded, so it needs reconciling here before the retry.
    #[test]
    fn record_retry_failure_health_decrements_gauge_when_connection_established() {
        let proxy = make_proxy();
        let url = "http://u2:4000";
        // Simulate upstream_request_filter's earlier increment for this attempt.
        proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .inc();
        let before = proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .get();

        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        let config = AppConfig::default();

        proxy.record_retry_failure_health(&ctx, &config, 0, true);

        let after = proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .get();
        assert_eq!(
            after,
            before - 1.0,
            "connection_established=true must decrement the gauge exactly once"
        );
    }

    /// Regression test for the Gitar finding on PR #371: a connect-phase/
    /// timeout retry failure recorded with `status = 0` would RESET
    /// `consecutive_5xx` to zero (since `record_request_latency` treats
    /// anything `< 500` as a success) instead of contributing to outlier
    /// detection -- actively masking a hard-down peer that had already
    /// accumulated real 5xx failures, the opposite of findings C/D's
    /// stated goal. `SYNTHETIC_RETRY_FAILURE_STATUS` (503) must increment
    /// it instead.
    #[test]
    fn record_retry_failure_health_with_synthetic_failure_status_increments_consecutive_5xx() {
        let proxy = make_proxy();
        let url = "http://u3:4000";
        // Simulate 2 prior real 5xx responses already accumulated.
        crate::proxy::health::record_request_latency(&proxy.state.upstream_health, url, 1_000, 500);
        crate::proxy::health::record_request_latency(&proxy.state.upstream_health, url, 1_000, 502);
        assert_eq!(
            proxy
                .state
                .upstream_health
                .statuses
                .get(url)
                .unwrap()
                .consecutive_5xx,
            2
        );

        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        let config = AppConfig::default();

        proxy.record_retry_failure_health(&ctx, &config, SYNTHETIC_RETRY_FAILURE_STATUS, false);

        assert_eq!(
            proxy
                .state
                .upstream_health
                .statuses
                .get(url)
                .unwrap()
                .consecutive_5xx,
            3,
            "a connect/timeout retry failure must CONTINUE the consecutive_5xx count, \
             not reset it -- status=0 would incorrectly reset to 0 here"
        );
    }

    /// Regression test for the second Gitar finding on PR #371: the
    /// proxy-phase-timeout branch of `try_retry_proxy_error` must clear
    /// `proxy_upstream_url`/`upstream_conn_slot` (via `release_conn_slot`)
    /// immediately after recording health -- not rely solely on
    /// `upstream_peer`'s retry-restore to do it on a NEXT attempt that
    /// might never happen (e.g. `set_retry(true)` doesn't actually result
    /// in Pingora re-invoking `upstream_peer`, such as a truncated retry
    /// buffer). Without the immediate release, `logging()`'s own
    /// unconditional active-connections decrement would fire a SECOND time
    /// for the same URL, driving the gauge negative.
    #[test]
    fn record_retry_failure_health_then_release_leaves_no_url_to_double_decrement() {
        let proxy = make_proxy();
        let url = "http://u4:4000";
        proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .inc();

        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        ctx.proxy.upstream_conn_slot = true;
        proxy.state.upstream_health.conn_inc(url);
        let config = AppConfig::default();

        // Exactly the sequence try_retry_proxy_error's timeout branch now
        // performs: record health first (needs proxy_upstream_url still
        // set), then release.
        proxy.record_retry_failure_health(&ctx, &config, SYNTHETIC_RETRY_FAILURE_STATUS, true);
        release_conn_slot(&mut ctx, &proxy.state.upstream_health);

        assert!(
            ctx.proxy.proxy_upstream_url.is_none(),
            "proxy_upstream_url must be cleared immediately, not left for a retry that may never happen"
        );
        assert!(!ctx.proxy.upstream_conn_slot);
        assert_eq!(
            proxy.state.upstream_health.conn_load(url),
            0,
            "the conn_count slot must also be released"
        );
        let gauge_after = proxy
            .state
            .metrics
            .upstream_active_connections
            .with_label_values(&[url])
            .get();
        assert_eq!(
            gauge_after, 0.0,
            "gauge must be decremented exactly once (by record_retry_failure_health)"
        );
    }

    /// Regression test for the symmetric gap `security-engineer` found while
    /// re-reviewing PR #371's timeout-branch fix: `try_retry_connect_error`
    /// must ALSO clear `proxy_upstream_url`/release the conn_count slot
    /// immediately after recording health, not leave it for a retry that
    /// might never happen. Unlike the proxy-timeout case there is no
    /// Prometheus gauge to double-decrement (connection_established=false
    /// never touches it), but leaving the URL set would still make
    /// `logging()`'s terminal path record this same connect failure's
    /// health a second time (a spurious extra `consecutive_5xx` increment /
    /// EWMA sample) if no further retry attempt actually happens.
    #[test]
    fn connect_phase_record_then_release_leaves_no_url_for_duplicate_health_recording() {
        let proxy = make_proxy();
        let url = "http://u5:4000";
        let mut ctx = make_ctx(UpstreamTarget::Local(LocalHandler::Health));
        ctx.proxy.proxy_upstream_url = Some(url.to_owned());
        ctx.proxy.upstream_conn_slot = true;
        proxy.state.upstream_health.conn_inc(url);
        let config = AppConfig::default();

        // Exactly the sequence try_retry_connect_error now performs:
        // record health (connection_established=false, no gauge touched),
        // then release.
        proxy.record_retry_failure_health(&ctx, &config, SYNTHETIC_RETRY_FAILURE_STATUS, false);
        release_conn_slot(&mut ctx, &proxy.state.upstream_health);

        assert!(
            ctx.proxy.proxy_upstream_url.is_none(),
            "proxy_upstream_url must be cleared immediately, not left for a retry that may never happen"
        );
        assert!(!ctx.proxy.upstream_conn_slot);
        assert_eq!(
            proxy.state.upstream_health.conn_load(url),
            0,
            "the conn_count slot must also be released"
        );
        assert_eq!(
            proxy
                .state
                .upstream_health
                .statuses
                .get(url)
                .unwrap()
                .consecutive_5xx,
            1,
            "health must have been recorded exactly once"
        );
    }
}
