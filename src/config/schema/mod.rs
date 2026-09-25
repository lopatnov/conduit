//! Config schema: `AppConfig`, `SiteConfig` and every type they contain.
//!
//! Split by concern into the submodules below (issue #114 / #314) — a pure relocation of what
//! used to be one `schema.rs`. Field order within every struct is the serialisation key order
//! and variant order within every `#[serde(untagged)]` enum is the order serde tries them in,
//! so neither may change. Every item is re-exported here, so every existing
//! `crate::config::schema::X` path keeps resolving.

mod app;
mod auth;
mod content;
mod limits;
mod logging;
mod middleware;
mod proxy;
mod routing;
mod site;
mod static_files;
mod tls;
mod upstream;

#[cfg(test)]
use indexmap::IndexMap;
use std::fmt;

/// Marker printed in place of a secret value in a manual `Debug` impl —
/// distinguishing "present" (`Some([REDACTED])`) from "absent" (`None`)
/// without ever printing the actual value (issue #354: every secret-bearing
/// config field used to derive plain `Debug`, so a `{:?}`-formatted print or
/// a panic message that happened to include one would leak it verbatim).
#[derive(Clone)]
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

// ── Constants ──────────────────────────────────────────────────────────────

pub use conduit_config_core::parse::CONFIG_VERSION;

pub use app::{AdminConfig, AppConfig, ConfigFile, GlobalConfig, OtlpConfig};
pub use auth::{
    ApiKeyConfig, BasicAuthConfig, Consumer, ConsumerBasicAuth, ConsumerJwtConfig, ConsumersConfig,
    ConsumersSharedJwtConfig, ForwardAuthConfig, JwtAuthConfig, RateLimitConfig,
};
pub use content::{
    CompressionConfig, CompressionOptions, CorsConfig, CorsOptions, HeaderTransformConfig,
    HealthCheckConfig, HealthCheckOptions, HotReloadConfig, HotReloadOptions, MetricsConfig,
    ResponseTimeConfig, ResponseTimeOptions, SecurityHeadersConfig, SecurityHeadersOptions,
    UploadConfig,
};
pub use limits::{IpFilterConfig, LimitsConfig};
pub use logging::{LogFormat, LoggingConfig, LoggingOptions};
pub use middleware::{FaultAbort, FaultDelay, FaultInjectionConfig, MiddlewareEntry};
pub use proxy::{
    CacheConfig, ConnectionPoolConfig, ProxyConfig, ProxyRouteConfig, ProxyRouteTarget,
    ProxyTimeout, RetryConfig, RewriteRule, StickyConfig, TcpConfig,
};
pub use routing::{MatchConfig, RedirectRule, RouteConfig};
pub use site::SiteConfig;
pub use static_files::{FallbackConfig, FallbackRule, StaticConfig, StaticOptions};
pub use tls::{AcmeConfig, Http2Config, TlsClientAuth, TlsConfig};
pub use upstream::{
    LoadBalanceStrategy, OutlierDetectionConfig, ProxyTarget, UpstreamGroup, UpstreamHealthCheck,
    UpstreamTlsConfig, WeightedTarget,
};

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn admin_config_token_is_redacted() {
        let cfg = AdminConfig {
            bind: Some("0.0.0.0:2019".to_string()),
            token: Some("super-secret-admin-token".to_string()),
        };
        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("super-secret-admin-token"),
            "token leaked into Debug output: {debug}"
        );
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            debug.contains("0.0.0.0:2019"),
            "non-secret field must still print: {debug}"
        );
    }

    #[test]
    fn admin_config_absent_token_shows_none() {
        let cfg = AdminConfig {
            bind: None,
            token: None,
        };
        let debug = format!("{cfg:?}");
        assert!(
            debug.contains("None"),
            "absent secret must still distinguish None from Some: {debug}"
        );
        assert!(!debug.contains("[REDACTED]"), "got: {debug}");
    }

    // `sticky_config_secret_is_redacted` moved to
    // `crates/conduit-proxy-http/src/config.rs` along with `StickyConfig`
    // itself (issue #114/#143).

    #[test]
    fn basic_auth_config_passwords_are_redacted_but_usernames_are_not() {
        let mut users = IndexMap::new();
        users.insert("alice".to_string(), "alices-plaintext-password".to_string());
        let cfg = BasicAuthConfig {
            users,
            challenge: None,
            realm: None,
            skip_paths: None,
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("alices-plaintext-password"), "got: {debug}");
        assert!(
            debug.contains("alice"),
            "usernames aren't secret and should still print: {debug}"
        );
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
    }

    #[test]
    fn api_key_config_keys_are_redacted() {
        let cfg = ApiKeyConfig {
            keys: vec!["key-one".to_string(), "key-two".to_string()],
            header: Some("x-api-key".to_string()),
            skip_paths: None,
        };
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("key-one"), "got: {debug}");
        assert!(!debug.contains("key-two"), "got: {debug}");
        assert!(
            debug.contains("x-api-key"),
            "non-secret field must still print: {debug}"
        );
    }
}
