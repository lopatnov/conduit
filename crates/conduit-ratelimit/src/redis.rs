#![cfg(feature = "redis")]
//! Redis-backed rate limiting with graceful fallback to in-process memory.
//!
//! Both `redis://` (plaintext) and `rediss://` (TLS) URLs are supported.
//! Use `rediss://` for cloud-hosted Redis that requires in-transit encryption
//! (AWS ElastiCache TLS, Azure Cache for Redis, Upstash, etc.).
//!
//! When Redis is unavailable (connection error, timeout), each check falls
//! through to the same `DashMap<String, TokenBucket>` used by the pure-memory
//! rate limiter.  This keeps the server operational even when Redis is down
//! (fail-open behaviour — requests are rate-limited in memory, not rejected).
//!
//! # Redis data model
//!
//! Each `(site, client)` pair maps to a Redis string that counts requests in
//! the current window.  Two commands implement the counter atomically from
//! the caller's perspective:
//!
//! ```text
//! INCR   conduit:rl:{site_label}:{window_secs}:{client_key}
//! EXPIRE conduit:rl:{site_label}:{window_secs}:{client_key}  {window_secs}
//! ```
//!
//! `site_label` scopes the key so two sites sharing a client key don't share
//! a counter — the Redis-backend twin of the fix `rate_limit::site_key`/
//! `route_key` applied to the in-memory limiter (issue #317, mirroring
//! #303/#304).

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use redis::aio::ConnectionManager;

use crate::bucket::{check_key, TokenBucket};

// ── RedisRateLimiter ──────────────────────────────────────────────────────────

/// Redis-backed rate limiter with in-memory fallback.
///
/// `ConnectionManager` is `Clone` (cheap, same underlying connection) and
/// handles transparent reconnection on failure.
pub struct RedisRateLimiter {
    conn: ConnectionManager,
    /// In-memory fallback used when Redis is unreachable.
    fallback: Arc<DashMap<String, TokenBucket>>,
}

impl RedisRateLimiter {
    /// Connect to the Redis server at `url` and return a `RedisRateLimiter`.
    ///
    /// Returns an error when the initial connection fails (e.g. Redis is not
    /// running).  At that point the caller can fall back to a pure-memory
    /// implementation and log the failure.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client =
            redis::Client::open(url).map_err(|e| anyhow::anyhow!("invalid Redis URL: {e}"))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| anyhow::anyhow!("cannot connect to Redis ({url}): {e}"))?;
        Ok(Self {
            conn,
            fallback: Arc::new(DashMap::new()),
        })
    }

    /// Check the rate limit for `client_key` on the site identified by
    /// `site_label`.
    ///
    /// Returns `true` when the request is within the limit.
    ///
    /// Algorithm (two-command fixed-window counter):
    /// 1. `INCR key` — atomically create-or-increment; returns the new count.
    /// 2. `EXPIRE key window_secs` — set TTL only on the first request of the
    ///    window (count == 1).
    ///
    /// `burst` raises the window's admission ceiling from `limit` to
    /// `limit + burst` (issue #306) — the natural fixed-window equivalent of
    /// the in-memory token bucket's burst capacity: extra requests are
    /// allowed within the *same* window, rather than a continuously
    /// replenishing allowance like a real token bucket. `burst = 0` (the
    /// default) reproduces the exact pre-#306 behavior.
    ///
    /// On Redis error or timeout the check falls back to the in-process
    /// `TokenBucket` and a `WARN` trace is emitted (fail-open).
    pub async fn check(
        &self,
        site_label: &str,
        client_key: &str,
        limit: u64,
        burst: u64,
        window_secs: u64,
    ) -> bool {
        let redis_key = format!("conduit:rl:{site_label}:{window_secs}:{client_key}");
        let mut conn = self.conn.clone();

        // Wrap the two-command sequence in a 50 ms deadline.
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            redis_fixed_window_check(&mut conn, &redis_key, limit, burst, window_secs),
        )
        .await;

        match result {
            Ok(Ok(allowed)) => allowed,
            Ok(Err(e)) => {
                tracing::warn!(
                    site = site_label,
                    key_len = client_key.len(),
                    "Redis rate-limit error (memory fallback): {e}"
                );
                self.fallback_check(site_label, client_key, limit, burst, window_secs)
            }
            Err(_timeout) => {
                tracing::warn!(
                    site = site_label,
                    key_len = client_key.len(),
                    "Redis rate-limit timed out after 50 ms (memory fallback)"
                );
                self.fallback_check(site_label, client_key, limit, burst, window_secs)
            }
        }
    }

    fn fallback_check(
        &self,
        site_label: &str,
        client_key: &str,
        limit: u64,
        burst: u64,
        window_secs: u64,
    ) -> bool {
        fallback_check_impl(
            &self.fallback,
            site_label,
            client_key,
            limit,
            burst,
            window_secs,
        )
    }

    /// Evict stale entries from the in-memory fallback map.
    ///
    /// Called by the same background cleanup task as the main memory limiter.
    pub fn cleanup_fallback(&self) {
        self.fallback
            .retain(|_, bucket| !bucket.is_stale(bucket.window_secs().saturating_mul(2)));
    }
}

