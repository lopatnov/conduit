//! Config validation for `cache`: called by the proxy route validation.

use conduit_config_core::scheme::is_redis_url;
use conduit_config_core::validation::ValidationError;

/// Validate the `cache` config block on a proxy route.
pub fn validate_cache_config(
    cache: &crate::config::CacheConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    let store = &cache.store;
    let valid = store == "memory" || is_redis_url(store) || store.starts_with("disk:");
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

    // Issue #508 (closed by this fix — the warning path is the alternative
    // scope #508 itself proposed) found that maxSizeMb is parsed and stored
    // but has no enforcement code anywhere — no LRU/eviction/admission policy
    // is implemented, so the cache can grow unbounded past this value. The
    // remaining enforcement work is tracked separately at #520 (still open).
    // Surface it as an advisory warning (not a hard error — the field itself
    // is harmless to leave set) so an operator relying on it for a memory
    // budget isn't silently unprotected.
    if cache.max_size_mb.is_some() {
        errors.push(ValidationError::warning(
            format!("{prefix}.maxSizeMb"),
            "cache.maxSizeMb is configured but not currently enforced — no eviction policy \
             is implemented, so the cache may grow unbounded past this limit. See \
             https://github.com/lopatnov/conduit/issues/520 for status.",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheConfig;

    fn minimal_cache() -> CacheConfig {
        CacheConfig {
            store: "memory".to_owned(),
            max_size_mb: None,
            ttl_secs: None,
            stale_while_revalidate_secs: None,
            stale_if_error_secs: None,
            early_refresh_secs: None,
            vary_headers: None,
            skip_paths: None,
            skip_if_cookie: None,
            methods: None,
        }
    }

    #[test]
    fn warning_for_cache_max_size_mb_unenforced() {
        let cache = CacheConfig {
            max_size_mb: Some(256),
            ..minimal_cache()
        };
        let mut e = Vec::new();
        validate_cache_config(&cache, "sites[0].proxy.cache", &mut e);
        assert!(
            e.iter()
                .any(|x| x.message.contains("maxSizeMb") && x.message.contains("issues/520")),
            "cache.maxSizeMb must warn that it's unenforced: {e:?}"
        );
        assert!(
            e.iter()
                .filter(|x| x.message.contains("maxSizeMb"))
                .all(|x| x.severity == conduit_config_core::validation::Severity::Warning),
            "the maxSizeMb finding must be advisory, not a hard error: {e:?}"
        );
    }

    #[test]
    fn no_warning_for_cache_without_max_size_mb() {
        let mut e = Vec::new();
        validate_cache_config(&minimal_cache(), "sites[0].proxy.cache", &mut e);
        assert!(
            !e.iter().any(|x| x.message.contains("maxSizeMb")),
            "no maxSizeMb warning expected when field is unset: {e:?}"
        );
    }
}
