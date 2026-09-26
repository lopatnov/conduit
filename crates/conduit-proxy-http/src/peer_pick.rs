//! Candidate-pool building and peer selection for the `proxy` map path
//! (issue #143) — split out of `crate::resolve`'s orchestrator into their
//! own file in PR A2 (issue #419) to keep `resolve.rs` under the
//! 400-production-line soft limit (conceptually still part of that
//! function's phase split, just physically separated), then moved into this
//! crate in PR B (issue #143 itself).

use crate::capacity;
use crate::options::{ProxyCtx, RouteOptions};
use crate::outcome::ProxyResolution;
use crate::retry::retry_state_for;
use crate::slow_start::Ramp;
use crate::state::{ProxyReqState, RetryState};
use crate::sticky::{self, Sticky};

/// Health-filtered, capacity-checked candidate pool for one routing decision.
pub(crate) struct CandidatePool {
    pub(crate) healthy_urls: Vec<String>,
    /// `true` when [`Self::healthy_urls`] is the fail-open passthrough
    /// (every candidate down, not really healthy) rather than a genuine
    /// health-filter result — see `finalize_sticky_cookie`'s `#374` note for
    /// why this distinction matters.
    pub(crate) fail_open: bool,
    pub(crate) capacity: capacity::Capacity,
    pub(crate) weighted: Vec<(String, u32)>,
}

/// Build the health/capacity-filtered candidate pool for `all_urls`.
///
/// Returns `Err` with the terminal resolution when the circuit is open
/// (#156) — every healthy peer at its connection cap.
pub(crate) fn build_pool(
    all_urls: &[String],
    all_weighted_base: Vec<(String, u32)>,
    opts: &RouteOptions<'_>,
    route_key: &str,
    ctx: &ProxyCtx<'_>,
) -> Result<CandidatePool, Box<ProxyResolution>> {
    // Filter to healthy upstreams; if all are down keep all (fail-open).
    let (healthy, fail_open) = ctx.upstream_health.filter_healthy(all_urls);
    let healthy_urls: Vec<String> = healthy.iter().cloned().cloned().collect();

    // Circuit breaker: per-upstream capacity filtering (#156). `Exhausted`
    // means every healthy peer is at its connection cap → 503.
    let capacity = capacity::Capacity::evaluate(
        &healthy_urls,
        opts.max_conns_per_upstream,
        route_key,
        ctx.upstream_health,
    );
    if matches!(capacity, capacity::Capacity::Exhausted) {
        return Err(Box::new(ProxyResolution::overloaded(
            ProxyReqState::default(),
        )));
    }

    // Build weighted list filtered to healthy targets. `pick_bounded` further
    // filters this to the admissible (under-capacity) subset internally.
    let weighted: Vec<(String, u32)> = all_weighted_base
        .into_iter()
        .filter(|(url, _)| healthy_urls.contains(url))
        .collect();

    Ok(CandidatePool {
        healthy_urls,
        fail_open,
        capacity,
        weighted,
    })
}

/// The peer chosen for this request, plus the retry state (if `retry` is
/// configured) anchored to it, and everything the caller's final
/// `ProxyReqState`/sticky-cookie steps need to know about how it was chosen.
pub(crate) struct PeerOutcome {
    pub(crate) chosen_url: String,
    pub(crate) is_least_conn: bool,
    /// The exact peer this session's sticky cookie is pinned to, when the
    /// cookie was HMAC-verified against one (#220). `None` in legacy
    /// no-secret mode or when there's no sticky cookie at all.
    pub(crate) pinned: Option<String>,
    /// `true` when the pin above was honored directly for this pick.
    pub(crate) honored_pin: bool,
    /// Threaded through from [`CandidatePool::fail_open`] — needed by
    /// `finalize_sticky_cookie`'s `#374` fail-open distinction.
    pub(crate) fail_open: bool,
    pub(crate) retry_state: Option<RetryState>,
}

