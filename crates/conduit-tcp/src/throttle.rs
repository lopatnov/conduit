//! A counter that says when a repeated event is worth a log line.
//!
//! Used for the events a client can cause at will (a refused connection, a bad PROXY protocol header): one `warn` per
//! interval and site, carrying the number of events since the last one, so that a flood does not become a flood of log
//! lines (and a scanner cannot fill the disk through the log).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The default interval between two logged lines.
const LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Counts events and says which of them to log.
pub struct Throttle {
    count: AtomicU64,
    /// Milliseconds after `epoch` of the last event that was logged (0 = none yet).
    last_logged_ms: AtomicU64,
    interval: Duration,
    epoch: Instant,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new(LOG_INTERVAL)
    }
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            count: AtomicU64::new(0),
            last_logged_ms: AtomicU64::new(0),
            interval,
            epoch: Instant::now(),
        }
    }

    /// Count an event; `Some(n)` when it is time to log (`n` events since the last logged one).
    pub fn note(&self) -> Option<u64> {
        self.count.fetch_add(1, Ordering::Relaxed);
        let now = millis(self.epoch.elapsed()).saturating_add(1);
        let last = self.last_logged_ms.load(Ordering::Relaxed);
        let due = last == 0 || now.saturating_sub(last) >= millis(self.interval);
        if due
            && self
                .last_logged_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            return Some(self.count.swap(0, Ordering::Relaxed));
        }
        None
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_event_is_logged_then_one_per_interval_with_the_count() {
        let t = Throttle::new(Duration::from_millis(200));
        assert_eq!(t.note(), Some(1));
        for _ in 0..5 {
            assert_eq!(t.note(), None);
        }
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(t.note(), Some(6), "the five held back and this one");
        assert_eq!(t.note(), None);
    }
}
