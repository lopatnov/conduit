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
//! against key `conduit:rl:{scope_label}\0{window_secs}\0{client_key}`.
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
//!
//! The three dynamic components are joined with `\0` (NUL), not `:` (issue
//! #350) — `scope_label` can legitimately contain colons itself (a
//! `"{host}:{port}"` site label, or `redis_route_scope`'s own
//! `"route\0{site_label}\0{route_key}"`, whose *inner* `site_label` can also
//! be colon-bearing for an unbracketed IPv6 host), and `client_key` can too
//! (an IPv6 client address, or an arbitrary `keyBy: "header:X-Name"` value).
//! With a plain `:` join, two different (scope, client) pairs can produce an
//! identical literal key string when one's characters happen to fall across
//! the other's delimiter boundaries — verified directly: `scope_label =
//! "2001:db8::1:8080"` / `client_key = "60:alice"` and `scope_label =
//! "2001:db8::1:8080:60"` / `client_key = "alice"` (same `window_secs = 60`)
//! both encode to the literal string `"conduit:rl:2001:db8::1:8080:60:60:alice"`
//! under the old `:`-joined format, silently sharing one counter across two
//! distinct sites. `\0` can't appear in `scope_label` (always Conduit-derived
//! from config, never from request data) and is stripped from `client_key`
//! before it ever reaches this module (`rate_limit::extract_client_key`'s
//! `strip_nul`, issue #320) — so the same collision is not reproducible under
//! the `\0` join, matching the already-\0-based in-memory key format above.

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
        let redis_key = build_redis_key(scope_label, window_secs, client_key);
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

/// Build the real-Redis fixed-window counter key, factored out of
/// [`RedisRateLimiter::check`] as a free function so the exact encoding is
/// directly unit-testable (issue #350) without a live Redis connection.
fn build_redis_key(scope_label: &str, window_secs: u64, client_key: &str) -> String {
    format!("conduit:rl:{scope_label}\0{window_secs}\0{client_key}")
}

/// Build the in-process fallback-map key, factored out of
/// [`fallback_check_impl`] as its own free function (issue #384) so the
/// exact encoding is directly unit-testable, mirroring [`build_redis_key`].
///
/// Include limit, burst, and window_secs in the key so that post-reload
/// config changes are picked up immediately rather than reusing a stale
/// bucket. Include scope_label so two independent scopes sharing a client
/// key don't share a bucket here either (issue #317). `\0`-joined, not
/// `:`-joined (issue #350, defense-in-depth companion to the real-Redis key
/// above — and confirmed by issue #384 to be a real, constructible
/// collision, not just theoretical: `security-engineer` reviewing #383
/// independently built one against the *fallback* format specifically, not
/// just the real-Redis one): this single `fallback` map is shared
/// process-wide across every scope (site/route/consumer) whenever Redis is
/// configured but unreachable, each with its own
/// scope_label/limit/burst/window_secs — a colon-bearing scope_label or
/// client_key (unbracketed IPv6, or an arbitrary `keyBy: "header:X-Name"`
/// value) can collide two distinct scopes into one bucket under a plain
/// `:`-join even though `limit`/`burst`/`window_secs` are colon-free
/// (digit-only) themselves — see
/// `fallback_key_disambiguates_a_verified_real_collision` for the exact
/// byte-verified example.
fn build_fallback_key(
    scope_label: &str,
    client_key: &str,
    limit: u64,
    burst: u64,
    window_secs: u64,
) -> String {
    format!("{scope_label}\0{client_key}\0{limit}\0{burst}\0{window_secs}")
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
    let key = build_fallback_key(scope_label, client_key, limit, burst, window_secs);
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

    // ── build_redis_key (issue #350 regression coverage) ────────────────

    #[test]
    fn build_redis_key_disambiguates_a_verified_real_collision() {
        // A byte-exact collision under the old `:`-joined format, verified
        // directly (not the issue's own illustrative example, which turned
        // out not to reproduce exactly): an unbracketed-IPv6-style site
        // label whose host component itself looks like "<ipv6>:<port>",
        // paired with a client key crafted to look like the tail of a
        // *different*, legitimately-configured site's label plus its own
        // client key. Both encode to the identical literal string
        // "conduit:rl:2001:db8::1:8080:60:60:alice" under the old
        // `:`-joined format (window_secs = 60 for both).
        let site_a = build_redis_key("2001:db8::1:8080", 60, "60:alice");
        let site_b = build_redis_key("2001:db8::1:8080:60", 60, "alice");
        assert_ne!(
            site_a, site_b,
            "two distinct (scope, client) pairs must never encode to the same Redis key"
        );
    }

    #[test]
    fn build_redis_key_is_stable_for_identical_inputs() {
        assert_eq!(
            build_redis_key("example.com:8080", 60, "1.2.3.4"),
            build_redis_key("example.com:8080", 60, "1.2.3.4")
        );
    }

    // ── build_fallback_key (issue #384 regression coverage) ─────────────

    #[test]
    fn build_fallback_key_disambiguates_a_verified_real_collision() {
        // A byte-exact collision under the old `:`-joined fallback-map
        // format, independently constructed by `security-engineer`
        // reviewing #383 (not just the real-Redis collision reused
        // verbatim — the fallback format's `client_key` isn't the final
        // segment, so this needed its own construction with matching
        // limit/burst/window_secs appended identically on both sides).
        // Both encode to "2001:db8::1:8080:60:alice:100:0:60" under the
        // old `:`-joined format.
        let site_a = build_fallback_key("2001:db8::1:8080", "60:alice", 100, 0, 60);
        let site_b = build_fallback_key("2001:db8::1:8080:60", "alice", 100, 0, 60);
        assert_ne!(
            site_a, site_b,
            "two distinct (scope, client) pairs must never encode to the same fallback-map key"
        );
    }

    #[test]
    fn build_fallback_key_is_stable_for_identical_inputs() {
        assert_eq!(
            build_fallback_key("example.com:8080", "1.2.3.4", 100, 0, 60),
            build_fallback_key("example.com:8080", "1.2.3.4", 100, 0, 60)
        );
    }

    #[test]
    fn fallback_check_impl_gives_the_two_colliding_scopes_independent_buckets() {
        // Behavioral confirmation, not just a string-equality check: with
        // the fix, the two scope/client pairs that used to collide under
        // the old `:`-joined key genuinely get independent token buckets —
        // exhausting one's single-request limit must not affect the other.
        let fallback: DashMap<String, TokenBucket> = DashMap::new();
        assert!(fallback_check_impl(
            &fallback,
            "2001:db8::1:8080",
            "60:alice",
            1,
            0,
            60
        ));
        // The first bucket (limit 1) is now exhausted; a second real
        // request from the SAME scope+client would be denied.
        assert!(!fallback_check_impl(
            &fallback,
            "2001:db8::1:8080",
            "60:alice",
            1,
            0,
            60
        ));
        // The other, distinct scope+client pair — which used to collide
        // into the very same bucket under the old format — must get its
        // own fresh allowance instead of inheriting the exhausted one.
        assert!(fallback_check_impl(
            &fallback,
            "2001:db8::1:8080:60",
            "alice",
            1,
            0,
            60
        ));
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
