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

use super::cache_common::{bytes_to_hex, SimpleHitHandler};

// ── Registry of per-URL storage instances ────────────────────────────────────

static REDIS_STORES: OnceLock<DashMap<String, &'static RedisCacheStorage>> = OnceLock::new();

fn redis_stores() -> &'static DashMap<String, &'static RedisCacheStorage> {
    REDIS_STORES.get_or_init(DashMap::new)
}

/// Return (or create) the `'static` Redis storage for the given URL.
///
/// Returns `None` when the connection fails so the caller can disable caching
/// gracefully rather than crashing.  The failure is logged at ERROR level.
pub fn get_or_create(url: &str) -> Option<&'static RedisCacheStorage> {
    if let Some(s) = redis_stores().get(url) {
        return Some(*s);
    }
    match RedisCacheStorage::new_blocking(url) {
        Ok(storage) => {
            let leaked = Box::leak(Box::new(storage));
            redis_stores().insert(url.to_owned(), leaked);
            Some(leaked)
        }
        Err(_) => None,
    }
}

// ── RedisCacheStorage ─────────────────────────────────────────────────────────

/// Redis-backed proxy cache storage.
pub struct RedisCacheStorage {
    conn: ConnectionManager,
}

impl RedisCacheStorage {
    /// Connect to Redis synchronously (using a temporary single-thread runtime).
    ///
    /// Returns a zero-overhead storage instance backed by a `ConnectionManager`
    /// that reconnects automatically on failure.
    /// Connect to Redis synchronously.
    ///
    /// Returns `Err` when the URL is invalid or Redis is unreachable so the
    /// caller can disable caching gracefully rather than crashing.
    pub fn new_blocking(url: &str) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("tokio rt for redis cache: {e}"))?;
        match rt.block_on(Self::new(url)) {
            Ok(s) => {
                tracing::info!(url, "Redis proxy cache connected");
                Ok(s)
            }
            Err(e) => {
                tracing::error!(url, "Redis proxy cache connect failed: {e}");
                Err(e)
            }
        }
    }

    async fn new(url: &str) -> anyhow::Result<Self> {
        let client =
            redis::Client::open(url).map_err(|e| anyhow::anyhow!("invalid Redis URL: {e}"))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| anyhow::anyhow!("cannot connect to Redis ({url}): {e}"))?;
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
    fn unreachable_redis_returns_none_not_panic() {
        // Port 1 is reserved and never listening — connection is refused immediately,
        // no Redis instance needed. Verifies fail-open: must return None, not panic.
        let result = get_or_create("redis://127.0.0.1:1");
        assert!(
            result.is_none(),
            "unreachable Redis must return None, not panic"
        );
    }
}