/// Resolve sticky-session pinning, pick a peer, and (if `retry` is
/// configured) build its anchored retry list — sticky resolution, the
/// slow-start ramp, and the effective strategy are all needed by both the
/// primary pick and the retry list, so this stays one phase rather than two
/// (constructing a fresh [`Ramp`] a second time would draw a different
/// random seed and break its "one seed per routing decision" invariant —
/// see `slow_start::Ramp`'s own doc comment).
///
/// Returns `Err` with the terminal resolution on a sticky-strict reject or
/// when no candidate could be picked at all.
pub(crate) fn pick_peer_with_retry(
    pool: &CandidatePool,
    opts: &RouteOptions<'_>,
    all_urls: &[String],
    route_key: &str,
    ctx: &ProxyCtx<'_>,
) -> Result<PeerOutcome, Box<ProxyResolution>> {
    // Sticky sessions: extract and optionally verify the session cookie.
    let stickiness = sticky::resolve_sticky(opts.sticky, all_urls, ctx);
    if matches!(stickiness, Sticky::Reject) {
        return Err(Box::new(ProxyResolution::overloaded(
            ProxyReqState::default(),
        )));
    }
    // The exact peer this session is pinned to, when the cookie was
    // HMAC-verified against one (#220). `None` in legacy no-secret mode —
    // there the cookie is only a hash key, never a routing target.
    let pinned: Option<&str> = match &stickiness {
        Sticky::Pinned(url) => Some(url.as_str()),
        _ => None,
    };
    // Hash input for the *fallback* path (pin can't be honored, or no-secret
    // mode). Keeping the pinned URL itself as the input preserves the
    // deterministic, self-healing relocation behavior #156 established.
    let sticky_hash_input: Option<&str> = match &stickiness {
        Sticky::Pinned(url) => Some(url.as_str()),
        Sticky::HashKey(key) => Some(key.as_str()),
        _ => None,
    };

    // Priority: sticky cookie > hash_key config > client IP.
    let hash_val =
        sticky::selection_hash_val(sticky_hash_input, opts.hash_key, ctx.path, ctx.client_ip);
    // When sticky is active, override strategy to consistent-hash so the
    // cookie value is always used for backend selection.
    let strategy = sticky::effective_strategy(sticky_hash_input.is_some(), opts.strategy);

    // Slow start (#157): ramp traffic to a recently-recovered upstream.
    // Constructed after `strategy` is resolved so hash/sticky routes (already
    // forced to `ConsistentHash` above) get the exemption for free -- see
    // `slow_start`'s module doc comment. `Ramp::new` is a true no-op when
    // `slow_start_secs` is unset.
    let ramp = Ramp::new(opts.slow_start_secs, ctx.upstream_health);

    // #220: a pin can be honored only when the peer it names is still
    // healthy AND under its connection cap. Otherwise fall through to the
    // strategy below, which relocates deterministically (and self-heals
    // once the pin is serviceable again — see `finalize_sticky_cookie`).
    let honored_pin: Option<&str> = pinned
        .filter(|url| pool.healthy_urls.iter().any(|h| h == *url) && pool.capacity.admits(url));

    // ONE decision point for "which peer serves this request", used by both
    // retry- and non-retry-configured routes (#366). Previously a route with
    // `retry` took a separate branch that bypassed strategy dispatch
    // entirely and did blind round-robin — so `ipHash`/`consistentHash`,
    // weighted, least-conn AND sticky affinity were all silently ignored the
    // moment `retry` was configured. `routes.rs` already had this shape
    // (pick, then anchor the retry list to what was picked, #367); this
    // brings `router.rs` in line with it.
    let (chosen_url, is_least_conn) = if let Some(pin) = honored_pin {
        // Honoring the pin *is* the routing decision — no strategy dispatch,
        // no hashing. is_least_conn = false: nothing incremented conn_count
        // for us, so the `circuit_tracking` block below owns that slot.
        (pin.to_string(), false)
    } else {
        let input = capacity::BoundedPick {
            strategy,
            healthy: &pool.healthy_urls,
            capacity: &pool.capacity,
            weighted: &pool.weighted,
            route_key,
            hash_val,
            counters: ctx.counters,
            health: ctx.upstream_health,
            ramp: &ramp,
        };
        let Some(picked) = capacity::pick_bounded(&input) else {
            return Err(Box::new(ProxyResolution::unresolved(
                ProxyReqState::default(),
            )));
        };
        picked
    };

    // Retry list, anchored so that `retry.urls[0] == chosen_url` — the
    // invariant `upstream_peer`'s `select_retry_target` relies on for
    // attempt 0 (#367, #216 part 2). Capacity- and ramp-filtered for the
    // same reason the old branch was: a retry must not rotate into a peer
    // already known saturated, and `slowStartSecs` must not become a silent
    // no-op just because `retry` is configured (#157).
    let retry_state = opts.retry.map(|retry| {
        let candidates = pool
            .capacity
            .candidates(&pool.healthy_urls)
            .unwrap_or(&pool.healthy_urls);
        // #375: exempt hash/sticky routes from ramp-filtering the retry
        // candidate list too, mirroring `pick_bounded`'s own exemption for
        // the primary pick. Without this, a hash-based or sticky route
        // with `retry` configured could have a mid-ramp peer silently
        // excluded from retry attempts 1+ -- `slow_start.rs`'s own
        // "structural, needs zero code" claim only ever covered the
        // primary pick, not this separate retry-list filter. The exemption
        // itself lives in `capacity::ramp_filter_retry_candidates`, shared
        // with the `routes[]` path (#436).
        let candidates = capacity::ramp_filter_retry_candidates(strategy, &ramp, candidates);
        retry_state_for(
            &candidates,
            &chosen_url,
            retry,
            opts.max_conns_per_upstream,
            // Mirrors `upstream_conn_slot`'s own formula below. The old
            // retry branch hardcoded this to `max_conns.is_some()`, which
            // was only correct while that branch forced is_least_conn=false;
            // now that retries go through real strategy dispatch, least-conn
            // routes genuinely do track a slot per attempt.
            is_least_conn || opts.max_conns_per_upstream.is_some(),
        )
    });

    Ok(PeerOutcome {
        chosen_url,
        is_least_conn,
        pinned: pinned.map(str::to_owned),
        honored_pin: honored_pin.is_some(),
        fail_open: pool.fail_open,
        retry_state,
    })
}
