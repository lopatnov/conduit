//! Load-balancing strategy trait and concrete implementations.
//!
//! Each strategy is a zero-sized struct that implements [`LoadBalancingStrategy`].
//! Adding a new strategy:
//!
//! 1. Create a struct (e.g. `pub struct CanaryStrategy;`).
//! 2. `impl LoadBalancingStrategy for CanaryStrategy { ... }`.
//! 3. Add a variant to [`crate::config::LoadBalanceStrategy`].
//! 4. Map it in [`from_config`].
//!
//! No changes to `router.rs` are required.

use std::sync::atomic::AtomicUsize;

use dashmap::DashMap;

use crate::config::LoadBalanceStrategy;
use crate::health::UpstreamRegistry;
use crate::targets;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A load-balancing strategy that selects one upstream URL from a candidate list.
///
/// Implementations are zero-sized and referenced as `'static` singletons — no
/// heap allocation on the hot path.
pub trait LoadBalancingStrategy: Send + Sync {
    /// Pick one URL from `urls`.
    ///
    /// # Parameters
    /// - `urls`      — healthy candidate URLs (already filtered by the caller)
    /// - `weighted`  — `(url, weight)` pairs; read by [`WeightedRoundRobin`], [`LeastConn`],
    ///   [`P2cChoice`] (issue #506) and [`LeastBandwidth`] (issue #627), ignored by the rest. A URL it does not name has weight 1.
    /// - `route_key` — stable key for per-route counters (round-robin cursor, etc.)
    /// - `hash_val`  — FNV-1a hash of the relevant input; used by hash strategies
    /// - `counters`  — shared per-route atomic counters for rotation
    /// - `health`    — upstream registry; used by [`LeastConn`]
    ///
    /// # Returns
    /// `Some((url, holds_slot))` where `holds_slot = true` means the
    /// strategy already incremented the inflight counter on `health` and the
    /// caller is responsible for decrementing it after the response completes.
    fn pick(
        &self,
        urls: &[String],
        weighted: &[(String, u32)],
        route_key: &str,
        hash_val: u64,
        counters: &DashMap<String, AtomicUsize>,
        health: &UpstreamRegistry,
    ) -> Option<(String, bool)>;
}

// ── Concrete strategies ───────────────────────────────────────────────────────

/// Rotate through backends in order, one request at a time.
pub struct RoundRobin;

impl LoadBalancingStrategy for RoundRobin {
    fn pick(
        &self,
        urls: &[String],
        _weighted: &[(String, u32)],
        route_key: &str,
        _hash_val: u64,
        counters: &DashMap<String, AtomicUsize>,
        _health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        targets::pick_round_robin(urls, route_key, counters).map(|u| (u, false))
    }
}

/// Pick a backend uniformly at random on every request.
pub struct Random;

impl LoadBalancingStrategy for Random {
    fn pick(
        &self,
        urls: &[String],
        _weighted: &[(String, u32)],
        route_key: &str,
        _hash_val: u64,
        counters: &DashMap<String, AtomicUsize>,
        _health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        targets::pick_random(urls, route_key, counters).map(|u| (u, false))
    }
}

/// Send each request to the backend with the fewest active connections **per unit of
/// weight** (issue #506): a weight-3 backend carries three times the in-flight requests of a
/// weight-1 one before the next request goes elsewhere. With every weight 1 (plain string
/// targets) this is plain least-connections.
pub struct LeastConn;

impl LoadBalancingStrategy for LeastConn {
    fn pick(
        &self,
        urls: &[String],
        weighted: &[(String, u32)],
        _route_key: &str,
        _hash_val: u64,
        _counters: &DashMap<String, AtomicUsize>,
        health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        // pick_least_conn increments the inflight counter on the chosen URL.
        health.pick_least_conn(urls, weighted).map(|u| (u, true))
    }
}

/// Rotate in proportion to explicit `weight` values.
pub struct WeightedRoundRobin;

impl LoadBalancingStrategy for WeightedRoundRobin {
    fn pick(
        &self,
        _urls: &[String],
        weighted: &[(String, u32)],
        route_key: &str,
        _hash_val: u64,
        counters: &DashMap<String, AtomicUsize>,
        _health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        targets::pick_weighted_round_robin(weighted, route_key, counters).map(|u| (u, false))
    }
}

/// Map each client (or URL) to a consistent backend via a hash.
///
/// Used for both `ip-hash` and `consistent-hash` — both reduce to a rendezvous
/// hash over the candidate list (issue #377).
pub struct HashBased;

