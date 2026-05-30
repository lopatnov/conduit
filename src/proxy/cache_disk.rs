//! Disk-backed proxy cache storage (Phase 3.8).
//!
//! Implements `pingora_cache::Storage` using the local filesystem.
//!
//! # Layout
//!
//! ```text
//! {dir}/
//!   {hash[0..2]}/
//!     {hash}.cache     — complete entry: header + body
//! ```
//!
//! Cache entries are written atomically: the body is first buffered in memory
//! (by `DiskMissHandler`), then written to `{hash}.tmp` and renamed to
//! `{hash}.cache` on `finish()`.  A failed write (process crash, disk full)
//! leaves behind a `.tmp` file that is silently ignored on lookup.
//!
//! # Binary format (per `.cache` file)
//!
//! ```text
//! [u32 LE: len(meta0)] [u32 LE: len(meta1)] [meta0 bytes] [meta1 bytes] [body bytes]
//! ```
//!
//! `meta0` and `meta1` are the two blobs returned by `CacheMeta::serialize()`.
//! The body immediately follows without a length prefix — everything after
//! `meta0 + meta1` is the response body.

use std::any::Any;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_cache::{
    storage::{HandleHit, HandleMiss, HitHandler, MissFinishType, MissHandler, PurgeType, Storage},
    trace::SpanHandle,
    CacheKey, CacheMeta,
};
use pingora_core::Result as PingoraResult;
use pingora_core::{Error, ErrorType};

// ── Registry of per-path storage instances ───────────────────────────────────

static DISK_STORES: OnceLock<DashMap<String, &'static DiskCacheStorage>> = OnceLock::new();

fn disk_stores() -> &'static DashMap<String, &'static DiskCacheStorage> {
    DISK_STORES.get_or_init(DashMap::new)
}

/// Return (or create) the `'static` disk storage for the given directory path.
///
/// The path is the portion after `"disk:"` in the cache store string.
/// Called at startup.
pub fn get_or_create(dir: &str) -> &'static DiskCacheStorage {
    if let Some(s) = disk_stores().get(dir) {
        return *s;
    }
    let storage = Box::leak(Box::new(DiskCacheStorage::new(dir)));
    disk_stores().insert(dir.to_owned(), storage);
    storage
}

// ── DiskCacheStorage ──────────────────────────────────────────────────────────

/// Disk-backed proxy cache storage.
pub struct DiskCacheStorage {
    dir: PathBuf,
}

impl DiskCacheStorage {
    fn new(dir: &str) -> Self {
        let path = PathBuf::from(dir);
        if let Err(e) = std::fs::create_dir_all(&path) {
            tracing::error!(dir = %path.display(), "Failed to create disk cache directory: {e}");
        } else {
            tracing::info!(dir = %path.display(), "Disk proxy cache initialised");
        }
        Self { dir: path }
    }

    fn entry_path(&self, hash: &str) -> PathBuf {
        let prefix = &hash[..hash.len().min(2)];
        self.dir.join(prefix).join(format!("{hash}.cache"))
    }

    fn tmp_path(&self, hash: &str) -> PathBuf {
        let prefix = &hash[..hash.len().min(2)];
        self.dir.join(prefix).join(format!("{hash}.tmp"))
    }

    fn hash(key: &CacheKey) -> String {
        let compact = key.to_compact();
        bytes_to_hex(&compact.primary)
    }

    fn compact_hash(key: &pingora_cache::key::CompactCacheKey) -> String {
        bytes_to_hex(&key.primary)
    }
}

/// Encode a 16-byte hash as a 32-character lowercase hex string.
fn bytes_to_hex(b: &[u8; 16]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl DiskCacheStorage {
    fn read_entry(path: &Path) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let mut f = std::fs::File::open(path).ok()?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        if buf.len() < 8 {
            return None;
        }
        let m0_len = u32::from_le_bytes(buf[0..4].try_into().ok()?) as usize;
        let m1_len = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
        let header_end = 8 + m0_len + m1_len;
        if buf.len() < header_end {
            return None;
        }
        let m0 = buf[8..8 + m0_len].to_vec();
        let m1 = buf[8 + m0_len..header_end].to_vec();
        let body = buf[header_end..].to_vec();
        Some((m0, m1, body))
    }

    fn write_entry(path: &Path, m0: &[u8], m1: &[u8], body: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(path)?;
        f.write_all(&(m0.len() as u32).to_le_bytes())?;
        f.write_all(&(m1.len() as u32).to_le_bytes())?;
        f.write_all(m0)?;
        f.write_all(m1)?;
        f.write_all(body)?;
        f.flush()
    }
}

