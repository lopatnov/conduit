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
}

impl TokenBucket {
    pub fn new(limit: u64, window_secs: u64) -> Self {
        let capacity = limit as f64;
        let refill_rate = capacity / window_secs.max(1) as f64;
        Self {
            tokens: capacity,
            capacity,
            refill_rate,
            window_secs: window_secs.max(1),
            last_touched: Instant::now(),
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
            true
        } else {
            false
        }
    }

    /// Returns `true` if this bucket has not been touched for `max_idle_secs`.
    fn is_stale(&self, max_idle_secs: u64) -> bool {
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
pub fn check(cfg: &RateLimitConfig, session: &Session, limiter: &RateLimiter) -> bool {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return true;
    }

    let key = extract_key(cfg, session);
    limiter
        .entry(key)
        .or_insert_with(|| TokenBucket::new(cfg.limit, cfg.window_secs))
        .try_consume()
}

/// Remove stale entries from the rate-limiter map.
///
/// An entry is considered stale when it has not been touched for twice its
/// configured window.  This ensures that even long windows (e.g. `windowSecs:
/// 3600`) retain bucket state long enough for the next request to be correctly
/// rate-limited.  Called every 60 seconds by the background cleanup task.
pub fn cleanup(limiter: &RateLimiter) {
    limiter.retain(|_, bucket| !bucket.is_stale(bucket.window_secs * 2));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_starts_full_and_allows_limit_requests() {
        let mut b = TokenBucket::new(3, 60);
        assert!(b.try_consume(), "1st token");
        assert!(b.try_consume(), "2nd token");
        assert!(b.try_consume(), "3rd token");
        assert!(!b.try_consume(), "4th token must be denied");
    }

    #[test]
    fn bucket_with_zero_window_secs_uses_minimum_one() {
        // window_secs 0 is normalised to 1 internally.
        let mut b = TokenBucket::new(1, 0);
        assert!(b.try_consume());
        assert!(!b.try_consume());
    }

    #[test]
    fn is_stale_with_zero_threshold_is_always_true() {
        let b = TokenBucket::new(10, 60);
        assert!(b.is_stale(0), "elapsed >= 0 is always true");
    }

    #[test]
    fn is_stale_with_huge_threshold_is_false() {
        let b = TokenBucket::new(10, 60);
        assert!(!b.is_stale(u64::MAX));
    }

    #[test]
    fn cleanup_preserves_fresh_bucket() {
        let limiter = RateLimiter::new();
        limiter.insert("key".to_string(), TokenBucket::new(10, 60));
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
    fn multiple_buckets_independent() {
        let limiter = RateLimiter::new();
        limiter.insert("a".to_string(), TokenBucket::new(1, 60));
        limiter.insert("b".to_string(), TokenBucket::new(2, 60));
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
}
