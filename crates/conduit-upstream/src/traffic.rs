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
use std::sync::Mutex;
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
    /// Monotonic milliseconds of the last refresh; 0 until the first reader arrives. Read without the
    /// lock as the cheap "is a refresh due?" test; only written under it.
    last_ms: AtomicU64,
    /// The smoothed rate, `f64::to_bits`, in bytes per second.
    rate_bits: AtomicU64,
    /// `total` at the last refresh, behind the lock that serialises refreshes: a refresh that was
    /// descheduled between reading the total and storing it can no longer overwrite a newer one with an
    /// older snapshot, which would make the next refresh count the same bytes twice (review of PR #631).
    window: Mutex<u64>,
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
        // Nothing due: no lock, one atomic load. This is the path nearly every pick takes.
        let last = self.last_ms.load(Ordering::Relaxed);
        if last != 0 && now_ms.saturating_sub(last) < WINDOW_MS {
            return;
        }
        // One refresh at a time. A reader that does not get the lock never waits: somebody else is
        // refreshing, and it keeps the rate that is there.
        let Ok(mut last_total) = self.window.try_lock() else {
            return;
        };
        let last = self.last_ms.load(Ordering::Relaxed);
        if last == 0 {
            // First reader: open the first window now. `last_total` stays 0, so the bytes recorded
            // before this first read (the meter is created by the first byte, the first read comes
            // with a later pick) are part of the first window instead of being thrown away: an upstream
            // that has just served a large response must not look idle.
            self.last_ms.store(now_ms.max(1), Ordering::Relaxed);
            return;
        }
        let elapsed_ms = now_ms.saturating_sub(last);
        if elapsed_ms < WINDOW_MS {
            return; // another reader refreshed between the check above and the lock
        }
        let total = self.total.load(Ordering::Relaxed);
        let before = std::mem::replace(&mut *last_total, total);
        let secs = elapsed_ms as f64 / 1000.0;
        let instant = total.saturating_sub(before) as f64 / secs;
        let keep = (-secs / DECAY_SECS).exp();
        let old = f64::from_bits(self.rate_bits.load(Ordering::Relaxed));
        self.rate_bits.store(
            (old * keep + instant * (1.0 - keep)).to_bits(),
            Ordering::Relaxed,
        );
        self.last_ms.store(now_ms, Ordering::Relaxed);
    }

    /// `total` at the last refresh (tests only).
    #[cfg(test)]
    fn window_total(&self) -> u64 {
        *self.window.lock().unwrap()
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

    /// Review finding on PR #631: the first read used to open its window at the bytes recorded so far,
    /// so what an upstream had already served was forgotten and it looked idle.
    #[test]
    fn bytes_recorded_before_the_first_read_count() {
        let m = TrafficMeter::default();
        m.add(5_000_000); // a large response served before anyone looked at the rate
        assert_eq!(m.rate(1_000), 0.0, "the first read only opens the window");
        assert!(
            m.rate(1_000 + WINDOW_MS) > 0.0,
            "the early bytes must show up"
        );
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

    /// Review finding on PR #631: refreshes are serialised, so the byte snapshot of the window only ever
    /// moves forward and no byte is counted twice. Four writers and refreshers race on one meter while the
    /// test watches the snapshot; with the lock-free version of this code a refresh that stalled between
    /// reading the total and storing it could set the snapshot back (rare, hence a stress test).
    #[test]
    fn concurrent_refreshes_never_move_the_window_back() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let meter = Arc::new(TrafficMeter::default());
        let clock = Arc::new(AtomicU64::new(1_000));
        meter.rate(1_000);
        let done = Arc::new(AtomicBool::new(false));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let (meter, clock) = (meter.clone(), clock.clone());
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        meter.add(10);
                        meter.rate(clock.fetch_add(WINDOW_MS, Ordering::Relaxed));
                    }
                })
            })
            .collect();
        let watcher = {
            let (meter, done) = (meter.clone(), done.clone());
            std::thread::spawn(move || {
                let mut seen = 0;
                while !done.load(Ordering::Relaxed) {
                    let now = meter.window_total();
                    assert!(
                        now >= seen,
                        "the window snapshot went back from {seen} to {now}"
                    );
                    seen = now;
                }
            })
        };
        workers.into_iter().for_each(|w| w.join().unwrap());
        done.store(true, Ordering::Relaxed);
        watcher.join().unwrap();
        assert_eq!(meter.total(), 4 * 20_000 * 10);
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