impl LoadBalancingStrategy for HashBased {
    fn pick(
        &self,
        urls: &[String],
        _weighted: &[(String, u32)],
        _route_key: &str,
        hash_val: u64,
        _counters: &DashMap<String, AtomicUsize>,
        _health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        targets::pick_by_hash(urls, hash_val).map(|u| (u, false))
    }
}

/// Send each request to the backend with the lowest recent latency.
pub struct LeastResponseTime;

impl LoadBalancingStrategy for LeastResponseTime {
    fn pick(
        &self,
        urls: &[String],
        _weighted: &[(String, u32)],
        route_key: &str,
        _hash_val: u64,
        counters: &DashMap<String, AtomicUsize>,
        health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        targets::pick_least_response_time(urls, health, route_key, counters).map(|u| (u, false))
    }
}

/// Power of Two Choices (P2C): sample 2 random backends and forward to the one with the lower
/// **cost**, `ewma_latency * (in_flight + 1) / weight`.
///
/// The cost is Tower's `PeakEwma` load (`rtt estimate * (pending + 1)`, which linkerd2-proxy's P2C
/// balancer uses) divided by the target weight. Selection is O(1): two samples, two lookups.
///
/// - `ewma_latency` is the passive, time-decayed latency in `UpstreamEntry.ewma_latency_us`
///   (30 ms for a peer with no sample yet).
/// - `in_flight` is the registry's per-upstream count, shared with `least-conn` and the circuit
///   breaker. P2C increments it for the chosen peer and reports that it did (the `true` in the
///   return value), so the caller releases it when the response completes. Without this term
///   (before issue #628) P2C compared latency alone and kept sending to the faster peer however
///   busy it was.
/// - `weight` (issue #506) divides the cost, so at equal latency a weight-3 peer carries about
///   three times the in-flight requests of a weight-1 peer. With every weight 1 and nothing in
///   flight the cost is the latency, as before. A weight-0 peer loses every comparison.
///
/// Reference: linkerd2-proxy `linkerd/pool/p2c/src/lib.rs`; Tower `tower/src/load/peak_ewma.rs`.
pub struct P2cChoice;

/// Default EWMA value used when an upstream has no recorded latency yet.
/// 30 ms — matches linkerd's `DEFAULT_RTT = 30 ms`.
const P2C_DEFAULT_LATENCY_US: f64 = 30_000.0;

/// The cost P2C compares: `ewma_latency * (in_flight + 1) / weight`; infinite for weight 0.
fn p2c_cost(health: &UpstreamRegistry, weighted: &[(String, u32)], index: usize, url: &str) -> f64 {
    let latency = health
        .statuses
        .get(url)
        .map(|e| crate::health::decayed_ewma_us(&e, crate::health::now_secs_f64()))
        .unwrap_or(P2C_DEFAULT_LATENCY_US);
    let in_flight = health.conn_load(url) as f64 + 1.0;
    match targets::weight_for(weighted, index, url) {
        0 => f64::INFINITY,
        w => latency * in_flight / f64::from(w),
    }
}

impl LoadBalancingStrategy for P2cChoice {
    fn pick(
        &self,
        urls: &[String],
        weighted: &[(String, u32)],
        _route_key: &str,
        _hash_val: u64,
        _counters: &DashMap<String, AtomicUsize>,
        health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        let chosen = match urls.len() {
            0 => return None,
            // Trivial case: only one choice.
            1 => &urls[0],
            len => match sample_pair(urls, weighted) {
                // Two distinct random indices, both with weight: pick the lower-cost one.
                Some((a, b)) => {
                    let cost_a = p2c_cost(health, weighted, a, &urls[a]);
                    let cost_b = p2c_cost(health, weighted, b, &urls[b]);
                    // Pick the lower cost; break ties in favour of `a`.
                    if cost_a <= cost_b {
                        &urls[a]
                    } else {
                        &urls[b]
                    }
                }
                // Most of the peers have weight 0 (or there are only two and one of them has): look at
                // all of them. A weight-0 peer costs infinity, so one that has weight wins; with none,
                // the first does.
                None => {
                    let cost = |i: usize| p2c_cost(health, weighted, i, &urls[i]);
                    let best = (0..len)
                        .min_by(|&i, &j| cost(i).total_cmp(&cost(j)))
                        .unwrap_or(0);
                    &urls[best]
                }
            },
        };
        // The request is now in flight on `chosen`; the caller releases the slot after the response.
        health.conn_inc(chosen);
        Some((chosen.clone(), true))
    }
}