/// The actual fallback-map admission logic, factored out of
/// [`RedisRateLimiter::fallback_check`] as a free function so it's testable
/// without a live Redis connection (`RedisRateLimiter::connect` requires
/// one; a plain `DashMap` doesn't).
fn fallback_check_impl(
    fallback: &DashMap<String, TokenBucket>,
    site_label: &str,
    client_key: &str,
    limit: u64,
    burst: u64,
    window_secs: u64,
) -> bool {
    // Include limit, burst, and window_secs in the key so that post-reload
    // config changes are picked up immediately rather than reusing a stale
    // bucket. Include site_label so two sites sharing a client key don't
    // share a bucket here either (issue #317).
    let key = format!("{site_label}:{client_key}:{limit}:{burst}:{window_secs}");
    // Routed through the shared MAX_BUCKETS-capped admission point (issue
    // #305's fallback-path counterpart) instead of an uncapped
    // entry()/or_insert_with() — this map has no cap check of its own.
    // `burst` now flows through for real (issue #306) — this fallback is a
    // genuine in-memory TokenBucket, so it supports burst the same way the
    // primary in-memory limiter always has; only the real-Redis fixed-window
    // path needed the `limit + burst` ceiling trick above.
    check_key(fallback, &key, limit, burst, window_secs)
}

// ── Redis helper ──────────────────────────────────────────────────────────────

