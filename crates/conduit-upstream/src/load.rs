//! The in-flight counts, the `least-conn` pick and the traffic meters of the [`UpstreamRegistry`].
//!
//! A second `impl UpstreamRegistry` block, split out of `health.rs` (issue #633): the struct and the
//! health state stay there; what the load-aware strategies read and write (`conn_load`/`conn_inc`/
//! `conn_dec`, `pick_least_conn`, `record_traffic`/`traffic_rate`) lives here.

use std::cmp::Ordering as CmpOrdering;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::health::UpstreamRegistry;
use crate::targets;

impl UpstreamRegistry {
    // ── Traffic (least-bandwidth) helpers ─────────────────────────────────────

    /// Record `bytes` moved (request or response body) for `url`.
    pub fn record_traffic(&self, url: &str, bytes: u64) {
        if let Some(meter) = self.traffic.get(url) {
            meter.add(bytes);
            return;
        }
        self.traffic.entry(url.to_owned()).or_default().add(bytes);
    }

    /// The smoothed traffic rate of `url` in bytes per second (0 when nothing was recorded).
    pub fn traffic_rate(&self, url: &str) -> f64 {
        self.traffic_rate_at(url, crate::traffic::monotonic_ms())
    }

    /// [`traffic_rate`](Self::traffic_rate) at a clock reading the caller took once for a whole pick
    /// (`crate::traffic::monotonic_ms()`), so that comparing N candidates reads the clock once, not N times.
    pub fn traffic_rate_at(&self, url: &str, now_ms: u64) -> f64 {
        self.traffic
            .get(url)
            .map_or(0.0, |meter| meter.rate(now_ms))
    }

    // ── Least-conn helpers ────────────────────────────────────────────────────

    /// Return the current inflight count for `url` (0 when unknown).
    pub fn conn_load(&self, url: &str) -> usize {
        self.conn_count
            .get(url)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Increment the inflight count for `url` and return the new value.
    pub fn conn_inc(&self, url: &str) {
        // The key exists after the first request for a URL: look it up without building a `String`
        // (`entry` needs an owned key, which cost an allocation per request).
        if let Some(count) = self.conn_count.get(url) {
            count.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.conn_count
            .entry(url.to_owned())
            .or_insert_with(|| AtomicUsize::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the inflight count for `url`, saturating at 0.
    pub fn conn_dec(&self, url: &str) {
        if let Some(c) = self.conn_count.get(url) {
            // fetch_update lets us implement saturating decrement atomically.
            // It's deprecated in favor of try_update on current stable rustc, but
            // try_update was only stabilized well after this project's declared
            // rust-version (1.89) -- keep the MSRV-compatible API and silence the
            // deprecation warning explicitly rather than breaking older toolchains.
            #[allow(deprecated)]
            let _ = c.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
        }
    }

    /// Pick the URL from `urls` with the lowest inflight count **per unit of weight**
    /// and increment its count (issue #506).
    ///
    /// The comparison is `inflight / weight`, done by cross-multiplication (nginx
    /// `least_conn` does the same) so there is no division and no rounding: peer `a`
    /// beats the best so far when `a.inflight * best.weight < best.inflight * a.weight`.
    /// A weight-3 peer is therefore chosen until it carries three times the
    /// connections of a weight-1 peer. `weighted` is the `(url, weight)` list of the
    /// route; a URL it does not name has weight 1, so with an empty list (or every
    /// weight 1) this is plain least-connections.
    ///
    /// **Ties.** Peers with the same `inflight / weight` (every peer, when traffic is light
    /// enough that nothing stays in flight) are chosen among at random, in proportion to their
    /// weights. Taking the first of them, as this function used to, sent every request of a
    /// quiet pool to its first target. nginx breaks the same ties with weighted round-robin.
    ///
    /// A peer with weight 0 receives nothing while any peer has a positive weight
    /// (the same rule as weighted round-robin); when every candidate has weight 0 the
    /// weights are ignored rather than refusing to route.
    ///
    /// Returns `None` when `urls` is empty.
    pub fn pick_least_conn(&self, urls: &[String], weighted: &[(String, u32)]) -> Option<String> {
        // Pass 1: the lowest inflight/weight, the first peer at it, and how many peers share it.
        let mut best: Option<(usize, usize, u32)> = None; // (index, inflight, weight)
        let mut tied = 0usize;
        for (i, url) in urls.iter().enumerate() {
            let weight = targets::weight_for(weighted, i, url);
            if weight == 0 {
                continue;
            }
            let load = self.conn_load(url);
            match best
                .map(|(_, best_load, best_weight)| ratio_cmp(load, weight, best_load, best_weight))
            {
                None | Some(CmpOrdering::Less) => {
                    best = Some((i, load, weight));
                    tied = 1;
                }
                Some(CmpOrdering::Equal) => tied += 1,
                Some(CmpOrdering::Greater) => {}
            }
        }
        let index = match best {
            // One peer at the best ratio: the common case under load, no second pass.
            Some((first, _, _)) if tied == 1 => first,
            // Several at the best ratio: one at random, in proportion to its weight. The counters
            // can move between the passes, so fall back to the first peer found in pass 1.
            Some((first, best_load, best_weight)) => {
                let at_best = |i: usize, url: &String| {
                    let weight = targets::weight_for(weighted, i, url);
                    (weight != 0
                        && ratio_cmp(self.conn_load(url), weight, best_load, best_weight)
                            == CmpOrdering::Equal)
                        .then_some(u64::from(weight))
                };
                let total: u64 = urls
                    .iter()
                    .enumerate()
                    .filter_map(|(i, url)| at_best(i, url))
                    .sum();
                let mut ticket = targets::fast_random() % total.max(1);
                let mut chosen = first;
                for (i, url) in urls.iter().enumerate() {
                    if let Some(weight) = at_best(i, url) {
                        chosen = i;
                        if ticket < weight {
                            break;
                        }
                        ticket -= weight;
                    }
                }
                chosen
            }
            // Every candidate has weight 0: plain least-connections, ties at random.
            None => {
                let lowest = urls.iter().map(|url| self.conn_load(url)).min()?;
                let tied: Vec<usize> = (0..urls.len())
                    .filter(|&i| self.conn_load(&urls[i]) <= lowest)
                    .collect();
                // `tied` can only be empty if every counter rose since `lowest` was read.
                match tied.len() {
                    0 => 0,
                    n => tied[(targets::fast_random() % n as u64) as usize],
                }
            }
        };
        let chosen = urls[index].clone();
        self.conn_inc(&chosen);
        Some(chosen)
    }
}

/// Compare `load_a / weight_a` with `load_b / weight_b` without dividing: cross-multiplied in `u128`,
/// so there is no rounding and no overflow. Both weights must be non-zero.
fn ratio_cmp(load_a: usize, weight_a: u32, load_b: usize, weight_b: u32) -> CmpOrdering {
    ((load_a as u128) * (weight_b as u128)).cmp(&((load_b as u128) * (weight_a as u128)))
}
