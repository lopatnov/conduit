use std::time::Instant;

use dashmap::DashMap;
use pingora_proxy::Session;

use crate::config::schema::RateLimitConfig;
use crate::filter::auth::is_path_skipped;

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

/// Extract the rate-limit key from the request.
///
/// `keyBy`:
/// - `"ip"` (default) — client IP address
/// - `"header:X-Foo"` — value of the named request header
///
/// Public so the Redis rate limiter can reuse the same key derivation logic.
pub fn extract_client_key(cfg: &RateLimitConfig, session: &Session) -> String {
    extract_key(cfg, session)
}

fn extract_key(cfg: &RateLimitConfig, session: &Session) -> String {
    let key_by = cfg.key_by.as_deref().unwrap_or("ip");

    if let Some(header_name) = key_by.strip_prefix("header:") {
        return session
            .req_header()
            .headers
            .get(header_name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_owned();
    }

    // Default: key by client IP address.
    session
        .client_addr()
        .and_then(|a| a.as_inet())
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Check the token-bucket rate limit for this request.
///
/// Returns `true` if the request is within the limit (allowed to proceed).
/// Maximum number of distinct rate-limit buckets per limiter.
///
/// When the DashMap exceeds this threshold a new key receives its own bucket
/// only if it already exists.  If it doesn't exist we return `false`
/// (treat as rate-limited) to prevent unbounded memory growth.
///
/// An attacker sending millions of unique `X-Custom-Header` values could
/// otherwise exhaust memory by creating millions of token buckets.
const MAX_BUCKETS: usize = 100_000;

pub fn check(cfg: &RateLimitConfig, session: &Session, limiter: &RateLimiter) -> bool {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return true;
    }

    let key = extract_key(cfg, session);

    // Fast path: bucket already exists.
    if let Some(mut bucket) = limiter.get_mut(&key) {
        return bucket.try_consume();
    }

    // Slow path: new key — check capacity before inserting.
    if limiter.len() >= MAX_BUCKETS {
        // Map is full: treat as rate-limited rather than allocating another bucket.
        tracing::warn!(
            key = %key,
            buckets = limiter.len(),
            "rate-limit bucket cap reached — treating new key as rate-limited"
        );
        return false;
    }

    limiter
        .entry(key)
        .or_insert_with(|| TokenBucket::new(cfg.limit, cfg.burst.unwrap_or(0), cfg.window_secs))
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
}