/// Fixed-window counter check using two Redis commands.
///
/// Steps:
/// 1. `INCR key` — atomically create-or-increment; returns the new count.
/// 2. `EXPIRE key window_secs` — set TTL only when count == 1 (first request in window).
///
/// If `count > limit + burst`, the request is rate-limited (issue #306 —
/// `burst = 0` reproduces the original `count > limit` behavior exactly).
///
/// Using INCR-first prevents the TTL-leak race present in the former
/// SET-NX + INCR approach: if the key expired between the SET-NX and the
/// INCR, the INCR would recreate the key *without* a TTL, causing the
/// counter to persist forever.  With INCR-first the TTL is set exactly
/// once — when the key is first created — and subsequent increments leave
/// the existing TTL unchanged.
async fn redis_fixed_window_check(
    conn: &mut ConnectionManager,
    redis_key: &str,
    limit: u64,
    burst: u64,
    window_secs: u64,
) -> Result<bool, redis::RedisError> {
    // INCR key — atomically creates (at 0) then increments; returns new value.
    let count: u64 = redis::cmd("INCR").arg(redis_key).query_async(conn).await?;

    // Set the TTL only on the first request of the window (count == 1).
    // Doing this after INCR guarantees the key always gets an expiry, even
    // if a previous window's key expired between two concurrent INCRs.
    if count == 1 {
        // Propagate EXPIRE errors: a missing TTL means the key persists beyond
        // the window, allowing unlimited requests.  The caller's timeout/error
        // handler will fall back to the in-memory limiter if this fails.
        let _: () = redis::cmd("EXPIRE")
            .arg(redis_key)
            .arg(window_secs)
            .query_async(conn)
            .await?;
    }

    Ok(count <= limit.saturating_add(burst))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Redis-dependent tests are gated behind an environment variable so they
    // do not fail in environments without Redis.
    //
    // Run with:
    //   REDIS_URL=redis://127.0.0.1:6379 cargo test -p lopatnov-conduit-ratelimit --features redis
    //
    // The unit tests below exercise only the fallback path (no Redis required).

    use super::*;

    /// Build a `RedisRateLimiter` with a dummy (invalid) connection manager
    /// by pointing to a non-existent server — initial connect will fail.
    ///
    /// We use this to test that `connect()` propagates the error correctly.
    #[tokio::test]
    async fn connect_to_unreachable_redis_returns_error() {
        let result = RedisRateLimiter::connect("redis://127.0.0.1:1").await;
        // Port 1 is reserved / will be refused.
        assert!(result.is_err(), "connection to port 1 must fail");
    }

    // ── fallback_check_impl (issue #317 regression coverage) ────────────

    #[test]
    fn fallback_check_scopes_by_site_label() {
        let fallback: DashMap<String, TokenBucket> = DashMap::new();
        assert!(fallback_check_impl(
            &fallback, "site-a", "1.2.3.4", 100, 0, 60
        ));
        assert!(fallback_check_impl(
            &fallback, "site-b", "1.2.3.4", 100, 0, 60
        ));
        assert_eq!(
            fallback.len(),
            2,
            "two sites sharing a client key must land in two distinct fallback buckets, not one shared bucket (#317)"
        );
    }

    #[test]
    fn fallback_check_exhausts_the_right_sites_bucket_only() {
        let fallback: DashMap<String, TokenBucket> = DashMap::new();
        // Exhaust site-a's limit of 1.
        assert!(fallback_check_impl(
            &fallback, "site-a", "9.9.9.9", 1, 0, 60
        ));
        assert!(
            !fallback_check_impl(&fallback, "site-a", "9.9.9.9", 1, 0, 60),
            "site-a's own bucket must be exhausted after its 1-request limit"
        );
        // Same client key, different site — must have its own untouched budget.
        assert!(
            fallback_check_impl(&fallback, "site-b", "9.9.9.9", 1, 0, 60),
            "site-b must not be affected by site-a's exhausted bucket (#317)"
        );
    }

    // ── burst threading (issue #306 regression coverage) ────────────────

    #[test]
    fn fallback_check_burst_allows_extra_requests_above_limit() {
        let fallback: DashMap<String, TokenBucket> = DashMap::new();
        // limit=1, burst=2 → capacity 3. All 3 should be admitted; the 4th must not.
        assert!(fallback_check_impl(
            &fallback, "site-a", "1.1.1.1", 1, 2, 60
        ));
        assert!(fallback_check_impl(
            &fallback, "site-a", "1.1.1.1", 1, 2, 60
        ));
        assert!(fallback_check_impl(
            &fallback, "site-a", "1.1.1.1", 1, 2, 60
        ));
        assert!(
            !fallback_check_impl(&fallback, "site-a", "1.1.1.1", 1, 2, 60),
            "the 4th request must exceed limit(1) + burst(2) = 3"
        );
    }

    #[test]
    fn fallback_check_zero_burst_matches_pre_306_behavior() {
        let fallback: DashMap<String, TokenBucket> = DashMap::new();
        assert!(fallback_check_impl(
            &fallback, "site-a", "2.2.2.2", 1, 0, 60
        ));
        assert!(
            !fallback_check_impl(&fallback, "site-a", "2.2.2.2", 1, 0, 60),
            "burst=0 must reject the 2nd request against limit=1, exactly like before #306"
        );
    }
}
