//! Per-upstream connection-capacity admission (`healthCheck.maxConnectionsPerUpstream`).
//!
//! One evaluation point for "which healthy upstreams may take another request",
//! shared by all routing paths (legacy `proxy` map, `routes[]` array, `groups`).
//! Before this module, capacity was checked once to decide "are *all* peers
//! maxed" (all-or-nothing 503) but never used to filter which peer a
//! load-balancing strategy could pick — so only `LeastConn` (which always
//! selects the true minimum-load candidate anyway) incidentally respected the
//! cap. See issue #156.
//!
//! [`pick_bounded`] also applies [`crate::slow_start::Ramp`] (issue #157) —
//! a second, soft cross-cutting admission constraint alongside this module's
//! hard `Capacity` one. Both are evaluated here, in that order (capacity
//! first, then ramp), so that neither the root crate's `router.rs`/
//! `routes.rs` nor any `LoadBalancingStrategy` implementation needs to know
//! either exists.
//!
//! Private module (`mod capacity;`, not `pub mod`) — moved here from the
//! root crate's `src/proxy/capacity.rs` in issue #143 PR B; nothing outside
//! the routing/resolution code in this crate ever called it directly.

use std::borrow::Cow;
use std::sync::atomic::AtomicUsize;

use dashmap::DashMap;

use conduit_upstream::health::UpstreamRegistry;
use conduit_upstream::LoadBalanceStrategy;

use crate::slow_start::Ramp;

/// Admission decision for one route's healthy candidate list.
pub(crate) enum Capacity {
    /// No `maxConnectionsPerUpstream` configured, or no healthy candidates at
    /// all — every candidate is admissible (identical to pre-#156 behavior;
    /// an empty candidate list must fall through to static/fallback, not
    /// become a spurious 503).
    Unlimited,
    /// A cap is configured and these candidates are below it. Never empty —
    /// an empty result is represented as `Exhausted` instead.
    Under(Vec<String>),
    /// A cap is configured and every healthy candidate is at or above it —
    /// circuit open, caller must return `LocalHandler::Overloaded` (503).
    Exhausted,
}

impl Capacity {
    /// Partition `healthy` by `conn_load(url) < max`.
    pub(crate) fn evaluate(
        healthy: &[String],
        max_conns: Option<u64>,
        route_key: &str,
        health: &UpstreamRegistry,
    ) -> Self {
        let Some(max_conns) = max_conns else {
            return Self::Unlimited;
        };
        if healthy.is_empty() {
            return Self::Unlimited;
        }
        let under: Vec<String> = healthy
            .iter()
            .filter(|u| health.conn_load(u) < max_conns as usize)
            .cloned()
            .collect();
        if under.is_empty() {
            tracing::debug!(
                route = route_key,
                max_conns,
                "circuit open: all upstreams at connection limit"
            );
            return Self::Exhausted;
        }
        Self::Under(under)
    }

    /// Candidate list the load-balancing strategy should choose from.
    /// `None` means circuit open (503). Borrows `healthy` when `Unlimited`,
    /// so the no-cap path allocates nothing extra.
    pub(crate) fn candidates<'a>(&'a self, healthy: &'a [String]) -> Option<&'a [String]> {
        match self {
            Self::Unlimited => Some(healthy),
            Self::Under(v) => Some(v.as_slice()),
            Self::Exhausted => None,
        }
    }

    /// `true` when `url` may take another request.
    pub(crate) fn admits(&self, url: &str) -> bool {
        match self {
            Self::Unlimited => true,
            Self::Under(v) => v.iter().any(|u| u == url),
            Self::Exhausted => false,
        }
    }
}

/// Hash-ring pick that honors capacity without shrinking the hash domain.
///
/// Starts at `hash_val % ring.len()` — the same index
/// [`conduit_upstream::targets::pick_by_hash`] would return — and walks the ring
/// forward until an admissible peer is found. With [`Capacity::Unlimited`]
/// this is byte-for-byte `pick_by_hash` (see the parity test below).
///
/// This deliberately does NOT filter `ring` down to the admissible subset
/// first: `pick_by_hash` is a naive `hash % len`, not a hash ring with
/// virtual nodes, so shrinking the domain by even one element remaps most
/// clients, not just the ones pinned to the removed peer. Forward-probing
/// keeps every other client's mapping unchanged and relocates only the
/// client(s) whose preferred peer is currently at capacity.
pub(crate) fn hash_pick_bounded(ring: &[String], hash_val: u64, cap: &Capacity) -> Option<String> {
    if ring.is_empty() {
        return None;
    }
    let start = (hash_val as usize) % ring.len();
    (0..ring.len())
        .map(|i| &ring[(start + i) % ring.len()])
        .find(|u| cap.admits(u))
        .cloned()
}

