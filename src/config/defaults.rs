/// Default bind address for the Admin API.
pub const DEFAULT_ADMIN_BIND: &str = "127.0.0.1:2019";

/// Default HTTP path for the health check endpoint.
pub const DEFAULT_HEALTH_PATH: &str = "/__health__";

/// Default HTTP path for the Prometheus metrics endpoint.
pub const DEFAULT_METRICS_PATH: &str = "/__metrics__";

/// Default HTTP path for the browser hot-reload SSE endpoint.
pub const DEFAULT_HOT_RELOAD_PATH: &str = "/__hot-reload__";

/// Graceful shutdown timeout when not configured.
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 30;

/// Token-bucket is the only supported algorithm for now.
pub const DEFAULT_RATE_LIMIT_ALGORITHM: &str = "token-bucket";

/// Rate limit key is client IP by default.
pub const DEFAULT_RATE_LIMIT_KEY_BY: &str = "ip";

/// Header name used for API key authentication.
pub const DEFAULT_API_KEY_HEADER: &str = "X-Api-Key";

/// Basic Auth WWW-Authenticate realm.
pub const DEFAULT_BASIC_AUTH_REALM: &str = "Restricted";

/// Static file handler ignores dot-files by default.
pub const DEFAULT_DOT_FILES: &str = "ignore";

/// Access log format when logging is enabled with `true`.
pub const DEFAULT_LOG_FORMAT: &str = "combined";

/// Cache-Control max-age when not specified in staticOptions.
pub const DEFAULT_STATIC_MAX_AGE: &str = "0";

/// HTTP methods cached by default when proxy cache is enabled.
pub const DEFAULT_CACHE_METHODS: &[&str] = &["GET", "HEAD"];

/// Upstream health check interval when not specified.
pub const DEFAULT_HEALTH_CHECK_INTERVAL_SECS: u64 = 10;

/// Unhealthy threshold: consecutive failures before marking upstream down.
pub const DEFAULT_UNHEALTHY_THRESHOLD: u32 = 3;

/// Healthy threshold: consecutive successes before marking upstream up.
pub const DEFAULT_HEALTHY_THRESHOLD: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_bind_is_loopback() {
        assert!(
            DEFAULT_ADMIN_BIND.starts_with("127.0.0.1"),
            "admin must bind loopback"
        );
        assert!(
            DEFAULT_ADMIN_BIND.contains("2019"),
            "default admin port is 2019"
        );
    }

    #[test]
    fn health_and_metrics_paths_start_with_slash() {
        assert!(DEFAULT_HEALTH_PATH.starts_with('/'));
        assert!(DEFAULT_METRICS_PATH.starts_with('/'));
        assert!(DEFAULT_HOT_RELOAD_PATH.starts_with('/'));
    }

    #[test]
    fn cache_methods_are_idempotent() {
        // Only GET and HEAD are safe to cache by default.
        assert!(DEFAULT_CACHE_METHODS.contains(&"GET"));
        assert!(DEFAULT_CACHE_METHODS.contains(&"HEAD"));
        assert!(!DEFAULT_CACHE_METHODS.contains(&"POST"));
        assert!(!DEFAULT_CACHE_METHODS.contains(&"PUT"));
    }

    #[test]
    fn thresholds_are_sane() {
        // Unhealthy threshold > healthy threshold so recovery is fast.
        assert!(DEFAULT_UNHEALTHY_THRESHOLD > DEFAULT_HEALTHY_THRESHOLD);
        assert!(DEFAULT_SHUTDOWN_TIMEOUT_SECS > 0);
        assert!(DEFAULT_HEALTH_CHECK_INTERVAL_SECS > 0);
    }

    #[test]
    fn rate_limit_defaults_are_non_empty() {
        assert!(!DEFAULT_RATE_LIMIT_ALGORITHM.is_empty());
        assert!(!DEFAULT_RATE_LIMIT_KEY_BY.is_empty());
        assert!(!DEFAULT_API_KEY_HEADER.is_empty());
    }
}
