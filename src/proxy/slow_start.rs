//! Slow-start traffic ramp-up (`healthCheck.slowStartSecs`, issue #157).
//!
//! Companion to [`crate::proxy::capacity`]: both are per-upstream, per-request
//! admission constraints, evaluated once per routing decision and applied
//! inside [`crate::proxy::capacity::pick_bounded`] (plus the two retry-bypass
//! call sites in `router.rs`/`routes.rs` that skip `pick_bounded` entirely),
//! so that no [`crate::proxy::strategy::LoadBalancingStrategy`] implementation
//! and no `router.rs`/`routes.rs` call site has to know this exists
//! (CLAUDE.md decision #22).
//!
//! Where capacity is a HARD filter (a peer over its cap must never be picked;
//! all peers over cap = 503), slow-start is a SOFT, probabilistic
//! de-prioritisation: a peer that recovered `t` seconds into a `w`-second
//! window (see [`crate::proxy::health::slow_start_fraction`]) participates in
//! each pick with probability `t/w`. It can never produce a 503 and can never
//! empty an otherwise-routable candidate list (fail-open — see
//! [`Ramp::filter_candidates`]).
//!
//! **Hash-based strategies (`ip-hash`/`consistent-hash`) and sticky sessions
//! are deliberately exempt.** A client's hash deterministically maps to one
//! peer for consistency; forcibly diverting some hashed clients elsewhere
//! during a ramp window breaks the exact guarantee those strategies exist to
//! provide, and outright breaks sticky sessions (a client would flap between
//! peers for the entire ramp). The exemption needs zero code in this module:
//! `capacity.rs::pick_bounded` already early-returns for hash strategies
//! *before* it would apply a [`Ramp`], and sticky routes always resolve to
//! `ConsistentHash` (see `router.rs::effective_strategy`), so they take the
//! same early return. Callers simply never call [`Ramp::filter_candidates`]/
//! [`Ramp::filter_weighted`] on that path.
//!
//! A prior investigation into whether Pingora's own `pingora-load-balancing`
//! crate already provides something like this (or should replace conduit's
//! strategy layer outright) found: no slow-start concept exists there at all,
//! and its own weighted round-robin has the identical contiguous-burst
//! property that ruled out weight-scaling as *this* module's mechanism (see
//! the PR description for #157 for the full comparison) — informing, not
//! changing, the design below.

use std::borrow::Cow;

use crate::proxy::health::{self, UpstreamRegistry};
use crate::proxy::upstream;

/// Fixed-point denominator for the admission draw (0.01% granularity).
const RAMP_DENOM: u64 = 10_000;

/// Slow-start admission gate for one routing decision.
///
/// Cheap to construct and copy-free to hold: `window_secs: None` (or
/// `Some(0)`, matching [`health::slow_start_fraction`]'s own no-op case) makes
/// every method here an identity operation — true backward compatibility with
/// pre-#157 behavior, allocation-free and clock-free on that path.
pub(crate) struct Ramp<'a> {
    window_secs: Option<u64>,
    health: &'a UpstreamRegistry,
    /// Fixed for the lifetime of this `Ramp` (== one routing decision).
    /// Admission is a pure function of `(seed, url)` — see [`roll`] — rather
    /// than a stream advanced per call, specifically so that
    /// [`Ramp::filter_candidates`] and [`Ramp::filter_weighted`] agree on the
    /// same URL within one request: `pick_bounded` filters both the plain
    /// candidate list and the `(url, weight)` list (`WeightedRoundRobin`
    /// reads the latter, not the former), and a URL appearing in both must
    /// get the same admit/reject decision, not two independent draws.
    seed: u64,
}

impl<'a> Ramp<'a> {
    /// A `Ramp` that admits everything. Test-only convenience — every
    /// production call site has a real (possibly `None`) `slow_start_secs`
    /// value on hand and calls [`Ramp::new`] directly instead.
    #[cfg(test)]
    pub(crate) fn disabled(health: &'a UpstreamRegistry) -> Self {
        Self::new(None, health)
    }

    /// `window_secs` is `healthCheck.slowStartSecs` as configured (already
    /// resolved to the effective per-route value by the caller). `Some(0)` is
    /// treated as disabled, matching `slow_start_fraction`'s own
    /// `window_secs == 0 => 1.0` rule.
    pub(crate) fn new(window_secs: Option<u64>, health: &'a UpstreamRegistry) -> Self {
        Self::with_seed(window_secs, health, request_seed())
    }

    fn with_seed(window_secs: Option<u64>, health: &'a UpstreamRegistry, seed: u64) -> Self {
        Self {
            window_secs,
            health,
            seed,
        }
    }

    fn window(&self) -> Option<u64> {
        self.window_secs.filter(|w| *w > 0)
    }

