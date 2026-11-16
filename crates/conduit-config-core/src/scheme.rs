//! URL-scheme predicates for config values, shared by every crate that decides on a `store` string
//! (issue #471: the Redis-scheme check used to be copied into ten places).

/// `true` for a `redis://` or `rediss://` (TLS) URL — how a `rateLimit.store` / `cache.store` value
/// selects the Redis backend. `disk:` paths and `memory` are not URLs.
pub fn is_redis_url(store: &str) -> bool {
    store.starts_with("redis://") || store.starts_with("rediss://")
}

/// The `store` value that selects the Redis backend without naming an address: the address is the
/// operator's, set once per process (`global.redis.url` or `CONDUIT_REDIS_URL`, issue #760).
pub const REDIS_STORE: &str = "redis";

/// The environment variable that carries the operator's Redis address in a process that reads its sites from
/// `ConduitSite` objects (a `ConduitSite` cannot set `global`).
pub const REDIS_URL_ENV: &str = "CONDUIT_REDIS_URL";

/// `true` when a `rateLimit.store` / `cache.store` value selects the Redis backend: the bare `redis` (the
/// operator's address) or a legacy `redis://` / `rediss://` URL.
pub fn is_redis_store(store: &str) -> bool {
    store == REDIS_STORE || is_redis_url(store)
}

/// The operator's Redis address: `global.redis.url` when set, else the value of [`REDIS_URL_ENV`]; blank
/// values (a `Secret` with a trailing newline) count as unset and are trimmed.
pub fn resolve_operator_redis_url(
    global_url: Option<&str>,
    env_url: Option<&str>,
) -> Option<String> {
    [global_url, env_url]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|u| !u.is_empty())
        .map(str::to_owned)
}

/// [`resolve_operator_redis_url`] with the environment read from the process.
pub fn operator_redis_url_from_env(global_url: Option<&str>) -> Option<String> {
    let env = std::env::var(REDIS_URL_ENV).ok();
    resolve_operator_redis_url(global_url, env.as_deref())
}

static OPERATOR_REDIS_URL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Record the operator's Redis address for the process, once, at startup (the connections are made then; a
/// later change takes a restart). Returns `false` when it was already recorded.
pub fn set_operator_redis_url(url: Option<String>) -> bool {
    OPERATOR_REDIS_URL.set(url).is_ok()
}

/// The operator's Redis address recorded at startup, if any.
pub fn operator_redis_url() -> Option<&'static str> {
    OPERATOR_REDIS_URL.get().and_then(Option::as_deref)
}

/// The Redis address a `store` value stands for: itself when it is a URL, the operator's address when it is the
/// bare `redis`, `None` otherwise (or when the operator set none).
pub fn redis_address_for(store: &str) -> Option<&str> {
    if is_redis_url(store) {
        Some(store)
    } else if store == REDIS_STORE {
        operator_redis_url()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::is_redis_url;

    #[test]
    fn the_bare_redis_is_a_redis_store_but_not_a_url() {
        assert!(super::is_redis_store("redis"));
        assert!(super::is_redis_store("rediss://h:1"));
        assert!(!super::is_redis_url("redis"));
        for other in ["memory", "Redis", "redis ", "redis:", "disk:/x", ""] {
            assert!(!super::is_redis_store(other), "{other:?}");
        }
    }

    #[test]
    fn the_operator_url_is_the_file_value_then_the_environment_trimmed() {
        use super::resolve_operator_redis_url as r;
        assert_eq!(
            r(Some("redis://a:1"), Some("redis://b:2")).as_deref(),
            Some("redis://a:1")
        );
        assert_eq!(
            r(None, Some("redis://b:2\n")).as_deref(),
            Some("redis://b:2")
        );
        assert_eq!(
            r(Some("  "), Some("redis://b:2")).as_deref(),
            Some("redis://b:2")
        );
        assert_eq!(r(None, Some("\n")), None);
        assert_eq!(r(None, None), None);
    }

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
