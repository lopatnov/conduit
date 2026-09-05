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
//! Each `(scope, client)` pair maps to a Redis string that counts requests in
//! the current window. A single atomic Lua script (`EVAL`) implements the
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
//! against key `conduit:rl:{scope_label}:{window_secs}:{client_key}`.
//! `scope_label` scopes the key so two independent rate-limit scopes sharing
//! a client key don't share a counter — the Redis-backend twin of the fix
//! `rate_limit::site_key`/`route_key`/`consumer_key` applied to the in-memory
//! limiter (issue #317, mirroring #303/#304). Originally site-only (hence the
//! parameter's earlier name, `site_label`); extended to route and consumer
//! scopes too (issue #322) — callers pass `site_label` unchanged for the
//! site-level check, `"route\0{site_label}\0{route_key}"` for per-route, and
//! the fixed literal `"consumer"` (with the consumer's username carried in
//! `client_key` instead, since a consumer's quota is intentionally global,
//! not site-scoped — see `rate_limit::consumer_key`'s own doc) for
//! per-consumer. `EXPIRE` normally runs only on the first request of a window
//! (count == 1), and also whenever the key has no TTL at all (self-healing
//! a key leaked by an older two-round-trip version of this code) — see
//! `redis_fixed_window_check`'s own doc comment for the full story.

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

    /// Check the rate limit for `client_key` within the scope identified by
    /// `scope_label` — a site label, a `"route\0{site}\0{route}"` tag, or the
    /// literal `"consumer"` (see this module's doc comment for the full
    /// per-layer convention).
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
        scope_label: &str,
        client_key: &str,
        limit: u64,
        burst: u64,
        window_secs: u64,
    ) -> bool {
        let redis_key = format!("conduit:rl:{scope_label}:{window_secs}:{client_key}");
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
                    scope = scope_label,
                    key_len = client_key.len(),
                    "Redis rate-limit error (memory fallback): {e}"
                );
                self.fallback_check(scope_label, client_key, limit, burst, window_secs)
            }
            Err(_timeout) => {
                tracing::warn!(
                    scope = scope_label,
                    key_len = client_key.len(),
                    "Redis rate-limit timed out after 50 ms (memory fallback)"
                );
                self.fallback_check(scope_label, client_key, limit, burst, window_secs)
            }
        }
    }

    fn fallback_check(
        &self,
        scope_label: &str,
        client_key: &str,
        limit: u64,
        burst: u64,
        window_secs: u64,
    ) -> bool {
        fallback_check_impl(
            &self.fallback,
            scope_label,
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
    scope_label: &str,
    client_key: &str,
    limit: u64,
    burst: u64,
    window_secs: u64,
) -> bool {
    // Include limit, burst, and window_secs in the key so that post-reload
    // config changes are picked up immediately rather than reusing a stale
    // bucket. Include scope_label so two independent scopes sharing a client key don't
    // share a bucket here either (issue #317).
    let key = format!("{scope_label}:{client_key}:{limit}:{burst}:{window_secs}");
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

/// Fixed-window counter check using a single atomic Lua script.
///
/// Steps, both inside one `EVAL`:
/// 1. `INCR key` — atomically create-or-increment; returns the new count.
/// 2. `EXPIRE key window_secs` — set TTL when count == 1 (first request in
///    window), **or** when the key has no TTL at all (`TTL` returns `-1`).
///
/// If `count > limit + burst`, the request is rate-limited (issue #306 —
/// `burst = 0` reproduces the original `count > limit` behavior exactly).
///
/// INCR and the conditional EXPIRE run as one atomic server-side operation
/// (ported from `main`'s #345 fix during the migration branch's sync with
/// `main`). An earlier version issued them as two separate round-trips: a
/// client-side timeout (this module wraps the whole check in a 50ms
/// deadline) or a connection error landing between the two commands could
/// leave the key at `count == 1` with **no TTL** — count == 1 was the only
/// case that would ever attempt EXPIRE, so a lost EXPIRE on that specific
/// request meant it never got retried on any later one. That key then
/// persisted forever; once later requests pushed its count past
/// `limit + burst`, that client was rejected *permanently*, not just for
/// the current window — a transient blip degrading into a permanent
/// fail-closed for that one key, silently contradicting this module's whole
/// fail-open design. A Lua script is atomic on the Redis server regardless
/// of what the client observes: a client-side timeout means the script
/// either hasn't started yet or has already run to completion server-side
/// — the client can never observe a state where INCR applied but EXPIRE
/// didn't, so this can no longer happen going forward.
///
/// The `TTL == -1` check additionally *repairs* keys already leaked by the
/// old two-round-trip code before this fix was deployed — those keys sit in
/// production Redis with no expiry and `count` already above 1, so the
/// `count == 1` condition alone would never touch them again. The very next
/// request against such a key notices the missing TTL and sets it,
/// self-healing the leak instead of requiring a manual `redis-cli DEL` per
/// affected key.
async fn redis_fixed_window_check(
    conn: &mut ConnectionManager,
    redis_key: &str,
    limit: u64,
    burst: u64,
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
    fn fallback_check_scopes_by_scope_label() {
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

    /// Ported from `main`'s #345 fix: a key already leaked by the pre-atomic-
    /// script code (count > 1, no TTL) must be repaired the next time it's
    /// checked, not left to persist forever.
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
        // leave behind: a key already past count == 1, with no TTL at all.
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(3) // count == 3, well past the count==1 case
            .query_async(&mut conn)
            .await
            .unwrap();
        let ttl_before: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(ttl_before, -1, "test setup: key must start with no TTL");

        let window_secs = 60;
        let allowed = redis_fixed_window_check(&mut conn, &key, 100, 0, window_secs)
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
