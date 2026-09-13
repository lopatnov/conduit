use std::time::Instant;

use dashmap::DashMap;

/// Token-bucket state for a single client key.
pub struct TokenBucket {
    /// Currently available tokens (may be fractional for smooth refilling).
    tokens: f64,
    /// Maximum tokens the bucket can hold (equals the configured `limit`).
    capacity: f64,
    /// Tokens added per second (`limit / window_secs`).
    refill_rate: f64,
    /// Configured window length in seconds; used by cleanup to set the idle TTL.
    window_secs: u64,
    /// Last time the bucket was refilled or a token was consumed.
    last_touched: Instant,
    /// Number of requests that passed (token was available).
    pub passed: u64,
    /// Number of requests that were rejected (token unavailable).
    pub rejected: u64,
}

impl TokenBucket {
    /// Create a new token bucket.
    ///
    /// * `limit`       — sustained request limit per `window_secs`
    /// * `burst`       — extra capacity above `limit` for short spikes (0 = no burst)
    /// * `window_secs` — refill window in seconds
    ///
    /// The bucket starts full at `limit + burst` tokens and refills at
    /// `limit / window_secs` tokens per second.  This allows a burst of up to
    /// `limit + burst` requests, while sustained throughput remains at `limit`.
    pub fn new(limit: u64, burst: u64, window_secs: u64) -> Self {
        let capacity = (limit + burst) as f64;
        let refill_rate = limit as f64 / window_secs.max(1) as f64;
        Self {
            tokens: capacity,
            capacity,
            refill_rate,
            window_secs: window_secs.max(1),
            last_touched: Instant::now(),
            passed: 0,
            rejected: 0,
        }
    }

    /// Refill tokens based on elapsed time, then try to consume one token.
    ///
    /// Returns `true` when a token was available (request should be allowed).
    pub fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_touched).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_touched = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            self.passed += 1;
            true
        } else {
            self.rejected += 1;
            false
        }
    }

    /// Return the configured window length in seconds.
    pub fn window_secs(&self) -> u64 {
        self.window_secs
    }

    /// Returns `true` if this bucket has not been touched for `max_idle_secs`.
    pub fn is_stale(&self, max_idle_secs: u64) -> bool {
        self.last_touched.elapsed().as_secs() >= max_idle_secs
    }
}

/// The shared rate-limiter map: client-key → token bucket.
pub type RateLimiter = DashMap<String, TokenBucket>;

/// Maximum number of distinct rate-limit buckets per limiter.
///
/// When the map exceeds this threshold a new key is denied a bucket (treated
/// as rate-limited) rather than growing unbounded — an attacker sending
/// millions of unique keys (e.g. via `keyBy: "header:X-Custom"`) could
/// otherwise exhaust memory by creating millions of token buckets.
///
/// This is the single capacity-checked admission point shared by every
/// rate-limit layer (site, route, consumer) — see [`check_key`].
pub const MAX_BUCKETS: usize = 100_000;

/// Check and consume a token for `key` against `limiter`, creating a new
/// bucket on first use (subject to [`MAX_BUCKETS`]).
///
/// Returns `true` when the request is within the limit (allowed to proceed).
///
/// This is the **single admission point** for every rate-limit layer — site,
/// route, consumer, and the Redis fallback path — so the `MAX_BUCKETS` cap
/// applies uniformly instead of being enforced on only some insertion paths
/// (issue #305). Takes the key as a parameter rather than deriving it: key
/// construction stays with the caller that owns the site/route/consumer
/// context (see issues #303/#304).
pub fn check_key(
    limiter: &RateLimiter,
    key: &str,
    limit: u64,
    burst: u64,
    window_secs: u64,
) -> bool {
    // Fast path: bucket already exists.
    if let Some(mut bucket) = limiter.get_mut(key) {
        return bucket.try_consume();
    }

    // Slow path: new key — check capacity before inserting.
    if limiter.len() >= MAX_BUCKETS {
        // Map is full: treat as rate-limited rather than allocating another bucket.
        // Deliberately not logging the raw key: `keyBy: "header:<name>"` can key
        // by an arbitrary request header (e.g. an API key or session token), so
        // logging it verbatim would leak secrets into the log stream. Log the
        // key's length instead — enough to spot an anomalous pattern (e.g. an
        // attacker cycling through many distinct long values) without exposing
        // the value itself (CodeRabbit finding on PR #311's review).
        tracing::warn!(
            key_len = key.len(),
            buckets = limiter.len(),
            "rate-limit bucket cap reached — treating new key as rate-limited"
        );
        return false;
    }

    limiter
        .entry(key.to_owned())
        .or_insert_with(|| TokenBucket::new(limit, burst, window_secs))
        .try_consume()
}

