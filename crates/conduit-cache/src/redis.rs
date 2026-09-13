#![cfg(feature = "redis")]
//! Redis-backed proxy cache storage (Phase 3.8).
//!
//! Implements the `pingora_cache::Storage` trait using Redis as the backend.
//! Each cache entry is stored as three Redis hash fields under a single key:
//!
//! ```text
//! HSET conduit:pcache:{key}  m0 <meta_internal>  m1 <meta_header>  b <body>
//! EXPIRE conduit:pcache:{key}  <ttl_secs>
//! ```
//!
//! The TTL is derived from `CacheMeta::fresh_until()` relative to `now()`.
//! On a cache miss the `RedisMissHandler` buffers the body in memory and
//! writes all three fields atomically in `finish()`.
//!
//! # Fail-open
//!
//! Any Redis error during `lookup` or `get_miss_handler` returns `None` /
//! falls through to a normal upstream fetch.  Errors are logged at `WARN`
//! level so they are visible without being fatal.
//!
//! # Connection lifecycle (issue #330)
//!
//! Connections are established **eagerly**, once per distinct URL, by
//! [`connect_and_register`] — called by the root crate during server startup
//! and again on every hot reload (see `src/proxy/cache_redis.rs`). The
//! request path ([`get`]) only ever performs a lookup against the resulting
//! registry and never connects. This used to be lazy (connect-on-first-
//! request, from inside `RequestFilter`), which required spinning up a
//! nested Tokio runtime and blocking on it from a thread that Pingora
//! already runs inside its own runtime — Tokio detects and panics on that
//! unconditionally ("Cannot start a runtime from within a runtime"), on
//! *every* request to a redis-cached route, forever, since the panicking
//! call never populated the registry. A `URL` absent from the registry
//! (startup connect failed, or a reload just introduced it and the connect
//! is still in flight) falls open to an uncached upstream fetch, same as
//! any other Redis error.

use std::any::Any;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_cache::{
    storage::{HandleMiss, HitHandler, MissFinishType, MissHandler, PurgeType, Storage},
    trace::SpanHandle,
    CacheKey, CacheMeta,
};
use pingora_core::Result as PingoraResult;
use pingora_core::{Error, ErrorType};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

use crate::common::{bytes_to_hex, SimpleHitHandler};

// ── Registry of per-URL storage instances ────────────────────────────────────

static REDIS_STORES: OnceLock<DashMap<String, &RedisCacheStorage>> = OnceLock::new();

fn redis_stores() -> &'static DashMap<String, &'static RedisCacheStorage> {
    REDIS_STORES.get_or_init(DashMap::new)
}

/// Return the `'static` Redis storage previously registered for `url` by
/// [`connect_and_register`], or `None` when no connection has been
/// established (yet, or at all).
///
/// **Never connects.** This is called from Pingora's request pipeline,
/// which already runs inside a Tokio runtime — connecting here (the former
/// `get_or_create`) required a nested runtime + `block_on`, which panics
/// unconditionally on every request to a redis-cached route (issue #330).
pub fn get(url: &str) -> Option<&'static RedisCacheStorage> {
    redis_stores().get(url).map(|s| *s)
}

/// Connect to `url` and register the resulting storage in the process-wide
/// registry, so [`get`] can hand it out without ever connecting from a
/// request thread.
///
/// Idempotent: a URL already in the registry returns `true` immediately
/// without reconnecting, so this is cheap to call again on every hot
/// reload.
///
/// Fail-open (issue #330): an invalid URL or unreachable server logs at
/// ERROR and returns `false` — the caller must not treat that as fatal.
/// The server keeps running with caching disabled for that store; `get()`
/// simply returns `None` for it.
pub async fn connect_and_register(url: &str) -> bool {
    if redis_stores().contains_key(url) {
        return true;
    }
    match RedisCacheStorage::connect(url).await {
        Ok(storage) => {
            let leaked: &'static RedisCacheStorage = Box::leak(Box::new(storage));
            redis_stores().insert(url.to_owned(), leaked);
            tracing::info!(url = %redact_url(url), "Redis proxy cache connected");
            true
        }
        Err(e) => {
            tracing::error!(
                url = %redact_url(url),
                "Redis proxy cache connect failed: {e} — caching disabled for this store"
            );
            false
        }
    }
}