/// Everything one capacity-aware pick needs. Bundled to stay under
/// `clippy::too_many_arguments` and to keep the three call sites uniform.
pub(crate) struct BoundedPick<'a> {
    pub strategy: Option<&'a LoadBalanceStrategy>,
    /// FULL healthy list — the hash ring. NOT the admissible subset; hash
    /// strategies forward-probe over this directly (see [`hash_pick_bounded`]).
    pub healthy: &'a [String],
    pub capacity: &'a Capacity,
    /// `(url, weight)` pairs, already health-filtered by the caller.
    /// Capacity-filtering happens inside [`pick_bounded`] — see its doc.
    pub weighted: &'a [(String, u32)],
    pub route_key: &'a str,
    pub hash_val: u64,
    pub counters: &'a DashMap<String, AtomicUsize>,
    pub health: &'a UpstreamRegistry,
    /// Slow-start admission gate (issue #157). Deliberately not `Option` —
    /// pass [`Ramp::disabled`] for routes without `slowStartSecs`, so every
    /// call site is forced to make an explicit choice rather than silently
    /// defaulting to "no ramp" the way an `Option::None` default would let it.
    pub ramp: &'a Ramp<'a>,
}

/// Whether `strategy` maps each client/route key onto a fixed peer
/// (`ip-hash` / `consistent-hash`; sticky routes reach this as
/// `consistent-hash` via `sticky::effective_strategy`).
///
/// These strategies are exempt from slow start (#157): a client must keep
/// hashing to the same peer, so the ramp must never be consulted for them —
/// not for the primary pick ([`pick_bounded`] returns before it) and not for
/// a retry candidate list ([`ramp_filter_retry_candidates`]). Keeping the
/// predicate in this one module is deliberate: it was duplicated inline at
/// each site, and the retry-list copy went missing on the `routes[]` path
/// (#436) after being added on the `proxy`-map path (#375).
pub(crate) fn is_hash_strategy(strategy: Option<&LoadBalanceStrategy>) -> bool {
    matches!(
        strategy,
        Some(LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash)
    )
}

/// Slow-start (#157) filter for a *retry* candidate list, which is its own
/// routing decision that never goes through [`pick_bounded`].
///
/// Hash-based strategies get the list back untouched (see
/// [`is_hash_strategy`]): a mid-ramp peer is fully eligible for the primary
/// pick under those strategies, so silently dropping it from retry attempts
/// 1+ would make the two disagree (#375, #436). Every other strategy is
/// ramp-filtered, and [`Ramp::filter_candidates`] itself never empties the
/// list. Both retry-list builders (`peer_pick` and `routes_resolve`) must go
/// through here rather than call the ramp directly.
pub(crate) fn ramp_filter_retry_candidates<'c>(
    strategy: Option<&LoadBalanceStrategy>,
    ramp: &Ramp<'_>,
    candidates: &'c [String],
) -> Cow<'c, [String]> {
    if is_hash_strategy(strategy) {
        Cow::Borrowed(candidates)
    } else {
        ramp.filter_candidates(candidates)
    }
}