    /// `true` when `url` participates in this pick. Always `true` for a peer
    /// that isn't ramping, has no recorded health state, or when slow-start
    /// is disabled entirely.
    fn admits(&self, url: &str) -> bool {
        let Some(window) = self.window() else {
            return true;
        };
        let Some(entry) = self.health.statuses.get(url) else {
            return true; // no recorded state -> slow_start_fraction's own "fully ramped" default
        };
        let fraction = health::slow_start_fraction(&entry, window);
        if fraction >= 1.0 {
            return true;
        }
        let threshold = (fraction * RAMP_DENOM as f64) as u64;
        roll(self.seed, url) % RAMP_DENOM < threshold
    }

    /// Apply the gate to a plain URL candidate list.
    ///
    /// Fails open unconditionally: if every candidate is excluded by the
    /// draw, returns `candidates` unchanged rather than emptying it —
    /// slow-start must never turn a routable request into a 503. Borrows
    /// (allocates nothing) when disabled or when nothing was actually
    /// excluded, which covers the overwhelmingly common steady-state case.
    pub(crate) fn filter_candidates<'c>(&self, candidates: &'c [String]) -> Cow<'c, [String]> {
        if self.window().is_none() {
            return Cow::Borrowed(candidates);
        }
        let kept: Vec<String> = candidates
            .iter()
            .filter(|u| self.admits(u))
            .cloned()
            .collect();
        if kept.is_empty() || kept.len() == candidates.len() {
            Cow::Borrowed(candidates)
        } else {
            Cow::Owned(kept)
        }
    }

    /// Same gate, applied to `(url, weight)` pairs — the list
    /// `WeightedRoundRobin` actually reads (not the plain candidate list).
    /// Without this, a route using `strategy: weighted-round-robin` would be
    /// the one strategy slow-start silently failed to cover, reproducing
    /// issue #156's own "only one strategy respects the constraint" bug
    /// class this module exists to avoid repeating.
    ///
    /// Uses the same per-`(seed, url)` draw as [`Self::filter_candidates`], so
    /// a URL present in both lists gets one consistent decision per request.
    pub(crate) fn filter_weighted<'w>(
        &self,
        weighted: &'w [(String, u32)],
    ) -> Cow<'w, [(String, u32)]> {
        if self.window().is_none() {
            return Cow::Borrowed(weighted);
        }
        let kept: Vec<(String, u32)> = weighted
            .iter()
            .filter(|(u, _)| self.admits(u))
            .cloned()
            .collect();
        if kept.is_empty() || kept.len() == weighted.len() {
            Cow::Borrowed(weighted)
        } else {
            Cow::Owned(kept)
        }
    }
}

/// One clock-derived seed per `Ramp` (== per routing decision). Same source
/// `strategy::gen_pair`/`upstream::pick_random` already use for the same
/// purpose elsewhere in this module tree.
fn request_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
}

