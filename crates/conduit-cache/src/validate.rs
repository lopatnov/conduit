//! Config validation for `cache`: called by the proxy route validation.

use conduit_config_core::validation::ValidationError;

/// Validate the `cache` config block on a proxy route.
pub fn validate_cache_config(
    cache: &crate::config::CacheConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let store = &cache.store;
    let valid = store == "memory"
        || store.starts_with("redis://")
        || store.starts_with("rediss://")
        || store.starts_with("disk:");
    if !valid {
        errors.push(ValidationError::new(
            format!("{prefix}.store"),
            format!(
                "invalid store \"{store}\" — must be \"memory\", \
                 a redis:// URL, a rediss:// URL (TLS), or disk:<path>"
            ),
        ));
    }
    if let (Some(swr), Some(ttl)) = (cache.stale_while_revalidate_secs, cache.ttl_secs) {
        if swr as u64 > ttl.saturating_mul(10) {
            // Not a hard error, just a suspicious config.
            tracing::debug!(
                "{prefix}.staleWhileRevalidateSecs ({swr}) is more than 10× ttlSecs ({ttl})"
            );
        }
    }
}