// ── Storage impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl Storage for DiskCacheStorage {
    async fn lookup(
        &'static self,
        key: &CacheKey,
        _trace: &SpanHandle,
    ) -> PingoraResult<Option<(CacheMeta, HitHandler)>> {
        let hash = Self::hash(key);
        let path = self.entry_path(&hash);

        // Run blocking file I/O on a dedicated thread.
        let result = tokio::task::spawn_blocking(move || Self::read_entry(&path)).await;

        match result {
            Ok(Some((m0, m1, body))) => match CacheMeta::deserialize(&m0, &m1) {
                Ok(meta) => {
                    // Evict if stale.
                    if meta.fresh_until() <= SystemTime::now() {
                        return Ok(None);
                    }
                    let handler = Box::new(DiskHitHandler {
                        body: Some(Bytes::from(body)),
                    }) as HitHandler;
                    Ok(Some((meta, handler)))
                }
                Err(e) => {
                    tracing::warn!(hash, "Disk cache meta deserialize error: {e}");
                    Ok(None)
                }
            },
            Ok(None) => Ok(None),
            Err(e) => {
                tracing::warn!(hash, "Disk cache lookup spawn error: {e}");
                Ok(None)
            }
        }
    }

    async fn get_miss_handler(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
        _trace: &SpanHandle,
    ) -> PingoraResult<MissHandler> {
        let (m0, m1) = meta
            .serialize()
            .map_err(|e| Error::because(ErrorType::InternalError, "cache meta serialize", e))?;

        Ok(Box::new(DiskMissHandler {
            cache_path: self.entry_path(&Self::hash(key)),
            tmp_path: self.tmp_path(&Self::hash(key)),
            meta0: m0,
            meta1: m1,
            body: Vec::new(),
        }) as MissHandler)
    }

    async fn purge(
        &'static self,
        key: &pingora_cache::key::CompactCacheKey,
        _purge_type: PurgeType,
        _trace: &SpanHandle,
    ) -> PingoraResult<bool> {
        let path = self.entry_path(&Self::compact_hash(key));
        match std::fs::remove_file(&path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => {
                tracing::warn!(path = %path.display(), "Disk cache purge error: {e}");
                Ok(false)
            }
        }
    }

    async fn update_meta(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
        _trace: &SpanHandle,
    ) -> PingoraResult<bool> {
        let path = self.entry_path(&Self::hash(key));

        // Read existing body, rewrite entry with new meta.
        let result = tokio::task::spawn_blocking(move || Self::read_entry(&path)).await;

        match result {
            Ok(Some((_, _, body))) => {
                let (m0, m1) = meta.serialize().map_err(|e| {
                    Error::because(ErrorType::InternalError, "cache meta serialize", e)
                })?;
                let path = self.entry_path(&Self::hash(key));
                tokio::task::spawn_blocking(move || Self::write_entry(&path, &m0, &m1, &body))
                    .await
                    .map_err(|e| Error::explain(ErrorType::InternalError, e.to_string()))?
                    .map_err(|e| Error::explain(ErrorType::InternalError, e.to_string()))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync + 'static) {
        self
    }
}

// ── DiskHitHandler ────────────────────────────────────────────────────────────

struct DiskHitHandler {
    body: Option<Bytes>,
}

#[async_trait]
impl HandleHit for DiskHitHandler {
    async fn read_body(&mut self) -> PingoraResult<Option<Bytes>> {
        Ok(self.body.take())
    }

    async fn finish(
        self: Box<Self>,
        _storage: &'static (dyn Storage + Sync),
        _key: &CacheKey,
        _trace: &SpanHandle,
    ) -> PingoraResult<()> {
        Ok(())
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }

    fn as_any_mut(&mut self) -> &mut (dyn Any + Send + Sync) {
        self
    }
}

// ── DiskMissHandler ───────────────────────────────────────────────────────────

struct DiskMissHandler {
    cache_path: PathBuf,
    tmp_path: PathBuf,
    meta0: Vec<u8>,
    meta1: Vec<u8>,
    body: Vec<u8>,
}

#[async_trait]
impl HandleMiss for DiskMissHandler {
    async fn write_body(&mut self, data: Bytes, _eof: bool) -> PingoraResult<()> {
        self.body.extend_from_slice(&data);
        Ok(())
    }

    async fn finish(self: Box<Self>) -> PingoraResult<MissFinishType> {
        let size = self.body.len();
        let tmp = self.tmp_path.clone();
        let cache = self.cache_path.clone();
        let cache_display = cache.display().to_string();
        let m0 = self.meta0.clone();
        let m1 = self.meta1.clone();
        let body = self.body.clone();

        let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            DiskCacheStorage::write_entry(&tmp, &m0, &m1, &body)?;
            std::fs::rename(&tmp, &cache)
        })
        .await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(path = %cache_display, "Disk cache write error: {e}");
            }
            Err(e) => {
                tracing::warn!(path = %cache_display, "Disk cache write spawn error: {e}");
            }
        }

        Ok(MissFinishType::Created(size))
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn entry_path_two_char_prefix() {
        let dir = TempDir::new().unwrap();
        let storage = DiskCacheStorage::new(dir.path().to_str().unwrap());
        let hash = "abcdef1234";
        let path = storage.entry_path(hash);
        assert!(path.to_string_lossy().contains("ab"));
        assert!(path.to_string_lossy().ends_with("abcdef1234.cache"));
    }

    #[test]
    fn read_write_entry_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cache");
        let m0 = b"meta0".to_vec();
        let m1 = b"meta1".to_vec();
        let body = b"hello world".to_vec();

        DiskCacheStorage::write_entry(&path, &m0, &m1, &body).unwrap();
        let (rm0, rm1, rbody) = DiskCacheStorage::read_entry(&path).unwrap();
        assert_eq!(m0, rm0);
        assert_eq!(m1, rm1);
        assert_eq!(body, rbody);
    }

    #[test]
    fn read_entry_nonexistent_returns_none() {
        assert!(DiskCacheStorage::read_entry(Path::new("/nonexistent/path.cache")).is_none());
    }

    #[test]
    fn read_entry_truncated_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.cache");
        std::fs::write(&path, b"truncated").unwrap();
        assert!(DiskCacheStorage::read_entry(&path).is_none());
    }

    #[test]
    fn read_write_empty_body() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.cache");
        let m0 = b"hdr0".to_vec();
        let m1 = b"hdr1".to_vec();
        let body: Vec<u8> = vec![];

        DiskCacheStorage::write_entry(&path, &m0, &m1, &body).unwrap();
        let (rm0, rm1, rbody) = DiskCacheStorage::read_entry(&path).unwrap();
        assert_eq!(m0, rm0);
        assert_eq!(m1, rm1);
        assert_eq!(body, rbody);
    }

    #[test]
    fn read_write_large_body() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("large.cache");
        let m0 = vec![0xAA; 256];
        let m1 = vec![0xBB; 512];
        let body = vec![0xCC; 65_536]; // 64 KB

        DiskCacheStorage::write_entry(&path, &m0, &m1, &body).unwrap();
        let (rm0, rm1, rbody) = DiskCacheStorage::read_entry(&path).unwrap();
        assert_eq!(m0, rm0);
        assert_eq!(m1, rm1);
        assert_eq!(body, rbody);
    }

    #[test]
    fn entry_path_uses_first_two_chars_as_directory() {
        let dir = TempDir::new().unwrap();
        let storage = DiskCacheStorage::new(dir.path().to_str().unwrap());
        let path = storage.entry_path("deadbeef0123");
        let components: Vec<_> = path.components().collect();
        // The second-to-last component should be the two-char prefix.
        let prefix_component = &components[components.len() - 2];
        assert_eq!(
            prefix_component.as_os_str().to_str().unwrap(),
            "de",
            "expected 'de' prefix dir, got {:?}",
            prefix_component
        );
    }

    #[test]
    fn write_entry_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        // Use a subdirectory that does not exist yet.
        let sub = dir.path().join("newdir").join("entry.cache");
        // write_entry calls create_dir_all internally, so this must succeed
        // even when the parent directory does not pre-exist.
        let result = DiskCacheStorage::write_entry(&sub, b"m0", b"m1", b"body");
        assert!(
            result.is_ok(),
            "write_entry should create missing parent dirs"
        );
        assert!(sub.exists(), "file should exist after write");
    }

    #[test]
    fn read_entry_parses_zero_length_entry() {
        // Write only 8 bytes (two u32 length fields) with no payload.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("header_only.cache");
        // len(m0)=0, len(m1)=0 → only 8 bytes written, body would start at byte 8
        let mut data = vec![];
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        // No meta0, meta1, or body bytes follow.
        std::fs::write(&path, &data).unwrap();
        // m0 and m1 are empty, body is empty — should succeed (valid zero-len entry).
        let result = DiskCacheStorage::read_entry(&path);
        assert!(result.is_some());
        let (m0, m1, body) = result.unwrap();
        assert!(m0.is_empty());
        assert!(m1.is_empty());
        assert!(body.is_empty());
    }

    #[test]
    fn overwrite_entry_reflects_new_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overwrite.cache");

        DiskCacheStorage::write_entry(&path, b"m0_v1", b"m1_v1", b"body_v1").unwrap();
        DiskCacheStorage::write_entry(&path, b"m0_v2", b"m1_v2", b"body_v2").unwrap();

        let (m0, m1, body) = DiskCacheStorage::read_entry(&path).unwrap();
        assert_eq!(m0, b"m0_v2");
        assert_eq!(m1, b"m1_v2");
        assert_eq!(body, b"body_v2");
    }

    #[test]
    fn get_or_create_returns_same_static_reference() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_str().unwrap().to_owned();

        let s1 = get_or_create(&path);
        let s2 = get_or_create(&path);

        // Both references must point to the same allocation.
        assert!(
            std::ptr::eq(s1 as *const _, s2 as *const _),
            "get_or_create should return the same static reference for the same path"
        );
    }
}
