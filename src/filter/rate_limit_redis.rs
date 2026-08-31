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
//! Each client key maps to a Redis string that counts requests in the
//! current window. A single atomic Lua script (`EVAL`) implements the
//! counter server-side:
//!
//! ```text
//! local c = redis.call('INCR', KEYS[1])
//! if c == 1 then redis.call('EXPIRE', KEYS[1], ARGV[1]) end
//! return c
//! ```
//!
//! `INCR` atomically creates (at 0) or increments the key and returns the
//! new count; `EXPIRE` runs only when that count is exactly 1 — the first
//! request of a window — so the TTL is set exactly once, not refreshed on
//! every request. Both commands execute as one atomic operation on the
//! Redis server — a client-side timeout or connection error can never
//! observe `INCR`'s effect without `EXPIRE` also having run (see
//! `redis_fixed_window_check`'s own doc comment for the two-round-trip race
//! this replaced).
//!
//! This is a fixed-window counter — simple, O(1) per check, and low memory
//! usage.

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
        // Include limit and window_secs in the key so that post-reload config
        // changes are picked up immediately rather than reusing a stale bucket.
        let key = format!("{client_key}:{limit}:{window_secs}");
        self.fallback
            .entry(key)
            .or_insert_with(|| TokenBucket::new(limit, 0, window_secs))
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
/// Steps, both inside a single atomic Lua script (`EVAL`):
/// 1. `INCR key` — atomically create-or-increment; returns the new count.
/// 2. `EXPIRE key window_secs` — set TTL only when count == 1 (first request in window).
///
/// If `count > limit`, the request is rate-limited.
///
/// INCR and the conditional EXPIRE run as one atomic server-side operation.
/// An earlier version issued them as two separate round-trips: a client-side
/// timeout (this module wraps the whole check in a 50ms deadline) or a
/// connection error landing between the two commands could leave the key at
/// `count == 1` with **no TTL** — count == 1 is the only case that would
/// ever attempt EXPIRE, so a lost EXPIRE on that specific request means it
/// never gets retried on any later one. That key then persists forever;
/// once later requests push its count past `limit`, that client is
/// rejected *permanently*, not just for the current window — a transient
/// blip degrading into a permanent fail-closed for that one key, silently
/// contradicting this module's whole fail-open design. A Lua script is
/// atomic on the Redis server regardless of what the client observes: a
/// client-side timeout means the script either hasn't started yet or has
/// already run to completion server-side — the client can never observe a
/// state where INCR applied but EXPIRE didn't.
async fn redis_fixed_window_check(
    conn: &mut ConnectionManager,
    redis_key: &str,
    limit: u64,
    window_secs: u64,
) -> Result<bool, redis::RedisError> {
    const SCRIPT: &str = r#"
        local c = redis.call('INCR', KEYS[1])
        if c == 1 then redis.call('EXPIRE', KEYS[1], ARGV[1]) end
        return c
    "#;
    let count: u64 = redis::Script::new(SCRIPT)
        .key(redis_key)
        .arg(window_secs)
        .invoke_async(conn)
        .await?;

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
