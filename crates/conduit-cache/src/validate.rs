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

    // `maxSizeMb` is the size budget of the store (LRU eviction, #520); the JSON
    // Schema already requires >= 1. A zero budget would admit nothing, so it is a mistake, not "unlimited".
    if cache.max_size_mb == Some(0) {
        errors.push(ValidationError::new(
            format!("{prefix}.maxSizeMb"),
            "maxSizeMb must be at least 1 (omit it for no size limit)",
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

    fn max_size_findings(max_size_mb: Option<u64>) -> Vec<ValidationError> {
        let cache = CacheConfig {
            max_size_mb,
            ..minimal_cache()
        };
        let mut e = Vec::new();
        validate_cache_config(&cache, "sites[0].proxy.cache", &mut e);
        e
    }

    #[test]
    fn zero_max_size_mb_is_a_hard_error() {
        let e = max_size_findings(Some(0));
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].path, "sites[0].proxy.cache.maxSizeMb");
        assert_eq!(
            e[0].severity,
            conduit_config_core::validation::Severity::Error
        );
    }

    #[test]
    fn a_positive_or_absent_max_size_mb_is_clean() {
        assert!(max_size_findings(Some(256)).is_empty());
        assert!(max_size_findings(None).is_empty());
    }
}
