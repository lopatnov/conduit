//! Load-balancing strategy trait and concrete implementations.
//!
//! Each strategy is a zero-sized struct that implements [`LoadBalancingStrategy`].
//! Adding a new strategy:
//!
//! 1. Create a struct (e.g. `pub struct CanaryStrategy;`).
//! 2. `impl LoadBalancingStrategy for CanaryStrategy { ... }`.
//! 3. Add a variant to [`crate::config::schema::LoadBalanceStrategy`].
//! 4. Map it in [`from_config`].
//!
//! No changes to `router.rs` are required.

use std::sync::atomic::AtomicUsize;

use dashmap::DashMap;

use crate::config::schema::LoadBalanceStrategy;
use crate::proxy::health::UpstreamRegistry;
use crate::proxy::upstream;

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
    /// - `weighted`  — `(url, weight)` pairs; used by [`WeightedRoundRobin`] only
    /// - `route_key` — stable key for per-route counters (round-robin cursor, etc.)
    /// - `hash_val`  — FNV-1a hash of the relevant input; used by hash strategies
    /// - `counters`  — shared per-route atomic counters for rotation
    /// - `health`    — upstream registry; used by [`LeastConn`]
    ///
    /// # Returns
    /// `Some((url, is_least_conn))` where `is_least_conn = true` means the
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
        upstream::pick_round_robin(urls, route_key, counters).map(|u| (u, false))
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
        upstream::pick_random(urls, route_key, counters).map(|u| (u, false))
    }
}

/// Send each request to the backend with the fewest active connections.
pub struct LeastConn;

impl LoadBalancingStrategy for LeastConn {
    fn pick(
        &self,
        urls: &[String],
        _weighted: &[(String, u32)],
        _route_key: &str,
        _hash_val: u64,
        _counters: &DashMap<String, AtomicUsize>,
        health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        // pick_least_conn increments the inflight counter on the chosen URL.
        health.pick_least_conn(urls).map(|u| (u, true))
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
        upstream::pick_weighted_round_robin(weighted, route_key, counters).map(|u| (u, false))
    }
}

/// Map each client (or URL) to a consistent backend via a hash.
///
/// Used for both `ip-hash` and `consistent-hash` — both reduce to a modulo
/// hash over the candidate list.
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
        upstream::pick_by_hash(urls, hash_val).map(|u| (u, false))
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
        upstream::pick_least_response_time(urls, health, route_key, counters).map(|u| (u, false))
    }
}

/// Power of Two Choices (P2C): sample 2 random backends and forward to the
/// less-loaded one.
///
/// Achieves O(log log n) maximum load with O(1) selection — significantly
/// better tail latency than random and competitive with `least-conn` at scale.
/// Reference: linkerd2-proxy `linkerd/pool/p2c/src/lib.rs`.
pub struct P2cChoice;

impl LoadBalancingStrategy for P2cChoice {
    fn pick(
        &self,
        urls: &[String],
        _weighted: &[(String, u32)],
        route_key: &str,
        _hash_val: u64,
        counters: &DashMap<String, AtomicUsize>,
        health: &UpstreamRegistry,
    ) -> Option<(String, bool)> {
        match urls.len() {
            0 => None,
            1 => {
                // Trivial case: only one choice.
                upstream::pick_round_robin(urls, route_key, counters).map(|u| (u, false))
            }
            len => {
                // Sample two distinct random indices.
                let (a, b) = gen_pair(len);
                let load_a = health.conn_load(&urls[a]);
                let load_b = health.conn_load(&urls[b]);
                // Pick the less loaded; break ties in favour of `a`.
                let chosen = if load_a <= load_b { &urls[a] } else { &urls[b] };
                Some((chosen.clone(), false))
            }
        }
    }
}

/// Generate two distinct random indices in `[0, len)` using a fast XorShift.
fn gen_pair(len: usize) -> (usize, usize) {
    // Use thread-local fast RNG rather than an allocating SmallRng.
    use std::time::{SystemTime, UNIX_EPOCH};
    // Simple splitmix64 seeded from time — good enough for load balancing.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let mut x = seed
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(0x6c62272e07bb0142);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;

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
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::health::UpstreamRegistry;

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
    fn least_conn_returns_is_least_conn_true() {
        let u = urls(2);
        let reg = registry();
        let (_, is_lc) = LEAST_CONN.pick(&u, &[], "r", 0, &counters(), &reg).unwrap();
        assert!(is_lc, "LeastConn must signal is_least_conn = true");
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
        ] {
            let strategy = from_config(s);
            let u = urls(2);
            // Just check it doesn't panic with a minimal call.
            let weighted = vec![("http://b0:4000".to_owned(), 1u32)];
            let _ = strategy.pick(&u, &weighted, "key", 0, &counters(), &registry());
        }
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
}