/// Capacity-aware strategy dispatch. Returns `(url, is_least_conn)` — the
/// same shape as [`conduit_upstream::strategy::LoadBalancingStrategy::pick`].
///
/// `None` means either circuit-open ([`Capacity::Exhausted`]) or no
/// candidate at all — both cases already behave correctly at the call site
/// (503 / fall through to static/fallback).
///
/// This is the ONLY place that branches on `LoadBalanceStrategy` variants
/// for capacity purposes — `router.rs` and `routes.rs` call this and never
/// match on the strategy themselves, so adding a new strategy never requires
/// touching either of them (this keeps the guarantee `strategy.rs`'s own doc
/// comment makes: "No changes to `router.rs` are required"). The same early
/// return for hash-based strategies is also what makes slow-start's
/// hash/sticky exemption (issue #157) structural rather than a separate
/// check: `input.ramp` is never consulted on that path.
///
/// `weighted` is filtered to the admissible subset internally (not by the
/// caller) specifically because [`conduit_upstream::strategy::WeightedRoundRobin`]
/// reads `weighted`, not the plain URL candidate list — filtering only the
/// latter would silently leave WRR still choosing from over-capacity peers.
pub(crate) fn pick_bounded(input: &BoundedPick<'_>) -> Option<(String, bool)> {
    let candidates = input.capacity.candidates(input.healthy)?;

    if is_hash_strategy(input.strategy) {
        return hash_pick_bounded(input.healthy, input.hash_val, input.capacity)
            .map(|u| (u, false));
    }

    // Slow start (#157): narrow the candidate list to peers not currently
    // mid-ramp, before strategy dispatch. This is also why the hash-strategy
    // branch above returns early -- hash-based strategies and sticky sessions
    // are deliberately exempt (see `slow_start`'s module doc comment) and
    // must never reach this filter. Fails open: never empties the list.
    let candidates = input.ramp.filter_candidates(candidates);
    let candidates = candidates.as_ref();

    let filtered: Option<Vec<(String, u32)>> = match input.capacity {
        Capacity::Unlimited => None,
        _ => Some(
            input
                .weighted
                .iter()
                .filter(|(u, _)| input.capacity.admits(u))
                .cloned()
                .collect(),
        ),
    };
    let weighted = filtered.as_deref().unwrap_or(input.weighted);
    // Same gate, applied to the weighted list -- WeightedRoundRobin reads
    // `weighted`, not `candidates`, so without this it would be the one
    // strategy slow-start silently failed to cover (the exact bug class
    // issue #156 already found once: "only one strategy respects the
    // constraint").
    let weighted = input.ramp.filter_weighted(weighted);
    let weighted = weighted.as_ref();

    let strategy = conduit_upstream::strategy::from_config(
        input.strategy.unwrap_or(&LoadBalanceStrategy::RoundRobin),
    );
    strategy.pick(
        candidates,
        weighted,
        input.route_key,
        input.hash_val,
        input.counters,
        input.health,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_upstream::targets as upstream;

    fn urls(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("http://u{i}:80")).collect()
    }

    #[test]
    fn evaluate_unlimited_when_no_cap() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        let cap = Capacity::evaluate(&healthy, None, "r", &reg);
        assert!(matches!(cap, Capacity::Unlimited));
    }

    #[test]
    fn evaluate_unlimited_for_empty_candidate_list() {
        let reg = UpstreamRegistry::new();
        let cap = Capacity::evaluate(&[], Some(1), "r", &reg);
        assert!(matches!(cap, Capacity::Unlimited));
    }

    #[test]
    fn evaluate_under_excludes_peer_at_limit() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        reg.conn_inc(&healthy[0]); // load 1, at the cap
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        assert!(cap.admits(&healthy[1]));
        assert!(!cap.admits(&healthy[0]));
    }

    #[test]
    fn evaluate_under_excludes_peer_over_limit() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        reg.conn_inc(&healthy[0]);
        reg.conn_inc(&healthy[0]); // load 2, over a cap of 1
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        assert!(!cap.admits(&healthy[0]));
    }

    #[test]
    fn evaluate_exhausted_when_all_at_limit() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        reg.conn_inc(&healthy[0]);
        reg.conn_inc(&healthy[1]);
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        assert!(matches!(cap, Capacity::Exhausted));
    }

    #[test]
    fn candidates_returns_full_list_when_unlimited() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(3);
        let cap = Capacity::evaluate(&healthy, None, "r", &reg);
        assert_eq!(cap.candidates(&healthy), Some(healthy.as_slice()));
    }

    #[test]
    fn candidates_returns_none_when_exhausted() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(1);
        reg.conn_inc(&healthy[0]);
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        assert_eq!(cap.candidates(&healthy), None);
    }

    #[test]
    fn hash_pick_bounded_matches_pick_by_hash_when_unlimited() {
        let reg = UpstreamRegistry::new();
        let ring = urls(5);
        let cap = Capacity::evaluate(&ring, None, "r", &reg);
        for hash_val in [0u64, 1, 4, 5, 9, 12345, u64::MAX] {
            assert_eq!(
                hash_pick_bounded(&ring, hash_val, &cap),
                upstream::pick_by_hash(&ring, hash_val),
                "hash_val={hash_val}"
            );
        }
    }

    #[test]
    fn hash_pick_bounded_keeps_preferred_peer_and_spills_forward_to_next_index() {
        let reg = UpstreamRegistry::new();
        let ring = urls(3);
        // hash_val = 0 prefers index 0.
        let preferred = upstream::pick_by_hash(&ring, 0).unwrap();
        assert_eq!(preferred, ring[0]);

        // Under capacity: unchanged mapping.
        let cap = Capacity::evaluate(&ring, Some(1), "r", &reg);
        assert_eq!(hash_pick_bounded(&ring, 0, &cap), Some(ring[0].clone()));

        // Saturate the preferred peer: spill to the next ring index (1), not
        // an arbitrary admissible peer.
        reg.conn_inc(&ring[0]);
        let cap = Capacity::evaluate(&ring, Some(1), "r", &reg);
        assert_eq!(hash_pick_bounded(&ring, 0, &cap), Some(ring[1].clone()));

        // Slot frees up: mapping returns to the preferred peer.
        reg.conn_dec(&ring[0]);
        let cap = Capacity::evaluate(&ring, Some(1), "r", &reg);
        assert_eq!(hash_pick_bounded(&ring, 0, &cap), Some(ring[0].clone()));
    }

    #[test]
    fn hash_pick_bounded_wraps_around_ring() {
        let reg = UpstreamRegistry::new();
        let ring = urls(3);
        // hash_val = 2 prefers the last index; saturate it and the wrap
        // target (index 0) too, leaving only index 1 admissible.
        reg.conn_inc(&ring[2]);
        reg.conn_inc(&ring[0]);
        let cap = Capacity::evaluate(&ring, Some(1), "r", &reg);
        assert_eq!(hash_pick_bounded(&ring, 2, &cap), Some(ring[1].clone()));
    }

    #[test]
    fn hash_pick_bounded_returns_none_for_empty_ring() {
        let reg = UpstreamRegistry::new();
        let cap = Capacity::evaluate(&[], None, "r", &reg);
        assert_eq!(hash_pick_bounded(&[], 0, &cap), None);
    }

    // ── pick_bounded dispatcher ─────────────────────────────────────────────

    fn counters() -> DashMap<String, AtomicUsize> {
        DashMap::new()
    }

    #[test]
    fn pick_bounded_exhausted_returns_none() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(1);
        reg.conn_inc(&healthy[0]);
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        let weighted = [(healthy[0].clone(), 1u32)];
        let counters = counters();
        let ramp = Ramp::disabled(&reg);
        let input = BoundedPick {
            strategy: None,
            healthy: &healthy,
            capacity: &cap,
            weighted: &weighted,
            route_key: "r",
            hash_val: 0,
            counters: &counters,
            health: &reg,
            ramp: &ramp,
        };
        assert!(pick_bounded(&input).is_none());
    }

    #[test]
    fn pick_bounded_weighted_round_robin_never_picks_at_capacity_peer() {
        // WeightedRoundRobin reads `weighted`, not the plain URL list — this
        // proves pick_bounded's internal weighted-filter (not the caller) is
        // what keeps it honest, per issue #156's WRR finding.
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        reg.conn_inc(&healthy[0]); // saturate peer 0 at cap 1
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        let weighted = [(healthy[0].clone(), 10u32), (healthy[1].clone(), 1u32)];
        let counters = counters();
        let ramp = Ramp::disabled(&reg);
        for _ in 0..10 {
            let input = BoundedPick {
                strategy: Some(&LoadBalanceStrategy::WeightedRoundRobin),
                healthy: &healthy,
                capacity: &cap,
                weighted: &weighted,
                route_key: "r",
                hash_val: 0,
                counters: &counters,
                health: &reg,
                ramp: &ramp,
            };
            let (url, is_least_conn) = pick_bounded(&input).expect("one admissible peer");
            assert_eq!(url, healthy[1], "must never pick the saturated peer");
            assert!(!is_least_conn);
        }
    }

    #[test]
    fn pick_bounded_least_conn_result_unchanged_by_filtering() {
        // LeastConn already picks the true minimum-load candidate; filtering
        // to the under-capacity subset must be a no-op for it.
        let reg = UpstreamRegistry::new();
        let healthy = urls(2); // both start at load 0
        let cap = Capacity::evaluate(&healthy, Some(5), "r", &reg);
        let counters = counters();
        let ramp = Ramp::disabled(&reg);
        let input = BoundedPick {
            strategy: Some(&LoadBalanceStrategy::LeastConn),
            healthy: &healthy,
            capacity: &cap,
            weighted: &[],
            route_key: "r",
            hash_val: 0,
            counters: &counters,
            health: &reg,
            ramp: &ramp,
        };
        let (url, is_least_conn) = pick_bounded(&input).expect("candidates present");
        assert!(healthy.contains(&url));
        assert!(is_least_conn, "LeastConn must report the acquired slot");
        reg.conn_dec(&url); // balance the slot LeastConn acquired
    }

    #[test]
    fn pick_bounded_least_response_time_skips_saturated_lowest_latency_peer() {
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        reg.statuses
            .entry(healthy[0].clone())
            .or_default()
            .latency_ms = Some(1); // fastest
        reg.statuses
            .entry(healthy[1].clone())
            .or_default()
            .latency_ms = Some(100);
        reg.conn_inc(&healthy[0]); // saturate the fastest peer at cap 1
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        let counters = counters();
        let ramp = Ramp::disabled(&reg);
        let input = BoundedPick {
            strategy: Some(&LoadBalanceStrategy::LeastResponseTime),
            healthy: &healthy,
            capacity: &cap,
            weighted: &[],
            route_key: "r",
            hash_val: 0,
            counters: &counters,
            health: &reg,
            ramp: &ramp,
        };
        let (url, _) = pick_bounded(&input).expect("one admissible peer");
        assert_eq!(
            url, healthy[1],
            "must skip the saturated peer even though it's fastest"
        );
    }

    // ── slow start (#157) ────────────────────────────────────────────────────

    fn mark_just_recovered(reg: &UpstreamRegistry, url: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .recovery_time_secs = Some(now);
    }

    #[test]
    fn pick_bounded_least_conn_avoids_a_zero_fraction_peer_with_a_healthy_sibling() {
        // This is the negative control that actually proves #157 is fixed:
        // LeastConn picks the true minimum-conn-load candidate, and a
        // just-recovered peer's load has drained to 0 while its siblings
        // carry real traffic -- so *before* this fix, LeastConn picked the
        // recovering peer on every single call (the worst offender of all
        // seven strategies, per the architect review for #157).
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        mark_just_recovered(&reg, &healthy[0]); // fraction 0.0
        reg.conn_inc(&healthy[1]); // sibling carries real load
        let cap = Capacity::evaluate(&healthy, None, "r", &reg);
        let counters = counters();
        let ramp = Ramp::new(Some(30), &reg);
        for _ in 0..20 {
            let input = BoundedPick {
                strategy: Some(&LoadBalanceStrategy::LeastConn),
                healthy: &healthy,
                capacity: &cap,
                weighted: &[],
                route_key: "r",
                hash_val: 0,
                counters: &counters,
                health: &reg,
                ramp: &ramp,
            };
            let (url, is_least_conn) = pick_bounded(&input).expect("candidates present");
            assert_eq!(
                url, healthy[1],
                "must not pick the just-recovered peer despite its lower conn load"
            );
            assert!(is_least_conn);
            reg.conn_dec(&url); // balance the slot LeastConn acquired, keep load fixed
        }
    }

    #[test]
    fn pick_bounded_weighted_round_robin_avoids_a_zero_fraction_peer() {
        // Proves the `filter_weighted` half of the fix specifically:
        // WeightedRoundRobin reads `weighted`, not `healthy`, so filtering
        // only the plain candidate list (and not `weighted` too) would leave
        // this the one strategy slow-start silently failed to cover -- the
        // exact bug class #156 already found once for capacity.
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        mark_just_recovered(&reg, &healthy[0]); // fraction 0.0
        let cap = Capacity::evaluate(&healthy, None, "r", &reg);
        let weighted = [(healthy[0].clone(), 10u32), (healthy[1].clone(), 1u32)];
        let counters = counters();
        let ramp = Ramp::new(Some(30), &reg);
        for _ in 0..20 {
            let input = BoundedPick {
                strategy: Some(&LoadBalanceStrategy::WeightedRoundRobin),
                healthy: &healthy,
                capacity: &cap,
                weighted: &weighted,
                route_key: "r",
                hash_val: 0,
                counters: &counters,
                health: &reg,
                ramp: &ramp,
            };
            let (url, _) = pick_bounded(&input).expect("one admissible peer");
            assert_eq!(
                url, healthy[1],
                "must never pick the just-recovered peer even at 10x weight"
            );
        }
    }

    #[test]
    fn pick_bounded_hash_strategy_ignores_ramp() {
        // Proves the hash/sticky exemption end-to-end: a peer at fraction 0.0
        // sitting at the preferred ring index is still selected -- ramp must
        // never be consulted on this path.
        let reg = UpstreamRegistry::new();
        let ring = urls(3);
        mark_just_recovered(&reg, &ring[0]); // fraction 0.0, preferred by hash_val=0
        let cap = Capacity::evaluate(&ring, None, "r", &reg);
        let counters = counters();
        let ramp = Ramp::new(Some(30), &reg);
        let input = BoundedPick {
            strategy: Some(&LoadBalanceStrategy::ConsistentHash),
            healthy: &ring,
            capacity: &cap,
            weighted: &[],
            route_key: "r",
            hash_val: 0,
            counters: &counters,
            health: &reg,
            ramp: &ramp,
        };
        let (url, _) = pick_bounded(&input).expect("ring non-empty");
        assert_eq!(
            url, ring[0],
            "hash-based strategies must ignore slow-start entirely"
        );
    }

    #[test]
    fn pick_bounded_fails_open_when_the_only_under_capacity_peer_is_ramping() {
        // Capacity (hard) and ramp (soft) interaction: peer 1 is over its cap
        // (excluded by Capacity), peer 0 is under capacity but at fraction
        // 0.0 -- ramp's fail-open rule must still admit it rather than
        // returning no candidate at all.
        let reg = UpstreamRegistry::new();
        let healthy = urls(2);
        mark_just_recovered(&reg, &healthy[0]); // fraction 0.0, but under capacity
        reg.conn_inc(&healthy[1]); // saturate the sibling at cap 1
        let cap = Capacity::evaluate(&healthy, Some(1), "r", &reg);
        let counters = counters();
        let ramp = Ramp::new(Some(30), &reg);
        let input = BoundedPick {
            strategy: Some(&LoadBalanceStrategy::RoundRobin),
            healthy: &healthy,
            capacity: &cap,
            weighted: &[],
            route_key: "r",
            hash_val: 0,
            counters: &counters,
            health: &reg,
            ramp: &ramp,
        };
        let (url, _) = pick_bounded(&input).expect("must fail open, not return None");
        assert_eq!(url, healthy[0]);
    }

    // ── retry-list ramp exemption (#375, #436) ───────────────────────────────

    #[test]
    fn is_hash_strategy_is_true_only_for_the_two_hash_strategies() {
        assert!(is_hash_strategy(Some(&LoadBalanceStrategy::IpHash)));
        assert!(is_hash_strategy(Some(&LoadBalanceStrategy::ConsistentHash)));
        // `None` means "default strategy" (round-robin), never hash.
        assert!(!is_hash_strategy(None));
        assert!(!is_hash_strategy(Some(&LoadBalanceStrategy::RoundRobin)));
        assert!(!is_hash_strategy(Some(&LoadBalanceStrategy::LeastConn)));
        assert!(!is_hash_strategy(Some(
            &LoadBalanceStrategy::WeightedRoundRobin
        )));
    }

    #[test]
    fn ramp_filter_retry_candidates_keeps_a_mid_ramp_peer_only_for_hash_strategies() {
        // peer 0 just recovered (fraction 0.0 over a 1h window -- excluded by
        // the ramp filter with probability 1 for all practical purposes);
        // peer 1 is fully ramped, so the filter can't fail open and both
        // outcomes below are deterministic.
        let reg = UpstreamRegistry::new();
        let list = urls(2);
        mark_just_recovered(&reg, &list[0]);
        let ramp = Ramp::new(Some(3600), &reg);

        for hash in [
            LoadBalanceStrategy::IpHash,
            LoadBalanceStrategy::ConsistentHash,
        ] {
            let kept = ramp_filter_retry_candidates(Some(&hash), &ramp, &list);
            assert_eq!(
                kept.as_ref(),
                list.as_slice(),
                "{hash:?} is ramp-exempt: the retry list must be returned untouched"
            );
            assert!(
                matches!(kept, Cow::Borrowed(_)),
                "exempt path allocates nothing"
            );
        }

        // Control: every non-hash strategy (and the `None` default) is still
        // ramp-filtered. Without this, an over-broad exemption -- e.g. always
        // returning the list untouched -- would pass the loop above.
        for other in [
            None,
            Some(LoadBalanceStrategy::RoundRobin),
            Some(LoadBalanceStrategy::LeastConn),
        ] {
            let kept = ramp_filter_retry_candidates(other.as_ref(), &ramp, &list);
            assert_eq!(
                kept.as_ref(),
                &list[1..],
                "{other:?} must still drop the mid-ramp peer from the retry list"
            );
        }
    }
}
