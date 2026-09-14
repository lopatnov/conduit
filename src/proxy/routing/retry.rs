//! Retry-state construction (issue #143, PR A2 of a 3-PR plan) — moved
//! verbatim out of `router.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;

use crate::config::schema::RetryConfig;
use crate::proxy::routing::state::RetryState;

/// Pick a starting URL and build retry state, rotating the URL list so that
/// `upstream_peer()` can walk it on each attempt.
///
/// `max_conns_per_upstream` is threaded through into `RetryState` so
/// per-attempt capacity admission (#216 part 2) evaluates every attempt of
/// one request against the same config snapshot that produced `urls`,
/// rather than re-reading config from inside `upstream_peer`. This branch
/// always bypasses `pick_bounded`/strategy dispatch entirely (see this
/// function's caller), so `is_least_conn` is never true here -- retries on
/// this path track a `conn_count` slot per attempt exactly when a cap is
/// configured, mirroring `circuit_tracking`'s own condition for the first
/// attempt a few lines below this function's call site.
pub(crate) fn pick_with_retry(
    urls: &[String],
    route_key: &str,
    counters: &DashMap<String, AtomicUsize>,
    retry: &RetryConfig,
    max_conns_per_upstream: Option<u64>,
) -> Option<(String, RetryState)> {
    let start_idx = if urls.len() > 1 {
        let entry = counters
            .entry(route_key.to_owned())
            .or_insert_with(|| AtomicUsize::new(0));
        entry.fetch_add(1, Ordering::Relaxed) % urls.len()
    } else {
        0
    };
    let first = urls.get(start_idx)?.clone();
    let state = retry_state_for(
        urls,
        &first,
        retry,
        max_conns_per_upstream,
        max_conns_per_upstream.is_some(),
    );
    Some((first, state))
}

/// Build [`RetryState`] with `candidates` rotated so `chosen_url` is
/// attempt 0.
///
/// `upstream_peer`'s `select_retry_target` trusts `urls[0]` as "the peer
/// routing already chose and already acquired a slot for" and skips capacity
/// re-probing for it (#216 part 2) — so this invariant is load-bearing, not
/// cosmetic. `chosen_url` can legitimately be absent from `candidates` (a
/// pin honored while the ramp filtered that peer out of the candidate list,
/// say); prepending keeps the invariant either way.
pub(crate) fn retry_state_for(
    candidates: &[String],
    chosen_url: &str,
    retry: &RetryConfig,
    max_conns_per_upstream: Option<u64>,
    tracks_conn_slot: bool,
) -> RetryState {
    let mut urls: Vec<String> = candidates.to_vec();
    match urls.iter().position(|u| u == chosen_url) {
        Some(pos) => urls.rotate_left(pos),
        None => urls.insert(0, chosen_url.to_owned()),
    }
    RetryState {
        urls,
        attempt: 0,
        max_attempts: retry.attempts as usize,
        conditions: retry.conditions.clone(),
        backoff_ms: retry.backoff_ms,
        backoff_jitter: retry.backoff_jitter.unwrap_or(false),
        budget_percent: retry.budget_percent,
        is_retrying: false,
        max_conns_per_upstream,
        tracks_conn_slot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_single_url_builds_state() {
        let urls = vec!["http://a:4000".to_string()];
        let counters = DashMap::new();
        let retry = RetryConfig {
            attempts: 3,
            conditions: vec!["5xx".to_string()],
            backoff_ms: None,
            budget_percent: None,
            backoff_jitter: None,
        };
        let (url, state) = pick_with_retry(&urls, "r", &counters, &retry, None).unwrap();
        assert_eq!(url, "http://a:4000");
        assert_eq!(state.max_attempts, 3);
        assert!(state.has_condition("5xx"));
        assert!(!state.has_condition("connection_error"));
    }

    #[test]
    fn retry_multiple_urls_rotates() {
        let urls = vec!["http://a:4000".to_string(), "http://b:4000".to_string()];
        let counters = DashMap::new();
        let retry = RetryConfig {
            attempts: 2,
            conditions: vec!["connection_error".to_string()],
            backoff_ms: Some(50),
            budget_percent: None,
            backoff_jitter: None,
        };
        let (url, state) = pick_with_retry(&urls, "r", &counters, &retry, None).unwrap();
        assert!(urls.contains(&url));
        assert_eq!(state.urls.len(), 2);
        assert_eq!(state.backoff_ms, Some(50));
    }

    #[test]
    fn retry_empty_urls_returns_none() {
        let counters = DashMap::new();
        let retry = RetryConfig {
            attempts: 3,
            conditions: vec![],
            backoff_ms: None,
            budget_percent: None,
            backoff_jitter: None,
        };
        assert!(pick_with_retry(&[], "r", &counters, &retry, None).is_none());
    }
}