/// Deterministic draw in `[0, u64::MAX]` for `(seed, url)`, via a splitmix64
/// avalanche mix (same constants already used by `strategy::gen_pair`) over
/// `seed` folded with the URL's own FNV-1a hash. Keyed on the URL rather than
/// list position so the same peer gets the same draw in `filter_candidates`
/// and `filter_weighted` even though the two lists can differ in length and
/// order.
fn roll(seed: u64, url: &str) -> u64 {
    let mut z = seed ^ upstream::fnv1a_hash(url);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("http://u{i}:80")).collect()
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Mark `url` as having recovered `secs_ago` seconds ago.
    fn set_recovered(reg: &UpstreamRegistry, url: &str, secs_ago: u64) {
        reg.statuses
            .entry(url.to_owned())
            .or_default()
            .recovery_time_secs = Some(now_secs().saturating_sub(secs_ago));
    }

    // ── disabled / no-op cases ───────────────────────────────────────────────

    #[test]
    fn disabled_borrows_candidates_unchanged() {
        let reg = UpstreamRegistry::new();
        let ramp = Ramp::disabled(&reg);
        let list = urls(3);
        assert!(matches!(ramp.filter_candidates(&list), Cow::Borrowed(_)));
    }

    #[test]
    fn window_none_is_a_true_noop_even_with_a_ramping_peer() {
        let reg = UpstreamRegistry::new();
        let list = urls(2);
        set_recovered(&reg, &list[0], 0); // fraction 0.0, would be excluded if enabled
        let ramp = Ramp::with_seed(None, &reg, 42);
        assert!(matches!(ramp.filter_candidates(&list), Cow::Borrowed(_)));
    }

    #[test]
    fn window_zero_is_disabled() {
        let reg = UpstreamRegistry::new();
        let list = urls(2);
        set_recovered(&reg, &list[0], 0);
        let ramp = Ramp::with_seed(Some(0), &reg, 42);
        assert!(matches!(ramp.filter_candidates(&list), Cow::Borrowed(_)));
    }

    // ── admission ────────────────────────────────────────────────────────────

    #[test]
    fn non_ramping_peer_is_always_admitted() {
        let reg = UpstreamRegistry::new();
        // No recorded recovery -> slow_start_fraction's own "fully ramped" default.
        for seed in 0..500u64 {
            let ramp = Ramp::with_seed(Some(30), &reg, seed);
            assert!(ramp.admits("http://never-ejected:80"));
        }
    }

    #[test]
    fn fraction_zero_peer_is_excluded_across_many_seeds() {
        let reg = UpstreamRegistry::new();
        set_recovered(&reg, "http://just-recovered:80", 0); // fraction 0.0
        for seed in 0..1000u64 {
            let ramp = Ramp::with_seed(Some(30), &reg, seed);
            assert!(
                !ramp.admits("http://just-recovered:80"),
                "fraction 0.0 must never admit (seed={seed})"
            );
        }
    }

    #[test]
    fn fraction_half_admits_roughly_half_across_many_urls() {
        let reg = UpstreamRegistry::new();
        let window = 30u64;
        let urls: Vec<String> = (0..1000).map(|i| format!("http://ramp-{i}:80")).collect();
        for u in &urls {
            set_recovered(&reg, u, window / 2); // fraction ~= 0.5
        }
        let ramp = Ramp::with_seed(Some(window), &reg, 0xC0FFEE);
        let admitted = urls.iter().filter(|u| ramp.admits(u)).count();
        assert!(
            (350..=650).contains(&admitted),
            "expected roughly half of 1000 urls admitted at fraction ~0.5, got {admitted}"
        );
    }

    // ── fail-open ────────────────────────────────────────────────────────────

    #[test]
    fn filter_candidates_fails_open_for_a_single_ramping_peer() {
        let reg = UpstreamRegistry::new();
        let list = urls(1);
        set_recovered(&reg, &list[0], 0); // fraction 0.0, only candidate
        let ramp = Ramp::with_seed(Some(30), &reg, 1);
        assert_eq!(ramp.filter_candidates(&list).as_ref(), list.as_slice());
    }

    #[test]
    fn filter_candidates_fails_open_when_every_candidate_is_ramping() {
        let reg = UpstreamRegistry::new();
        let list = urls(4);
        for u in &list {
            set_recovered(&reg, u, 0); // all fraction 0.0
        }
        let ramp = Ramp::with_seed(Some(30), &reg, 2);
        assert_eq!(ramp.filter_candidates(&list).as_ref(), list.as_slice());
    }

    #[test]
    fn filter_candidates_excludes_the_ramping_peer_when_a_sibling_is_healthy() {
        let reg = UpstreamRegistry::new();
        let list = urls(2);
        set_recovered(&reg, &list[0], 0); // fraction 0.0
                                          // list[1] has no recorded state -> fully ramped, always admitted.
        let ramp = Ramp::with_seed(Some(30), &reg, 3);
        let filtered = ramp.filter_candidates(&list);
        assert_eq!(filtered.as_ref(), &[list[1].clone()]);
    }

    // ── consistency between filter_candidates and filter_weighted ───────────

    #[test]
    fn filter_weighted_agrees_with_filter_candidates_for_the_same_url() {
        let reg = UpstreamRegistry::new();
        let list = urls(3);
        set_recovered(&reg, &list[0], 0); // fraction 0.0
        let weighted: Vec<(String, u32)> = list.iter().map(|u| (u.clone(), 5u32)).collect();
        let ramp = Ramp::with_seed(Some(30), &reg, 4);

        let filtered_candidates = ramp.filter_candidates(&list);
        let filtered_weighted = ramp.filter_weighted(&weighted);

        let candidate_urls: Vec<&String> = filtered_candidates.iter().collect();
        let weighted_urls: Vec<&String> = filtered_weighted.iter().map(|(u, _)| u).collect();
        assert_eq!(
            candidate_urls, weighted_urls,
            "the same URL must get the same admit/reject decision in both lists"
        );
        assert!(
            !candidate_urls.contains(&&list[0]),
            "the ramping peer must be excluded from both"
        );
    }

    #[test]
    fn filter_weighted_disabled_borrows_unchanged() {
        let reg = UpstreamRegistry::new();
        let ramp = Ramp::disabled(&reg);
        let weighted = vec![("http://a:80".to_owned(), 1u32)];
        assert!(matches!(ramp.filter_weighted(&weighted), Cow::Borrowed(_)));
    }

    // ── roll ─────────────────────────────────────────────────────────────────

    #[test]
    fn roll_is_stable_for_same_seed_and_url() {
        assert_eq!(roll(1, "http://a:80"), roll(1, "http://a:80"));
    }

    #[test]
    fn roll_differs_across_urls_for_the_same_seed() {
        assert_ne!(roll(1, "http://a:80"), roll(1, "http://b:80"));
    }
}