/// Redact any embedded credentials (`user:pass@`) from a Redis URL before
/// logging it — `redis://user:pass@host:port` must never appear in logs
/// verbatim. Security review finding on issue #330's fix: this log line
/// used to be unreachable in practice, since the pre-fix connect path
/// panicked before ever getting here — now that it's genuinely live on
/// every startup/reload, the credential-in-URL case matters for real.
fn redact_url(url: &str) -> std::borrow::Cow<'_, str> {
    let Some(scheme_end) = url.find("://") else {
        return std::borrow::Cow::Borrowed(url);
    };
    let authority_start = scheme_end + 3;
    let rest = &url[authority_start..];
    // Bound the search to the authority component only — everything up to
    // the first '/' (or the whole remainder, if there's no path).
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // The LAST '@' within the authority is the userinfo/host separator, not
    // the first — this codebase's `$VAR` secret-interpolation model has no
    // URL-encoding step, so a raw '@' inside a password is realistic (a
    // second review-round finding on PR #331: `find` here previously leaked
    // a fragment of a password containing its own '@').
    let Some(at) = authority.rfind('@') else {
        return std::borrow::Cow::Borrowed(url);
    };
    std::borrow::Cow::Owned(format!(
        "{}***@{}",
        &url[..authority_start],
        &rest[at + 1..]
    ))
}

// ── RedisCacheStorage ─────────────────────────────────────────────────────────

/// Redis-backed proxy cache storage.
pub struct RedisCacheStorage {
    conn: ConnectionManager,
}

impl RedisCacheStorage {
    /// Connect to Redis. Returns a zero-overhead storage instance backed by
    /// a `ConnectionManager` that reconnects automatically on failure.
    ///
    /// Returns `Err` when the URL is invalid or Redis is unreachable so the
    /// caller ([`connect_and_register`]) can disable caching gracefully
    /// rather than crashing.
    async fn connect(url: &str) -> anyhow::Result<Self> {
        // Embed only the redacted form in the error text itself — the
        // caller's log call redacts its own `url` field, but this message's
        // `{e}` interpolation would otherwise carry the raw credentials
        // right back in, one level deeper (issue found reviewing #347/#330).
        let redacted = redact_url(url);
        let client = redis::Client::open(url)
            .map_err(|e| anyhow::anyhow!("invalid Redis URL ({redacted}): {e}"))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| anyhow::anyhow!("cannot connect to Redis ({redacted}): {e}"))?;
        Ok(Self { conn })
    }

    fn redis_key(key: &CacheKey) -> String {
        let compact = key.to_compact();
        format!("conduit:pcache:{}", bytes_to_hex(&compact.primary))
    }

    fn compact_redis_key(key: &pingora_cache::key::CompactCacheKey) -> String {
        format!("conduit:pcache:{}", bytes_to_hex(&key.primary))
    }
}

