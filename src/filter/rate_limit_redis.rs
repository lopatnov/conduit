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
//! if c == 1 or redis.call('TTL', KEYS[1]) == -1 then
//!     redis.call('EXPIRE', KEYS[1], ARGV[1])
//! end
//! return c
//! ```
//!
//! `INCR` atomically creates (at 0) or increments the key and returns the
//! new count; `EXPIRE` normally runs only when that count is exactly 1 —
//! the first request of a window — so the TTL is set once, not refreshed on
//! every request. It also runs whenever the key has no TTL at all
//! (`TTL == -1`), which self-heals a key left behind by an older version of
//! this code that could leak a TTL-less key on a client-side timeout — see
//! `redis_fixed_window_check`'s own doc comment for the full story. Both
//! commands execute as one atomic operation on the Redis server — a
//! client-side timeout or connection error can never observe `INCR`'s
//! effect without `EXPIRE` also having run.
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
/// 2. `EXPIRE key window_secs` — set TTL when count == 1 (first request in
///    window), **or** when the key has no TTL at all (`TTL` returns `-1`) —
///    see below for why that second case matters.
///
/// If `count > limit`, the request is rate-limited.
///
/// INCR and the conditional EXPIRE run as one atomic server-side operation.
/// An earlier version issued them as two separate round-trips: a client-side
/// timeout (this module wraps the whole check in a 50ms deadline) or a
/// connection error landing between the two commands could leave the key at
/// `count == 1` with **no TTL** — count == 1 was the only case that would
/// ever attempt EXPIRE, so a lost EXPIRE on that specific request meant it
/// never got retried on any later one. That key then persisted forever;
/// once later requests pushed its count past `limit`, that client was
/// rejected *permanently*, not just for the current window — a transient
/// blip degrading into a permanent fail-closed for that one key, silently
/// contradicting this module's whole fail-open design. A Lua script is
/// atomic on the Redis server regardless of what the client observes: a
/// client-side timeout means the script either hasn't started yet or has
/// already run to completion server-side — the client can never observe a
/// state where INCR applied but EXPIRE didn't, so this can no longer happen
/// going forward.
///
/// The `TTL == -1` check exists to *repair* keys already leaked by the old
/// two-round-trip code before this fix was deployed — those keys are still
/// sitting in production Redis with no expiry and `count` already above 1,
/// so the `count == 1` condition alone would never touch them again. The
/// very next request against such a key notices the missing TTL and sets
/// it, self-healing the leak instead of requiring a manual `redis-cli DEL`
/// per affected key (CodeRabbit finding on PR #345).
async fn redis_fixed_window_check(
    conn: &mut ConnectionManager,
    redis_key: &str,
    limit: u64,
    window_secs: u64,
) -> Result<bool, redis::RedisError> {
    const SCRIPT: &str = r#"
        local c = redis.call('INCR', KEYS[1])
        if c == 1 or redis.call('TTL', KEYS[1]) == -1 then
            redis.call('EXPIRE', KEYS[1], ARGV[1])
        end
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

    /// Regression test for the CodeRabbit finding on PR #345: a key already
    /// leaked by the pre-fix two-round-trip code (count > 1, no TTL) must be
    /// repaired the next time it's checked, not left to persist forever.
    ///
    /// Requires a real Redis at `REDIS_URL` — skips (not fails) when unset,
    /// per this module's own documented convention.
    #[tokio::test]
    async fn repairs_legacy_ttl_less_key_on_next_check() {
        let Ok(url) = std::env::var("REDIS_URL") else {
            eprintln!("skipping repairs_legacy_ttl_less_key_on_next_check: REDIS_URL not set");
            return;
        };
        let client = redis::Client::open(url.as_str()).expect("valid REDIS_URL");
        let mut conn = ConnectionManager::new(client)
            .await
            .expect("connect to REDIS_URL");

        let key = format!(
            "conduit:rl:test-legacy-ttl-leak:{}",
            std::process::id() // cheap uniqueness across parallel test runs
        );
        // Clean slate, then simulate exactly what the old buggy code could
        // leave behind: a key already past count == 1, with no TTL at all
        // (as if EXPIRE had been lost on the very first request).
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(3) // count == 3, i.e. well past the count==1 case
            .query_async(&mut conn)
            .await
            .unwrap();
        let ttl_before: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(ttl_before, -1, "test setup: key must start with no TTL");

        // A single check against this pre-leaked key must both increment it
        // and, despite count now being 4 (not 1), notice the missing TTL and
        // repair it.
        let window_secs = 60;
        let allowed = redis_fixed_window_check(&mut conn, &key, 100, window_secs)
            .await
            .expect("check succeeds");
        assert!(allowed, "count 4 is well within limit 100");

        let ttl_after: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!(
            ttl_after > 0 && ttl_after <= window_secs as i64,
            "leaked key must be repaired with a TTL in (0, {window_secs}], got {ttl_after}"
        );

        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
    }
}
