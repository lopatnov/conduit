//! Config schema: `AppConfig`, `SiteConfig` and every type they contain.
//!
//! The types live in `crates/conduit-config` since issue #222 (split into submodules by concern in
//! #314). This module is the facade that keeps every `crate::config::schema::X` path resolving: an
//! explicit list, not a glob, so the surface the root crate uses is auditable in one place.

pub use conduit_config::schema::{
    AcmeConfig, AdminConfig, ApiKeyConfig, AppConfig, BasicAuthConfig, CacheConfig,
    CompressionConfig, CompressionOptions, ConfigFile, ConnectionPoolConfig, Consumer,
    ConsumerBasicAuth, ConsumerJwtConfig, ConsumersConfig, ConsumersSharedJwtConfig, CorsConfig,
    CorsOptions, FallbackConfig, FallbackRule, FaultAbort, FaultDelay, FaultInjectionConfig,
    ForwardAuthConfig, GlobalConfig, HeaderTransformConfig, HealthCheckConfig, HealthCheckOptions,
    HotReloadConfig, HotReloadOptions, Http2Config, IpFilterConfig, JwtAuthConfig, LimitsConfig,
    LoadBalanceStrategy, LogFormat, LoggingConfig, LoggingOptions, MatchConfig, MetricsConfig,
    MiddlewareEntry, OtlpConfig, OutlierDetectionConfig, ProxyConfig, ProxyRouteConfig,
    ProxyRouteTarget, ProxyTarget, ProxyTimeout, RateLimitConfig, RedirectRule, ResponseTimeConfig,
    ResponseTimeOptions, RetryConfig, RewriteRule, RouteConfig, SecurityHeadersConfig,
    SecurityHeadersOptions, SiteConfig, StaticConfig, StaticOptions, StickyConfig, TcpConfig,
    TlsClientAuth, TlsConfig, UploadConfig, UpstreamGroup, UpstreamHealthCheck, UpstreamTlsConfig,
    WeightedTarget, CONFIG_VERSION,
};