// ── Storage impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl Storage for RedisCacheStorage {
    async fn lookup(
        &'static self,
        key: &CacheKey,
        _trace: &SpanHandle,
    ) -> PingoraResult<Option<(CacheMeta, HitHandler)>> {
        let redis_key = Self::redis_key(key);
        let mut conn = self.conn.clone();

        let result: redis::RedisResult<(Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)> =
            redis::cmd("HMGET")
                .arg(&redis_key)
                .arg("m0")
                .arg("m1")
                .arg("b")
                .query_async(&mut conn)
                .await;

        match result {
            Ok((Some(m0), Some(m1), Some(body))) => match CacheMeta::deserialize(&m0, &m1) {
                Ok(meta) => {
                    let handler = Box::new(SimpleHitHandler::new(Bytes::from(body))) as HitHandler;
                    Ok(Some((meta, handler)))
                }
                Err(e) => {
                    tracing::warn!(key = %redis_key, "Redis cache meta deserialize error: {e}");
                    Ok(None)
                }
            },
            Ok(_) => Ok(None), // key not found or partial data
            Err(e) => {
                tracing::warn!(key = %redis_key, "Redis cache lookup error: {e}");
                Ok(None) // fail-open
            }
        }
    }

    async fn get_miss_handler(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
        _trace: &SpanHandle,
    ) -> PingoraResult<MissHandler> {
        let ttl = meta
            .fresh_until()
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::from_secs(60))
            .as_secs()
            .max(1);

        let (meta0, meta1) = meta
            .serialize()
            .map_err(|e| Error::because(ErrorType::InternalError, "cache meta serialize", e))?;

        Ok(Box::new(RedisMissHandler {
            redis_key: Self::redis_key(key),
            conn: self.conn.clone(),
            meta0,
            meta1,
            body: Vec::new(),
            ttl,
        }) as MissHandler)
    }

    async fn purge(
        &'static self,
        key: &pingora_cache::key::CompactCacheKey,
        _purge_type: PurgeType,
        _trace: &SpanHandle,
    ) -> PingoraResult<bool> {
        let redis_key = Self::compact_redis_key(key);
        let mut conn = self.conn.clone();
        let removed: u64 = conn.del(&redis_key).await.unwrap_or(0);
        Ok(removed > 0)
    }

    async fn update_meta(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
        _trace: &SpanHandle,
    ) -> PingoraResult<bool> {
        let redis_key = Self::redis_key(key);
        let mut conn = self.conn.clone();

        let (m0, m1) = meta
            .serialize()
            .map_err(|e| Error::because(ErrorType::InternalError, "cache meta serialize", e))?;

        let res: redis::RedisResult<()> = redis::cmd("HMSET")
            .arg(&redis_key)
            .arg("m0")
            .arg(m0)
            .arg("m1")
            .arg(m1)
            .query_async(&mut conn)
            .await;

        match res {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(key = %redis_key, "Redis cache update_meta error: {e}");
                Ok(false)
            }
        }
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync + 'static) {
        self
    }
}

// ── RedisMissHandler ──────────────────────────────────────────────────────────

struct RedisMissHandler {
    redis_key: String,
    conn: ConnectionManager,
    meta0: Vec<u8>,
    meta1: Vec<u8>,
    body: Vec<u8>,
    ttl: u64,
}

#[async_trait]
impl HandleMiss for RedisMissHandler {
    async fn write_body(&mut self, data: Bytes, _eof: bool) -> PingoraResult<()> {
        self.body.extend_from_slice(&data);
        Ok(())
    }