/// Send each request to the backend currently moving the least traffic, per unit of weight
/// (issue #627).
///
/// ```text
/// cost = sqrt((bytes per second + 1) * (requests in flight + 1)) / weight
/// ```
///
/// - *Bytes per second*: the request-body plus response-body bytes the proxy moved for the upstream,
///   smoothed over about ten seconds ([`crate::traffic::TrafficMeter`]). The proxy meters an upstream
///   only on routes that use this strategy, so the first requests of a route see every upstream at 0.
/// - *In flight*: the registry's per-upstream count, which this strategy increments for the chosen
///   peer and reports (the `true` in the return value) so that the caller releases it, as for `p2c`.
///   The rate is refreshed once per 250 ms window, so on its own it is stale for every request inside
///   a window (the harness of issue #629 measured the share of one upstream swinging by 26 points a
///   second), and a slow upstream moves fewer bytes and looks idle; the in-flight factor reacts at
///   once and grows on a slow upstream.
/// - The square root keeps `weight` proportional: both factors grow with an upstream's share of the
///   traffic, so their product grows with its square, and the root brings that back to the share
///   itself (measured: weights 3:1:1 gave a 58/21/21 split).
/// - With more than two upstreams only two, sampled at random, are compared, as in `p2c`: a scan of
///   all of them sends every request of a window to the one that was quietest when it opened.
///
/// An idle pool ties at 1 and is spread at random, in proportion to weight.
///
/// Prior art: Citrix ADC `leastbandwidth` (bytes sent plus received over the previous 14 seconds,
/// weighted), which has neither the in-flight factor nor the sampling.
pub struct LeastBandwidth;

impl LoadBalancingStrategy for LeastBandwidth {
    /// Compare two sampled upstreams (all of them for two or fewer) and send the request to the cheaper one,
    /// taking an in-flight slot on it that the caller releases.
    fn pick(
        &self,
        urls: &[String],
        weighted: &[(String, u32)],
        _route_key: &str,
        _hash_val: u64,
        _counters: &DashMap<String, AtomicUsize>,
        health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        // One clock reading for the whole pick.
        let now = crate::traffic::monotonic_ms();
        let cost = |url: &str| {
            ((health.traffic_rate_at(url, now) + 1.0) * (health.conn_load(url) as f64 + 1.0)).sqrt()
        };
        let index = match urls.len() {
            0 => return None,
            // With two peers a pair is both of them.
            1 | 2 => targets::pick_lowest_cost(urls, weighted, 0..urls.len(), cost)?,
            len => match sample_pair(urls, weighted) {
                Some((a, b)) => {
                    targets::pick_lowest_cost(urls, weighted, [a, b].into_iter(), cost)?
                }
                // Most of the peers have weight 0: every peer is a candidate, the zero-weight ones are
                // skipped inside.
                None => targets::pick_lowest_cost(urls, weighted, 0..len, cost)?,
            },
        };
        // The request is now in flight on the chosen peer; the caller releases the slot.
        health.conn_inc(&urls[index]);
        Some((urls[index].clone(), true))
    }
}

/// Two distinct random indices of `urls` whose peers both have a non-zero weight, for `p2c` and
/// `least-bandwidth` to compare. A few draws are enough when most peers have weight (the usual case costs
/// two weight lookups); `None` when they are not, and the caller then looks at every peer, so that a
/// weight-0 peer is never chosen over a peer that has weight. Needs `urls.len() >= 2`.
fn sample_pair(urls: &[String], weighted: &[(String, u32)]) -> Option<(usize, usize)> {
    for _ in 0..4 {
        let (a, b) = gen_pair(urls.len());
        if targets::weight_for(weighted, a, &urls[a]) != 0
            && targets::weight_for(weighted, b, &urls[b]) != 0
        {
            return Some((a, b));
        }
    }
    None
}

/// Generate two distinct random indices in `[0, len)` using a fast XorShift.
fn gen_pair(len: usize) -> (usize, usize) {
    let x = targets::fast_random();

    let a = (x % len as u64) as usize;
    let b = ((x >> 32) % (len as u64 - 1)) as usize;
    let b = if b >= a { b + 1 } else { b };
    (a, b)
}

// ── Static singletons (zero allocation on hot path) ───────────────────────────

static ROUND_ROBIN: RoundRobin = RoundRobin;
static RANDOM: Random = Random;
static LEAST_CONN: LeastConn = LeastConn;
static WEIGHTED_RR: WeightedRoundRobin = WeightedRoundRobin;
static HASH_BASED: HashBased = HashBased;
static LEAST_RT: LeastResponseTime = LeastResponseTime;
static P2C: P2cChoice = P2cChoice;
static LEAST_BANDWIDTH: LeastBandwidth = LeastBandwidth;

