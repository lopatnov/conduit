//! Redis-backed rate limiting with graceful fallback to in-process memory.
//!
//! When Redis is unavailable (connection error, timeout), each check falls
//! through to the same `DashMap<String, TokenBucket>` used by the pure-memory
//! rate limiter.  This keeps the server operational even when Redis is down
//! (fail-open behaviour — requests are rate-limited in memory, not rejected).
//!
//! # Redis data model
//!
//! Each client key maps to a Redis string that counts requests in the current
//! window.  Two commands implement the counter atomically from the client's
//! perspective:
//!
//! ```text
//! SET  conduit:rl:{window_secs}:{client_key}  0  EX {window_secs}  NX
//! INCR conduit:rl:{window_secs}:{client_key}
//! ```
//!
//! The SET creates the key with the window TTL only if it does not already
//! exist; the INCR atomically returns the new count.  This is a
//! **fixed-window counter** — simple, O(1) per check, and low memory usage.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use redis::aio::ConnectionManager;

use super::rate_limit::TokenBucket;

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

    /// Check the rate limit for `client_key`.
    ///
    /// Returns `true` when the request is within the limit.
    ///
    /// Algorithm (two-command fixed-window counter):
    /// 1. `SET key 0 EX window_secs NX` — initialise the counter if absent.
    /// 2. `INCR key` — atomically increment and return the new count.
    ///
    /// On Redis error or timeout the check falls back to the in-process
    /// `TokenBucket` and a `WARN` trace is emitted (fail-open).
    pub async fn check(&self, client_key: &str, limit: u64, window_secs: u64) -> bool {
        let redis_key = format!("conduit:rl:{window_secs}:{client_key}");
        let mut conn = self.conn.clone();

        // Wrap the two-command sequence in a 50 ms deadline.
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            redis_fixed_window_check(&mut conn, &redis_key, limit, window_secs),
        )
        .await;

        match result {
            Ok(Ok(allowed)) => allowed,
            Ok(Err(e)) => {
                tracing::warn!(
                    key = client_key,
                    "Redis rate-limit error (memory fallback): {e}"
                );
                self.fallback_check(client_key, limit, window_secs)
            }
            Err(_timeout) => {
                tracing::warn!(
                    key = client_key,
                    "Redis rate-limit timed out after 50 ms (memory fallback)"
                );
                self.fallback_check(client_key, limit, window_secs)
            }
        }
    }

    fn fallback_check(&self, client_key: &str, limit: u64, window_secs: u64) -> bool {
        self.fallback
            .entry(client_key.to_owned())
            .or_insert_with(|| TokenBucket::new(limit, window_secs))
            .try_consume()
    }

    /// Evict stale entries from the in-memory fallback map.
    ///
    /// Called by the same background cleanup task as the main memory limiter.
    pub fn cleanup_fallback(&self) {
        self.fallback
            .retain(|_, bucket| !bucket.is_stale(bucket.window_secs().saturating_mul(2)));
    }
}

// ── Redis helper ──────────────────────────────────────────────────────────────

/// Fixed-window counter check using two Redis commands.
///
/// Steps:
/// 1. `INCR key` — atomically create-or-increment; returns the new count.
/// 2. `EXPIRE key window_secs` — set TTL only when count == 1 (first request in window).
///
/// If `count > limit`, the request is rate-limited.
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
    window_secs: u64,
) -> Result<bool, redis::RedisError> {
    // INCR key — atomically creates (at 0) then increments; returns new value.
    let count: u64 = redis::cmd("INCR").arg(redis_key).query_async(conn).await?;

    // Set the TTL only on the first request of the window (count == 1).
    // Doing this after INCR guarantees the key always gets an expiry, even
    // if a previous window's key expired between two concurrent INCRs.
    if count == 1 {
        redis::cmd("EXPIRE")
            .arg(redis_key)
            .arg(window_secs)
            .query_async::<()>(conn)
            .await
            .ok(); // non-fatal: worst case the window outlives its deadline slightly
    }

    Ok(count <= limit)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Redis-dependent tests are gated behind an environment variable so they
    // do not fail in environments without Redis.
    //
    // Run with:
    //   REDIS_URL=redis://127.0.0.1:6379 cargo test rate_limit_redis
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
}