/// Remove stale entries from the rate-limiter map.
///
/// An entry is considered stale when it has not been touched for twice its
/// configured window.  This ensures that even long windows (e.g. `windowSecs:
/// 3600`) retain bucket state long enough for the next request to be correctly
/// rate-limited.  Called every 60 seconds by the background cleanup task.
pub fn cleanup(limiter: &RateLimiter) {
    limiter.retain(|_, bucket| !bucket.is_stale(bucket.window_secs.saturating_mul(2)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_starts_full_and_allows_limit_requests() {
        let mut b = TokenBucket::new(3, 0, 60);
        assert!(b.try_consume(), "1st token");
        assert!(b.try_consume(), "2nd token");
        assert!(b.try_consume(), "3rd token");
        assert!(!b.try_consume(), "4th token must be denied");
    }

    #[test]
    fn bucket_with_zero_window_secs_uses_minimum_one() {
        // window_secs 0 is normalised to 1 internally.
        let mut b = TokenBucket::new(1, 0, 0);
        assert!(b.try_consume());
        assert!(!b.try_consume());
    }

    #[test]
    fn is_stale_with_zero_threshold_is_always_true() {
        let b = TokenBucket::new(10, 0, 60);
        assert!(b.is_stale(0), "elapsed >= 0 is always true");
    }

    #[test]
    fn is_stale_with_huge_threshold_is_false() {
        let b = TokenBucket::new(10, 0, 60);
        assert!(!b.is_stale(u64::MAX));
    }

    #[test]
    fn cleanup_preserves_fresh_bucket() {
        let limiter = RateLimiter::new();
        limiter.insert("key".to_string(), TokenBucket::new(10, 0, 60));
        cleanup(&limiter);
        assert_eq!(limiter.len(), 1, "fresh bucket should survive cleanup");
    }

    #[test]
    fn cleanup_on_empty_limiter_is_noop() {
        let limiter = RateLimiter::new();
        cleanup(&limiter);
        assert_eq!(limiter.len(), 0);
    }

    #[test]
    fn bucket_window_secs_returns_configured_value() {
        let b = TokenBucket::new(100, 0, 120);
        assert_eq!(b.window_secs(), 120);
    }

    #[test]
    fn bucket_window_secs_minimum_one_when_zero_configured() {
        // window_secs 0 is normalised to 1 internally.
        let b = TokenBucket::new(10, 0, 0);
        assert_eq!(b.window_secs(), 1);
    }

    #[test]
    fn multiple_buckets_independent() {
        let limiter = RateLimiter::new();
        limiter.insert("a".to_string(), TokenBucket::new(1, 0, 60));
        limiter.insert("b".to_string(), TokenBucket::new(2, 0, 60));
        {
            let mut a = limiter.get_mut("a").unwrap();
            assert!(a.try_consume());
            assert!(!a.try_consume());
        }
        {
            let mut b = limiter.get_mut("b").unwrap();
            assert!(b.try_consume());
            assert!(b.try_consume());
            assert!(!b.try_consume());
        }
    }

    // ── burst capacity ────────────────────────────────────────────────────────

    #[test]
    fn burst_allows_extra_requests_above_limit() {
        // limit=2, burst=3 → bucket starts with 5 tokens
        let mut b = TokenBucket::new(2, 3, 60);
        for i in 0..5 {
            assert!(b.try_consume(), "token {i} should be available");
        }
        assert!(!b.try_consume(), "6th token must be denied (limit+burst=5)");
    }

    #[test]
    fn zero_burst_behaves_like_classic_token_bucket() {
        let mut b = TokenBucket::new(3, 0, 60);
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(!b.try_consume());
    }

    #[test]
    fn burst_capacity_is_limit_plus_burst() {
        // A bucket with limit=2, burst=3 should allow exactly 5 requests
        // without waiting (capacity = limit + burst = 5).
        let mut b = TokenBucket::new(2, 3, 60);
        let allowed = (0..10).filter(|_| b.try_consume()).count();
        assert_eq!(allowed, 5, "capacity must equal limit + burst");
    }

    // ── check_key / MAX_BUCKETS cap (issue #305) ───────────────────────────────

    #[test]
    fn check_key_allows_within_limit() {
        let limiter = RateLimiter::new();
        assert!(check_key(&limiter, "k", 2, 0, 60));
        assert!(check_key(&limiter, "k", 2, 0, 60));
        assert!(
            !check_key(&limiter, "k", 2, 0, 60),
            "3rd request must be denied"
        );
    }

    #[test]
    fn check_key_reuses_existing_bucket_across_calls() {
        let limiter = RateLimiter::new();
        check_key(&limiter, "k", 5, 0, 60);
        assert_eq!(limiter.len(), 1);
        check_key(&limiter, "k", 5, 0, 60);
        assert_eq!(limiter.len(), 1, "same key must not create a second bucket");
    }

    #[test]
    fn check_key_denies_new_key_past_max_buckets() {
        let limiter = RateLimiter::new();
        for i in 0..MAX_BUCKETS {
            limiter.insert(format!("existing-{i}"), TokenBucket::new(100, 0, 60));
        }
        assert!(
            !check_key(&limiter, "one-too-many", 100, 0, 60),
            "a brand-new key past the cap must be denied, not allocate another bucket"
        );
        assert_eq!(limiter.len(), MAX_BUCKETS, "the cap must not be exceeded");
    }

    #[test]
    fn check_key_still_serves_existing_key_past_max_buckets() {
        let limiter = RateLimiter::new();
        limiter.insert("hot-key".to_string(), TokenBucket::new(2, 0, 60));
        for i in 0..MAX_BUCKETS {
            limiter.insert(format!("existing-{i}"), TokenBucket::new(100, 0, 60));
        }
        assert!(
            check_key(&limiter, "hot-key", 2, 0, 60),
            "an already-existing key must still be served even when the map is at capacity"
        );
    }
}
