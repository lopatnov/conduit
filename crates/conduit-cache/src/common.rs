//! Shared helpers for the disk and Redis proxy-cache `Storage` backends
//! (`cache_disk.rs`, `cache_redis.rs`).

use std::any::Any;

use async_trait::async_trait;
use bytes::Bytes;
use pingora_cache::{
    key::CompactCacheKey,
    storage::{HandleHit, PurgeTarget, Storage},
    trace::SpanHandle,
    CacheKey,
};
use pingora_core::Result as PingoraResult;

/// Encode a 16-byte hash as a 32-character lowercase hex string.
pub(crate) fn bytes_to_hex(b: &[u8; 16]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The logical cache key a purge should remove, or `None` when the target
/// cannot refer to anything these backends hold.
///
/// Both backends store one entry per logical key and hand out no
/// [`CacheEntryId`](pingora_cache::eviction::CacheEntryId) (the default
/// `HandleHit::entry_id`/`HandleMiss::entry_id` return `None`).  An
/// [`PurgeTarget::Exact`] target that names an id therefore identifies an entry
/// generation this storage never issued; deleting "whatever is under that key"
/// for it could remove a newer, unrelated entry, so it is treated as not found.
pub(crate) fn purge_target_key(target: PurgeTarget<'_>) -> Option<&CompactCacheKey> {
    match target {
        PurgeTarget::Active(key) => Some(key),
        PurgeTarget::Exact(entry) => entry.entry_id().is_none().then(|| entry.key()),
    }
}

/// A cache-hit handler that serves a single pre-loaded response body.
///
/// Both the disk and Redis backends read the whole body into memory during
/// `lookup()`, so their hit handlers only ever need to hand that buffer back
/// once — the two were byte-for-byte identical before being merged here.
pub(crate) struct SimpleHitHandler {
    body: Option<Bytes>,
}

impl SimpleHitHandler {
    pub(crate) fn new(body: Bytes) -> Self {
        Self { body: Some(body) }
    }
}

#[async_trait]
impl HandleHit for SimpleHitHandler {
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

#[cfg(test)]
mod tests {
    use super::*;
    use pingora_cache::eviction::{CacheEntryId, CacheEntryKey};

    #[test]
    fn bytes_to_hex_all_zeros() {
        let b = [0u8; 16];
        assert_eq!(bytes_to_hex(&b), "0".repeat(32));
    }

    #[test]
    fn bytes_to_hex_all_ff() {
        let b = [0xFFu8; 16];
        assert_eq!(bytes_to_hex(&b), "ff".repeat(16));
    }

    #[test]
    fn bytes_to_hex_known_value() {
        let b: [u8; 16] = [
            0x00, 0x01, 0x0F, 0x10, 0xAB, 0xCD, 0xEF, 0xFE, 0x80, 0x7F, 0x55, 0xAA, 0x11, 0x22,
            0x33, 0x44,
        ];
        assert_eq!(bytes_to_hex(&b), "00010f10abcdeffe807f55aa11223344");
    }

    #[tokio::test]
    async fn simple_hit_handler_returns_body_once() {
        let mut handler = SimpleHitHandler::new(Bytes::from_static(b"hello"));
        assert_eq!(
            handler.read_body().await.unwrap(),
            Some(Bytes::from_static(b"hello"))
        );
        assert_eq!(handler.read_body().await.unwrap(), None);
    }

    #[tokio::test]
    async fn simple_hit_handler_finish_is_noop_ok() {
        let handler: Box<SimpleHitHandler> =
            Box::new(SimpleHitHandler::new(Bytes::from_static(b"x")));
        let storage = crate::cache::cache_storage() as &'static (dyn Storage + Sync);
        let key = CacheKey::new("host.example\0https:/path", "");
        let span = pingora_cache::trace::Span::inactive();
        assert!(handler.finish(storage, &key, &span.handle()).await.is_ok());
    }

    #[test]
    fn purge_target_key_resolves_active_and_unidentified_exact_targets() {
        let key = CacheKey::new("host.example\0https:/path", "").to_compact();
        assert_eq!(
            purge_target_key(PurgeTarget::Active(&key)).map(|k| k.primary),
            Some(key.primary)
        );

        let unidentified = CacheEntryKey::key_only(key.clone());
        assert_eq!(
            purge_target_key(PurgeTarget::Exact(&unidentified)).map(|k| k.primary),
            Some(key.primary)
        );
    }

    #[test]
    fn purge_target_key_refuses_an_exact_target_with_an_id_it_never_issued() {
        let key = CacheKey::new("host.example\0https:/path", "").to_compact();
        let identified = CacheEntryKey::identified(key, CacheEntryId::new(7));
        assert!(purge_target_key(PurgeTarget::Exact(&identified)).is_none());
    }

    #[test]
    fn simple_hit_handler_as_any_downcasts_to_concrete_type() {
        let mut handler = SimpleHitHandler::new(Bytes::from_static(b"x"));
        assert!(handler
            .as_any()
            .downcast_ref::<SimpleHitHandler>()
            .is_some());
        assert!(handler
            .as_any_mut()
            .downcast_mut::<SimpleHitHandler>()
            .is_some());
    }
}