    async fn finish(self: Box<Self>) -> PingoraResult<MissFinishType> {
        let size = self.body.len();
        let mut conn = self.conn.clone();

        // HSET + EXPIRE as a pipeline for atomicity and efficiency.
        let res: redis::RedisResult<()> = redis::pipe()
            .cmd("HSET")
            .arg(&self.redis_key)
            .arg("m0")
            .arg(&self.meta0)
            .arg("m1")
            .arg(&self.meta1)
            .arg("b")
            .arg(&self.body)
            .ignore()
            .cmd("EXPIRE")
            .arg(&self.redis_key)
            .arg(self.ttl)
            .ignore()
            .query_async(&mut conn)
            .await;

        if let Err(e) = res {
            tracing::warn!(key = %self.redis_key, "Redis cache write error: {e}");
        }

        Ok(MissFinishType::Created(size))
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_key_format() {
        let key = CacheKey::new("example.com", "http:/foo", "");
        let rk = RedisCacheStorage::redis_key(&key);
        assert!(rk.starts_with("conduit:pcache:"), "key: {rk}");
    }

    #[test]
    fn redis_key_is_32_hex_chars_after_prefix() {
        let key = CacheKey::new("host.example", "https:/path", "");
        let rk = RedisCacheStorage::redis_key(&key);
        let hex_part = rk.strip_prefix("conduit:pcache:").unwrap();
        assert_eq!(
            hex_part.len(),
            32,
            "expected 32 hex chars, got {hex_part:?}"
        );
        assert!(
            hex_part.chars().all(|c| c.is_ascii_hexdigit()),
            "non-hex chars in key: {hex_part}"
        );
    }

    #[test]
    fn two_different_keys_produce_different_redis_keys() {
        let k1 = CacheKey::new("host1.example", "https:/path1", "");
        let k2 = CacheKey::new("host2.example", "https:/path2", "");
        let rk1 = RedisCacheStorage::redis_key(&k1);
        let rk2 = RedisCacheStorage::redis_key(&k2);
        assert_ne!(rk1, rk2, "different cache keys must not collide");
    }

    #[test]
    fn get_returns_none_for_an_unregistered_url() {
        assert!(get("redis://127.0.0.1:1").is_none());
    }

    // ── connect() error redaction ────────────────────────────────────────────

    #[tokio::test]
    async fn connect_error_does_not_leak_credentials() {
        // Port 1 is a reserved/unlikely-to-be-listening port — the connect
        // attempt fails fast without a live Redis instance. The bug this
        // guards: connect()'s own anyhow! error text used to embed the raw
        // `url` (with credentials) directly, so even though the caller's log
        // call separately redacted its own `url` field, the error's Display
        // output — interpolated via `{e}` — carried the password right back
        // in one level deeper.
        let result = RedisCacheStorage::connect("redis://alice:s3cret@127.0.0.1:1").await;
        let Err(err) = result else {
            panic!("port 1 must not accept a connection");
        };
        let msg = err.to_string();
        assert!(
            !msg.contains("s3cret"),
            "connect() error must not leak the raw password: {msg}"
        );
        assert!(
            !msg.contains("alice:s3cret@"),
            "connect() error must not leak raw userinfo: {msg}"
        );
    }

    #[tokio::test]
    async fn connect_and_register_unreachable_redis_returns_false_and_registers_nothing() {
        // Port 1 is reserved and never listening — connection is refused
        // immediately, no live Redis instance needed. Verifies fail-open:
        // must return false and leave the registry empty, not panic.
        let url = "redis://127.0.0.1:1";
        assert!(
            !connect_and_register(url).await,
            "unreachable Redis must return false, not panic"
        );
        assert!(
            get(url).is_none(),
            "a failed connect must not register anything"
        );
    }

    /// Regression for #330. The pre-fix `get_or_create` built a nested
    /// Tokio runtime and `block_on`'d it; Tokio panics ("Cannot start a
    /// runtime from within a runtime") when that happens on a thread
    /// already inside a runtime — which is every Pingora worker thread,
    /// i.e. every request to a redis-cached route, forever, since the
    /// panicking call never populated the registry.
    ///
    /// Must be `#[tokio::test]`, not `#[test]` — a plain `#[test]` has no
    /// ambient runtime and would not reproduce the panic (this is exactly
    /// why `unreachable_redis_returns_none_not_panic`, the old `#[test]`
    /// version of this test, never caught the bug).
    #[tokio::test]
    async fn lookup_from_within_a_runtime_does_not_panic() {
        assert!(get("redis://127.0.0.1:1").is_none());
    }

    // ── redact_url ────────────────────────────────────────────────────────

    #[test]
    fn redact_url_strips_username_and_password() {
        assert_eq!(
            redact_url("redis://alice:s3cret@example.com:6379"),
            "redis://***@example.com:6379"
        );
    }

    #[test]
    fn redact_url_no_credentials_returned_unchanged() {
        let url = "redis://example.com:6379";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_handles_rediss_scheme() {
        assert_eq!(
            redact_url("rediss://user:pw@secure.example:6380"),
            "rediss://***@secure.example:6380"
        );
    }

    #[test]
    fn redact_url_does_not_treat_an_at_sign_in_a_path_as_credentials() {
        // No '@' before the first '/' after the scheme -- not userinfo.
        let url = "redis://example.com:6379/db@1";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_malformed_url_returned_unchanged() {
        let url = "not-a-url";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_password_containing_at_sign_does_not_leak_a_fragment() {
        // Regression for a second review-round finding on PR #331: this
        // codebase's `$VAR` secret-interpolation model has no URL-encoding
        // step, so a raw '@' inside a password is realistic. The first '@'
        // is part of the password, not the userinfo/host separator -- using
        // the LAST '@' in the authority is the only correct split.
        let redacted = redact_url("redis://user:pa@ss@host:6379");
        assert_eq!(redacted, "redis://***@host:6379");
        assert!(
            !redacted.contains("pa") && !redacted.contains("ss"),
            "no fragment of the password must survive redaction: {redacted}"
        );
    }
}
