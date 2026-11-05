//! Per-upstream traffic meter for the `least-bandwidth` strategy (issue #627).
//!
//! The proxy adds the size of every request-body and response-body chunk it moves for an upstream
//! ([`TrafficMeter::add`], one relaxed atomic add); the strategy reads a smoothed rate in bytes per
//! second ([`TrafficMeter::rate`]). There is no timer thread: the rate is refreshed lazily by whichever
//! reader notices that the last refresh is older than [`WINDOW_MS`], and only one reader wins that
//! refresh (a compare-and-swap on the timestamp), so a pick never waits on a lock.
//!
//! The smoothing is an exponentially decayed average with a time constant of [`DECAY_SECS`]
//! (`rate = rate * e^(-dt/tau) + instant * (1 - e^(-dt/tau))`), so a burst is remembered for some
//! seconds and an idle upstream's rate falls towards zero. Citrix ADC's `leastbandwidth` sums the bytes
//! of the last 14 seconds; this is the same idea with a smooth edge.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A rate is recomputed at most this often (milliseconds).
pub const WINDOW_MS: u64 = 250;
/// Time constant of the decayed average (seconds).
pub const DECAY_SECS: f64 = 10.0;

/// Bytes moved for one upstream and their smoothed rate.
#[derive(Debug, Default)]
pub struct TrafficMeter {
    /// Bytes ever recorded.
    total: AtomicU64,
    /// `total` at the last refresh.
    last_total: AtomicU64,
    /// Monotonic milliseconds of the last refresh; 0 until the first reader arrives.
    last_ms: AtomicU64,
    /// The smoothed rate, `f64::to_bits`, in bytes per second.
    rate_bits: AtomicU64,
}

impl TrafficMeter {
    /// Record `bytes` moved for this upstream (either direction).
    pub fn add(&self, bytes: u64) {
        self.total.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Bytes ever recorded.
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// The smoothed rate in bytes per second as of `now_ms` (monotonic milliseconds, never 0).
    pub fn rate(&self, now_ms: u64) -> f64 {
        self.refresh(now_ms);
        f64::from_bits(self.rate_bits.load(Ordering::Relaxed))
    }

    fn refresh(&self, now_ms: u64) {
        let last = self.last_ms.load(Ordering::Relaxed);
        if last == 0 {
            // First reader: open the first window at the bytes recorded so far.
            let now = now_ms.max(1);
            if self
                .last_ms
                .compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.last_total
                    .store(self.total.load(Ordering::Relaxed), Ordering::Relaxed);
            }
            return;
        }
        let elapsed_ms = now_ms.saturating_sub(last);
        if elapsed_ms < WINDOW_MS {
            return;
        }
        // One reader refreshes; the others keep the rate they have.
        if self
            .last_ms
            .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let total = self.total.load(Ordering::Relaxed);
        let before = self.last_total.swap(total, Ordering::Relaxed);
        let secs = elapsed_ms as f64 / 1000.0;
        let instant = total.saturating_sub(before) as f64 / secs;
        let keep = (-secs / DECAY_SECS).exp();
        let old = f64::from_bits(self.rate_bits.load(Ordering::Relaxed));
        self.rate_bits.store(
            (old * keep + instant * (1.0 - keep)).to_bits(),
            Ordering::Relaxed,
        );
    }
}

/// Monotonic milliseconds since the first call in this process, plus one (so it is never 0).
pub fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meter_nobody_wrote_to_has_rate_zero() {
        let m = TrafficMeter::default();
        assert_eq!(m.rate(1_000), 0.0);
        assert_eq!(m.rate(5_000), 0.0);
    }

    #[test]
    fn the_rate_follows_the_bytes_and_is_the_same_for_the_same_traffic() {
        let (a, b) = (TrafficMeter::default(), TrafficMeter::default());
        a.rate(1_000);
        b.rate(1_000); // open the windows
        for step in 1..=20u64 {
            a.add(1_000_000); // 1 MB per window
            b.add(1_000); //     1 KB per window
            a.rate(1_000 + step * WINDOW_MS);
            b.rate(1_000 + step * WINDOW_MS);
        }
        let now = 1_000 + 20 * WINDOW_MS;
        let (ra, rb) = (a.rate(now), b.rate(now));
        assert!(ra > 0.0 && rb > 0.0);
        assert!(
            (ra / rb - 1000.0).abs() < 1.0,
            "1000x the bytes must be 1000x the rate: {ra} vs {rb}"
        );
        // After 5 s of 4 MB/s the decayed average has climbed to about 1 - e^(-5/10) = 39% of it.
        assert!((1_400_000.0..1_700_000.0).contains(&ra), "rate {ra}");
    }

    #[test]
    fn an_idle_meter_forgets() {
        let m = TrafficMeter::default();
        m.rate(1_000);
        for step in 1..=40u64 {
            m.add(1_000_000);
            m.rate(1_000 + step * WINDOW_MS);
        }
        let busy = m.rate(1_000 + 40 * WINDOW_MS);
        // A minute of silence: the next reader folds it into one window of zero traffic.
        let later = m.rate(1_000 + 40 * WINDOW_MS + 60_000);
        assert!(busy > 1_000_000.0, "{busy}");
        assert!(later < busy * 0.01, "idle for a minute: {later} vs {busy}");
    }

    #[test]
    fn a_reader_inside_the_window_changes_nothing() {
        let m = TrafficMeter::default();
        m.rate(1_000);
        m.add(500_000);
        m.rate(1_100); // 100 ms: window not over
        assert_eq!(m.rate(1_100), 0.0);
        assert!(m.rate(1_000 + WINDOW_MS) > 0.0);
    }

    #[test]
    fn totals_add_up_from_many_threads() {
        let m = std::sync::Arc::new(TrafficMeter::default());
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let m = m.clone();
                std::thread::spawn(move || (0..10_000).for_each(|_| m.add(3)))
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(m.total(), 4 * 10_000 * 3);
    }

    #[test]
    fn monotonic_ms_never_returns_zero_and_does_not_go_back() {
        let a = monotonic_ms();
        let b = monotonic_ms();
        assert!(a >= 1 && b >= a);
    }
}