/// Return the `'static` strategy that corresponds to the config enum variant.
pub fn from_config(s: &LoadBalanceStrategy) -> &'static dyn LoadBalancingStrategy {
    match s {
        LoadBalanceStrategy::RoundRobin => &ROUND_ROBIN,
        LoadBalanceStrategy::Random => &RANDOM,
        LoadBalanceStrategy::LeastConn => &LEAST_CONN,
        LoadBalanceStrategy::WeightedRoundRobin => &WEIGHTED_RR,
        LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash => &HASH_BASED,
        LoadBalanceStrategy::LeastResponseTime => &LEAST_RT,
        LoadBalanceStrategy::P2c => &P2C,
        LoadBalanceStrategy::LeastBandwidth => &LEAST_BANDWIDTH,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::UpstreamRegistry;

    fn counters() -> DashMap<String, AtomicUsize> {
        DashMap::new()
    }
    fn registry() -> UpstreamRegistry {
        UpstreamRegistry::new()
    }
    fn urls(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("http://b{}:4000", i)).collect()
    }

    #[test]
    fn round_robin_cycles() {
        let c = counters();
        let u = urls(3);
        let s = &ROUND_ROBIN as &dyn LoadBalancingStrategy;
        let picks: Vec<String> = (0..6)
            .map(|_| s.pick(&u, &[], "r", 0, &c, &registry()).unwrap().0)
            .collect();
        // Each URL must appear exactly twice in 6 picks (round-robin).
        for url in &u {
            assert_eq!(picks.iter().filter(|p| *p == url).count(), 2);
        }
    }

    #[test]
    fn random_picks_from_list() {
        let c = counters();
        let u = urls(3);
        let (picked, lc) = RANDOM.pick(&u, &[], "r", 0, &c, &registry()).unwrap();
        assert!(u.contains(&picked));
        assert!(!lc);
    }

    #[test]
    fn least_conn_returns_holds_slot_true() {
        let u = urls(2);
        let reg = registry();
        let (_, is_lc) = LEAST_CONN.pick(&u, &[], "r", 0, &counters(), &reg).unwrap();
        assert!(is_lc, "LeastConn must signal holds_slot = true");
    }

    #[test]
    fn weighted_rr_respects_weights() {
        let c = counters();
        let weighted = vec![
            ("http://heavy:4000".to_owned(), 3u32),
            ("http://light:4000".to_owned(), 1u32),
        ];
        let picks: Vec<String> = (0..4)
            .map(|_| {
                WEIGHTED_RR
                    .pick(&[], &weighted, "r", 0, &c, &registry())
                    .unwrap()
                    .0
            })
            .collect();
        let heavy = picks.iter().filter(|p| p.contains("heavy")).count();
        let light = picks.iter().filter(|p| p.contains("light")).count();
        assert_eq!(heavy, 3, "heavy should get 3 out of 4 requests");
        assert_eq!(light, 1, "light should get 1 out of 4 requests");
    }

    #[test]
    fn hash_based_is_stable_for_same_hash() {
        let u = urls(4);
        let hash = 12345u64;
        let first = HASH_BASED
            .pick(&u, &[], "r", hash, &counters(), &registry())
            .unwrap()
            .0;
        let second = HASH_BASED
            .pick(&u, &[], "r", hash, &counters(), &registry())
            .unwrap()
            .0;
        assert_eq!(first, second, "same hash must always pick the same URL");
    }

    #[test]
    fn hash_based_distributes_across_backends() {
        let u = urls(4);
        let picks: std::collections::HashSet<String> = (0u64..20)
            .filter_map(|h| {
                HASH_BASED
                    .pick(&u, &[], "r", h * 999_983, &counters(), &registry())
                    .map(|(url, _)| url)
            })
            .collect();
        assert!(
            picks.len() > 1,
            "different hashes should hit different backends"
        );
    }

    #[test]
    fn from_config_covers_all_variants() {
        use LoadBalanceStrategy::*;
        for s in &[
            RoundRobin,
            Random,
            LeastConn,
            WeightedRoundRobin,
            IpHash,
            ConsistentHash,
            LeastResponseTime,
            P2c,
            LeastBandwidth,
        ] {
            let strategy = from_config(s);
            let u = urls(2);
            // Just check it doesn't panic with a minimal call.
            let weighted = vec![("http://b0:4000".to_owned(), 1u32)];
            let _ = strategy.pick(&u, &weighted, "key", 0, &counters(), &registry());
        }
    }

    #[test]
    fn p2c_selects_from_candidate_list() {
        let u = urls(4);
        let reg = registry();
        let (picked, lc) = P2C.pick(&u, &[], "r", 0, &counters(), &reg).unwrap();
        assert!(u.contains(&picked), "P2C must pick from the candidate list");
        // #628: P2C counts the request in flight, so the caller must release it afterwards.
        assert!(lc, "P2C holds an in-flight slot and must say so");
        assert_eq!(reg.conn_load(&picked), 1);
    }

    #[test]
    fn p2c_counts_a_single_peer_in_flight_too() {
        let u = vec!["http://only:4000".to_owned()];
        let reg = registry();
        let (picked, lc) = P2C.pick(&u, &[], "r", 0, &counters(), &reg).unwrap();
        assert!(lc);
        assert_eq!(reg.conn_load(&picked), 1);
    }

    #[test]
    fn p2c_single_url_returns_it() {
        let u = vec!["http://only:4000".to_owned()];
        let (picked, _) = P2C.pick(&u, &[], "r", 0, &counters(), &registry()).unwrap();
        assert_eq!(picked, "http://only:4000");
    }

    #[test]
    fn p2c_distributes_across_backends() {
        let u = urls(4);
        let reg = registry();
        let c = counters();
        let picks: std::collections::HashSet<String> = (0..40)
            .filter_map(|_| P2C.pick(&u, &[], "r", 0, &c, &reg).map(|(url, _)| url))
            .collect();
        assert!(
            picks.len() > 1,
            "P2C should hit more than one backend over 40 picks"
        );
    }

    // ── weights for least-conn and p2c (#506) ─────────────────────────────────

    fn seed_latency(reg: &UpstreamRegistry, url: &str, us: f64) {
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .ewma_latency_us = us;
    }

    fn two(weight_a: u32, weight_b: u32) -> (Vec<String>, Vec<(String, u32)>) {
        let u = urls(2);
        let w = vec![(u[0].clone(), weight_a), (u[1].clone(), weight_b)];
        (u, w)
    }

    /// How many of 200 picks (a pair of two always compares both peers) land on `u[index]`. Each
    /// request finishes before the next one, so the in-flight counts stay as the caller set them.
    fn p2c_wins(u: &[String], w: &[(String, u32)], reg: &UpstreamRegistry, index: usize) -> usize {
        let c = counters();
        (0..200)
            .filter(|_| {
                let (url, _) = P2C.pick(u, w, "r", 0, &c, reg).unwrap();
                reg.conn_dec(&url);
                url == u[index]
            })
            .count()
    }

    /// Requests that never finish: how many of `n` land on each of `u`.
    fn p2c_held_open(
        u: &[String],
        w: &[(String, u32)],
        reg: &UpstreamRegistry,
        n: usize,
    ) -> Vec<usize> {
        let c = counters();
        for _ in 0..n {
            P2C.pick(u, w, "r", 0, &c, reg).unwrap();
        }
        u.iter().map(|url| reg.conn_load(url)).collect()
    }

    // ── p2c counts requests in flight (#628) ──────────────────────────────────

    /// Idle, the faster peer wins every comparison; with three requests in flight on it, the slower idle peer wins.
    #[test]
    fn p2c_moves_to_the_slower_peer_when_the_faster_one_is_busy() {
        let (u, w) = two(1, 1);
        let reg = registry();
        seed_latency(&reg, &u[0], 10_000.0);
        seed_latency(&reg, &u[1], 20_000.0);
        // Idle: the faster peer wins every comparison.
        assert_eq!(p2c_wins(&u, &w, &reg, 0), 200);
        // The faster peer carries three requests: 10 ms * (3 + 1) = 40 ms against 20 ms * 1.
        for _ in 0..3 {
            reg.conn_inc(&u[0]);
        }
        assert_eq!(p2c_wins(&u, &w, &reg, 1), 200);
    }

    #[test]
    fn p2c_shares_load_between_peers_of_different_speed() {
        // One peer twice as slow as the other. Latency alone sent everything to the fast one; with the
        // in-flight factor the cost 10 * (a + 1) = 20 * (b + 1) settles at about two to one.
        let (u, w) = two(1, 1);
        let reg = registry();
        seed_latency(&reg, &u[0], 10_000.0);
        seed_latency(&reg, &u[1], 20_000.0);
        let held = p2c_held_open(&u, &w, &reg, 100);
        assert_eq!(held.iter().sum::<usize>(), 100);
        assert!(
            (32..=34).contains(&held[1]),
            "the slow peer should carry about a third of 100 held-open requests, got {held:?}"
        );
    }

    #[test]
    fn p2c_shares_load_in_proportion_to_weight() {
        // Same latency, weights 3:1: (a + 1) / 3 = (b + 1) / 1 settles at about three to one.
        let (u, w) = two(3, 1);
        let reg = registry();
        let held = p2c_held_open(&u, &w, &reg, 40);
        assert!(
            (9..=10).contains(&held[1]),
            "the weight-1 peer should carry about a quarter of 40 held-open requests, got {held:?}"
        );
    }

    #[test]
    fn p2c_with_every_weight_one_and_no_load_still_prefers_the_faster_peer() {
        // The old behaviour, kept when nothing is in flight.
        let (u, w) = two(1, 1);
        let reg = registry();
        seed_latency(&reg, &u[0], 80_000.0);
        seed_latency(&reg, &u[1], 20_000.0);
        assert_eq!(p2c_wins(&u, &w, &reg, 1), 200);
    }

    #[test]
    fn least_conn_splits_held_open_requests_by_weight() {
        let (u, w) = two(3, 1);
        let reg = registry();
        let c = counters();
        let picks: Vec<String> = (0..40)
            .map(|_| LEAST_CONN.pick(&u, &w, "r", 0, &c, &reg).unwrap().0)
            .collect();
        let first = picks.iter().filter(|p| **p == u[0]).count();
        assert_eq!((first, picks.len() - first), (30, 10));
    }

    /// Held open with random ties: each round of three picks hits three different peers.
    #[test]
    fn least_conn_without_weights_spreads_idle_peers_evenly() {
        let u = urls(3);
        let reg = registry();
        let c = counters();
        let picks: Vec<String> = (0..6)
            .map(|_| LEAST_CONN.pick(&u, &[], "r", 0, &c, &reg).unwrap().0)
            .collect();
        // Held open, with ties broken at random: each round of three goes to three different peers.
        for round in picks.chunks(3) {
            let mut seen: Vec<&String> = round.iter().collect();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), 3, "a round of three picks hit {round:?}");
        }
    }

    #[test]
    fn p2c_with_weight_one_picks_the_faster_peer() {
        let (u, w) = two(1, 1);
        let reg = registry();
        seed_latency(&reg, &u[0], 80_000.0);
        seed_latency(&reg, &u[1], 20_000.0);
        assert_eq!(
            p2c_wins(&u, &w, &reg, 1),
            200,
            "the faster peer must win every comparison"
        );
        // The same with no weighted list at all.
        assert_eq!(p2c_wins(&u, &[], &reg, 1), 200);
    }

    #[test]
    fn p2c_prefers_the_heavier_peer_when_latencies_are_equal() {
        let (u, w) = two(3, 1);
        let reg = registry();
        seed_latency(&reg, &u[0], 30_000.0);
        seed_latency(&reg, &u[1], 30_000.0);
        assert_eq!(p2c_wins(&u, &w, &reg, 0), 200);
    }

    #[test]
    fn p2c_weight_divides_the_latency_it_is_judged_by() {
        let (u, w) = two(3, 1);
        let reg = registry();
        // Weight 3 and twice the latency: 60/3 = 20 < 30 -> the heavy peer still wins.
        seed_latency(&reg, &u[0], 60_000.0);
        seed_latency(&reg, &u[1], 30_000.0);
        assert_eq!(p2c_wins(&u, &w, &reg, 0), 200);
        // Four times the latency: 120/3 = 40 > 30 -> the light peer wins.
        seed_latency(&reg, &u[0], 120_000.0);
        assert_eq!(p2c_wins(&u, &w, &reg, 1), 200);
    }

    #[test]
    fn p2c_never_prefers_a_zero_weight_peer() {
        let (u, w) = two(0, 1);
        let reg = registry();
        seed_latency(&reg, &u[0], 1.0); // the fastest peer in the pool, but weight 0
        seed_latency(&reg, &u[1], 500_000.0);
        assert_eq!(p2c_wins(&u, &w, &reg, 1), 200);
    }

    #[test]
    fn p2c_with_every_weight_zero_still_picks_a_peer() {
        let (u, w) = two(0, 0);
        let reg = registry();
        let (picked, _) = P2C.pick(&u, &w, "r", 0, &counters(), &reg).unwrap();
        assert!(u.contains(&picked));
    }

    // ── least-bandwidth (#627) ────────────────────────────────────────────────

    /// Give `url` a smoothed rate by feeding its meter `bytes` per 250 ms window for `windows`
    /// windows. The clock is a made-up one that starts far ahead of the real one, so that the real
    /// reads the strategy makes never open a window of their own.
    fn seed_rate(reg: &UpstreamRegistry, url: &str, bytes: u64, windows: u64) {
        let meter = reg.traffic.entry(url.to_owned()).or_default().clone();
        let start = 10_000_000;
        meter.rate(start);
        for step in 1..=windows {
            meter.add(bytes);
            meter.rate(start + step * crate::traffic::WINDOW_MS);
        }
    }

    /// `n` picks, each finishing before the next, so the in-flight counts stay as the caller set them.
    fn bandwidth_picks(
        u: &[String],
        w: &[(String, u32)],
        reg: &UpstreamRegistry,
        n: usize,
    ) -> Vec<usize> {
        let c = counters();
        let mut counts = vec![0; u.len()];
        for _ in 0..n {
            let (url, slot) = LEAST_BANDWIDTH.pick(u, w, "r", 0, &c, reg).unwrap();
            assert!(
                slot,
                "least-bandwidth holds an in-flight slot and must say so"
            );
            reg.conn_dec(&url);
            counts[u.iter().position(|x| *x == url).unwrap()] += 1;
        }
        counts
    }

    #[test]
    fn least_bandwidth_picks_the_quieter_peer() {
        let (u, w) = two(1, 1);
        let reg = registry();
        seed_rate(&reg, &u[0], 1_000_000, 8);
        seed_rate(&reg, &u[1], 1_000, 8);
        assert_eq!(bandwidth_picks(&u, &w, &reg, 50), vec![0, 50]);
        // And the other way round.
        let reg = registry();
        seed_rate(&reg, &u[0], 1_000, 8);
        seed_rate(&reg, &u[1], 1_000_000, 8);
        assert_eq!(bandwidth_picks(&u, &w, &reg, 50), vec![50, 0]);
    }

    #[test]
    fn least_bandwidth_divides_the_cost_by_the_weight() {
        // The cost is the square root of the rate (see the strategy's doc), so a weight-3 peer stays
        // the cheaper one until its rate is nine times the weight-1 peer's.
        let (u, w) = two(3, 1);
        let reg = registry();
        seed_rate(&reg, &u[0], 4_000_000, 8); // 4x: sqrt(4) / 3 = 0.67 < 1
        seed_rate(&reg, &u[1], 1_000_000, 8);
        assert_eq!(bandwidth_picks(&u, &w, &reg, 50), vec![50, 0]);
        let reg = registry();
        seed_rate(&reg, &u[0], 16_000_000, 8); // 16x: sqrt(16) / 3 = 1.33 > 1
        seed_rate(&reg, &u[1], 1_000_000, 8);
        assert_eq!(bandwidth_picks(&u, &w, &reg, 50), vec![0, 50]);
    }

    /// At equal rates the peer with requests in flight is dearer, and a quiet peer with eight in flight loses to one with none.
    #[test]
    fn least_bandwidth_counts_requests_in_flight() {
        let (u, w) = two(1, 1);
        // Equal rates: the peer with requests in flight is the dearer one.
        let reg = registry();
        seed_rate(&reg, &u[0], 1_000_000, 8);
        seed_rate(&reg, &u[1], 1_000_000, 8);
        for _ in 0..3 {
            reg.conn_inc(&u[0]);
        }
        assert_eq!(bandwidth_picks(&u, &w, &reg, 50), vec![0, 50]);
        // A peer with a quarter of the traffic (cost factor 0.5) but eight requests in flight
        // (factor 3) is the dearer one too: a slow peer moves few bytes and looks idle, its
        // piled-up requests give it away.
        let reg = registry();
        seed_rate(&reg, &u[0], 250_000, 8);
        seed_rate(&reg, &u[1], 1_000_000, 8);
        for _ in 0..8 {
            reg.conn_inc(&u[0]);
        }
        assert_eq!(bandwidth_picks(&u, &w, &reg, 50), vec![0, 50]);
    }

    #[test]
    fn least_bandwidth_takes_and_reports_an_in_flight_slot() {
        let (u, w) = two(1, 1);
        let reg = registry();
        let (url, slot) = LEAST_BANDWIDTH
            .pick(&u, &w, "r", 0, &counters(), &reg)
            .unwrap();
        assert!(slot);
        assert_eq!(reg.conn_load(&url), 1);
    }

    /// Nothing recorded and nothing in flight: every cost is 1, a tie, spread at random (about 200 each of 600).
    #[test]
    fn least_bandwidth_spreads_an_idle_pool_of_equal_weights_at_random() {
        let u = urls(3);
        // Nothing recorded and nothing in flight anywhere: every cost is 1, a tie, chosen at random
        // (600 picks: 200 each, standard deviation about 11.5).
        for (i, count) in bandwidth_picks(&u, &[], &registry(), 600)
            .into_iter()
            .enumerate()
        {
            assert!((140..=260).contains(&count), "peer {i} got {count} of 600");
        }
    }

    /// The cost is divided by the weight, so the weight-3 peer is the cheaper one every time until traffic flows.
    #[test]
    fn least_bandwidth_prefers_the_heavier_peer_while_nothing_is_in_flight() {
        // The cost is divided by the weight, so with no traffic and no request in flight the weight-3
        // peer is the cheaper one every time (like `p2c`); the split becomes proportional once traffic
        // flows (the harness of issue #629 measured 58/21/21 for weights 3:1:1).
        let (u, w) = two(3, 1);
        assert_eq!(bandwidth_picks(&u, &w, &registry(), 100), vec![100, 0]);
    }

    /// Requests that never finish: the weight-3 peer takes more than three times the weight-1 peer's share.
    #[test]
    fn least_bandwidth_gives_the_heavier_peer_more_in_flight_requests() {
        // Requests that never finish: the weight-3 peer takes more of them, up to the point where
        // sqrt(in flight + 1) / 3 catches up with the weight-1 peer's sqrt(in flight + 1).
        let (u, w) = two(3, 1);
        let reg = registry();
        let c = counters();
        for _ in 0..100 {
            LEAST_BANDWIDTH.pick(&u, &w, "r", 0, &c, &reg).unwrap();
        }
        let (heavy, light) = (reg.conn_load(&u[0]), reg.conn_load(&u[1]));
        assert_eq!(heavy + light, 100);
        assert!(heavy > light * 3, "heavy {heavy}, light {light}");
    }

    /// Review finding on PR #631: with weights 0, 0, 1 a pair of two sampled peers can be the two
    /// zero-weight ones, and the strategy used to pick one of them.
    #[test]
    fn p2c_and_least_bandwidth_never_pick_a_zero_weight_peer_among_many() {
        let u = urls(3);
        let w = vec![(u[0].clone(), 0), (u[1].clone(), 0), (u[2].clone(), 1)];
        let reg = registry();
        let c = counters();
        for strategy in [&P2C as &dyn LoadBalancingStrategy, &LEAST_BANDWIDTH] {
            for _ in 0..300 {
                let (url, slot) = strategy.pick(&u, &w, "r", 0, &c, &reg).unwrap();
                assert_eq!(url, u[2]);
                if slot {
                    reg.conn_dec(&url);
                }
            }
        }
        // And with two peers of three weighted, every pick is one of those two.
        let w = vec![(u[0].clone(), 0), (u[1].clone(), 2), (u[2].clone(), 1)];
        for strategy in [&P2C as &dyn LoadBalancingStrategy, &LEAST_BANDWIDTH] {
            for _ in 0..300 {
                let (url, slot) = strategy.pick(&u, &w, "r", 0, &c, &reg).unwrap();
                assert_ne!(url, u[0]);
                if slot {
                    reg.conn_dec(&url);
                }
            }
        }
    }

    #[test]
    fn least_bandwidth_never_picks_a_weight_zero_peer_while_another_has_weight() {
        let (u, w) = two(0, 1);
        let reg = registry();
        seed_rate(&reg, &u[1], 5_000_000, 8); // the weight-1 peer is the busy one, and still the only choice
        assert_eq!(bandwidth_picks(&u, &w, &reg, 30), vec![0, 30]);
        // Every weight 0: the weights are ignored and the quieter peer wins.
        let (u, w) = two(0, 0);
        let reg = registry();
        seed_rate(&reg, &u[0], 5_000_000, 8);
        assert_eq!(bandwidth_picks(&u, &w, &reg, 30), vec![0, 30]);
    }

    #[test]
    fn least_bandwidth_with_an_empty_list_picks_nothing() {
        let c = counters();
        assert!(LEAST_BANDWIDTH
            .pick(&[], &[], "r", 0, &c, &registry())
            .is_none());
    }

    #[test]
    fn returns_none_for_empty_url_list() {
        let c = counters();
        let reg = registry();
        let empty: Vec<String> = vec![];
        assert!(ROUND_ROBIN.pick(&empty, &[], "r", 0, &c, &reg).is_none());
        assert!(RANDOM.pick(&empty, &[], "r", 0, &c, &reg).is_none());
        assert!(HASH_BASED.pick(&empty, &[], "r", 0, &c, &reg).is_none());
    }

    // ── gen_pair ──────────────────────────────────────────────────────────────

    #[test]
    fn gen_pair_returns_distinct_indices() {
        // Run many times to cover both branches (b >= a and b < a).
        for _ in 0..100 {
            let (a, b) = gen_pair(4);
            assert!(a < 4, "a must be in range [0, len)");
            assert!(b < 4, "b must be in range [0, len)");
            assert_ne!(a, b, "gen_pair must return distinct indices");
        }
    }

    #[test]
    fn gen_pair_works_with_two_elements() {
        let (a, b) = gen_pair(2);
        // With len=2, only (0,1) or (1,0) is possible.
        assert!((a, b) == (0, 1) || (a, b) == (1, 0));
    }
}
