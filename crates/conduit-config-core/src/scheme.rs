//! URL-scheme predicates for config values, shared by every crate that decides on a `store` string
//! (issue #471: the Redis-scheme check used to be copied into ten places).

/// `true` for a `redis://` or `rediss://` (TLS) URL — how a `rateLimit.store` / `cache.store` value
/// selects the Redis backend. `disk:` paths and `memory` are not URLs.
pub fn is_redis_url(store: &str) -> bool {
    store.starts_with("redis://") || store.starts_with("rediss://")
}

#[cfg(test)]
mod tests {
    use super::is_redis_url;

    #[test]
    fn redis_and_rediss_are_redis_urls() {
        assert!(is_redis_url("redis://127.0.0.1:6379"));
        assert!(is_redis_url("rediss://cache.example:6380/0"));
    }

    #[test]
    fn everything_else_is_not() {
        assert!(!is_redis_url("memory"));
        assert!(!is_redis_url("disk:/var/cache/conduit"));
        assert!(!is_redis_url("http://redis://x"));
        assert!(!is_redis_url("redis:/missing-slash"));
        assert!(!is_redis_url(""));
    }
}
